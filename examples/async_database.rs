use std::error::Error;
use std::fmt::{self, Display, Formatter};

use fsm::{Applied, machine};

#[derive(Clone, Debug, Eq, PartialEq)]
enum Effect {
    WriteAudit { actor: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Rejection {
    NotAllowed,
    AlreadyApproved,
}

struct Facts {
    may_approve: bool,
    actor: String,
}

machine! {
    name: Approvals,
    context: Facts,
    effect: Effect,
    rejection: Rejection,

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
    effects: Vec<Effect>,
}

#[derive(Default)]
struct Transaction {
    state: Option<State>,
    effects: Vec<Effect>,
}

#[derive(Debug)]
struct DatabaseError;

impl Display for DatabaseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("database operation failed")
    }
}

impl Error for DatabaseError {}

impl Transaction {
    async fn begin() -> Result<Self, DatabaseError> {
        Ok(Self::default())
    }

    async fn store_state(&mut self, state: &State) -> Result<(), DatabaseError> {
        self.state = Some(state.clone());
        Ok(())
    }

    async fn apply_effect(&mut self, effect: &Effect) -> Result<(), DatabaseError> {
        self.effects.push(effect.clone());
        Ok(())
    }

    async fn commit(self, row: &mut Row) -> Result<(), DatabaseError> {
        if let Some(state) = self.state {
            row.state = state;
        }
        row.effects.extend(self.effects);
        Ok(())
    }
}

async fn approve(
    row: &mut Row,
    facts: &Facts,
    reference: String,
) -> Result<Applied<State, Transition, Effect>, Box<dyn Error>> {
    let event = Event::Approve { reference };
    let plan = Approvals::evaluate(&row.state, &event, facts).await?;

    let mut transaction = Transaction::begin().await?;
    transaction.store_state(&plan.to).await?;
    if let Some(effect) = &plan.effect {
        transaction.apply_effect(effect).await?;
    }
    transaction.commit(row).await?;

    Ok(plan.confirm())
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
    fn transaction_publishes_state_and_effect_before_confirmation() {
        let mut row = Row {
            state: State::Pending,
            effects: Vec::new(),
        };
        let facts = Facts {
            may_approve: true,
            actor: "operator-7".to_owned(),
        };

        let applied = run_ready(approve(&mut row, &facts, "approval-42".to_owned()))
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
        assert_eq!(applied.to, row.state);
        assert_eq!(applied.effect.as_ref(), row.effects.first());
    }
}
