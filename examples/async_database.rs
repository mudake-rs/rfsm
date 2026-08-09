use std::error::Error;
use std::fmt::{self, Display, Formatter};

use rfsm::{Applied, machine};

#[derive(Clone, Debug, Eq, PartialEq)]
enum Effect {
    WriteAudit { actor: String },
}

struct Facts {
    may_approve: bool,
    actor: String,
}

machine! {
    name: Approvals,
    context: Facts,
    effect: Effect,

    states: {
        *Pending,
        Approved { reference: String },
    },
    events: {
        Approve { reference: String },
    },

    transitions: {
        Approved: Pending + Approve { reference } [async allowed(reference)]
            / audit(reference) => Approved { reference },
        Pending + Approve { .. } => reject NotAllowed,
        Approved { .. } + Approve { .. } => reject AlreadyApproved,
    }
}

impl ApprovalsContext for Facts {
    async fn allowed(&self, _reference: &String) -> bool {
        self.may_approve
    }

    fn audit(&self, _reference: &String) -> Effect {
        Effect::WriteAudit {
            actor: self.actor.clone(),
        }
    }
}

struct Row {
    state: State,
    version: u64,
    effects: Vec<Effect>,
}

struct Snapshot {
    state: State,
    version: u64,
}

impl From<&Row> for Snapshot {
    fn from(row: &Row) -> Self {
        Self {
            state: row.state.clone(),
            version: row.version,
        }
    }
}

struct Transaction<'a> {
    row: &'a mut Row,
    state: Option<State>,
    effects: Vec<Effect>,
}

#[derive(Debug, Eq, PartialEq)]
enum DatabaseError {
    VersionConflict,
}

impl Display for DatabaseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::VersionConflict => formatter.write_str("database version conflict"),
        }
    }
}

impl Error for DatabaseError {}

impl<'a> Transaction<'a> {
    async fn begin(row: &'a mut Row) -> Result<Self, DatabaseError> {
        Ok(Self {
            row,
            state: None,
            effects: Vec::new(),
        })
    }

    async fn store_state_if_version(
        &mut self,
        expected_version: u64,
        state: &State,
    ) -> Result<(), DatabaseError> {
        if self.row.version != expected_version {
            return Err(DatabaseError::VersionConflict);
        }
        self.state = Some(state.clone());
        Ok(())
    }

    async fn apply_effect(&mut self, effect: &Effect) -> Result<(), DatabaseError> {
        self.effects.push(effect.clone());
        Ok(())
    }

    async fn commit(self) -> Result<(), DatabaseError> {
        if let Some(state) = self.state {
            self.row.state = state;
        }
        self.row.effects.extend(self.effects);
        self.row.version += 1;
        Ok(())
    }
}

async fn approve(
    row: &mut Row,
    snapshot: Snapshot,
    facts: Facts,
    reference: String,
) -> Result<Applied<State, Transition, Effect>, Box<dyn Error>> {
    let mut machine = Approvals::from_state(snapshot.state, facts);
    let applied = machine.process(Event::Approve { reference }).await?;

    let mut transaction = Transaction::begin(row).await?;
    transaction
        .store_state_if_version(snapshot.version, machine.state())
        .await?;
    if let Some(effect) = &applied.effect {
        transaction.apply_effect(effect).await?;
    }
    transaction.commit().await?;

    Ok(applied)
}

fn main() {
    // The application supplies its executor; the library has no runtime
    // dependency.
    let _ = approve;
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    use super::*;

    fn run_ready<F: Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut context = Context::from_waker(Waker::noop());

        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("test transaction unexpectedly suspended"),
        }
    }

    #[test]
    fn transaction_commits_state_and_effect_before_returning_applied() {
        let mut row = Row {
            state: State::Pending,
            version: 0,
            effects: Vec::new(),
        };
        let snapshot = Snapshot::from(&row);
        let facts = Facts {
            may_approve: true,
            actor: "operator-7".to_owned(),
        };

        let applied = run_ready(approve(&mut row, snapshot, facts, "approval-42".to_owned()))
            .unwrap_or_else(|failure| panic!("unexpected approval failure: {failure}"));

        assert_eq!(
            row.state,
            State::Approved {
                reference: "approval-42".to_owned(),
            }
        );
        assert_eq!(
            row.effects,
            vec![Effect::WriteAudit {
                actor: "operator-7".to_owned(),
            }]
        );
        assert_eq!(row.version, 1);
        assert_eq!(applied.to, row.state);
        assert_eq!(applied.effect.as_ref(), row.effects.first());
    }

    #[test]
    fn stale_version_preserves_durable_state_and_effects() {
        let mut row = Row {
            state: State::Pending,
            version: 0,
            effects: Vec::new(),
        };
        let stale = Snapshot::from(&row);
        row.version = 1;

        let failure = run_ready(approve(
            &mut row,
            stale,
            Facts {
                may_approve: true,
                actor: "operator-7".to_owned(),
            },
            "approval-42".to_owned(),
        ))
        .err()
        .unwrap_or_else(|| panic!("expected version conflict"));

        assert_eq!(failure.to_string(), "database version conflict");
        assert_eq!(row.state, State::Pending);
        assert_eq!(row.version, 1);
        assert!(row.effects.is_empty());
    }

    #[test]
    fn dropping_staged_transaction_preserves_durable_state_and_effects() {
        let mut row = Row {
            state: State::Pending,
            version: 0,
            effects: Vec::new(),
        };
        let snapshot = Snapshot::from(&row);
        let mut machine = Approvals::from_state(
            snapshot.state,
            Facts {
                may_approve: true,
                actor: "operator-7".to_owned(),
            },
        );
        let applied = run_ready(machine.process(Event::Approve {
            reference: "approval-42".to_owned(),
        }))
        .unwrap_or_else(|failure| panic!("unexpected approval failure: {failure}"));

        let mut transaction = run_ready(Transaction::begin(&mut row))
            .unwrap_or_else(|failure| panic!("unexpected transaction failure: {failure}"));
        run_ready(transaction.store_state_if_version(snapshot.version, machine.state()))
            .unwrap_or_else(|failure| panic!("unexpected state write failure: {failure}"));
        if let Some(effect) = &applied.effect {
            run_ready(transaction.apply_effect(effect))
                .unwrap_or_else(|failure| panic!("unexpected effect write failure: {failure}"));
        }
        drop(transaction);

        assert_eq!(row.state, State::Pending);
        assert_eq!(row.version, 0);
        assert!(row.effects.is_empty());
    }
}
