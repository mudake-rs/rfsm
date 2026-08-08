use std::error::Error;
use std::fmt::{self, Display, Formatter};

use fsm::{Committed, Machine, Reaction};

#[derive(Clone, Debug, Eq, PartialEq)]
enum State {
    Pending,
    Approved { reference: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Event {
    Approve { reference: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Transition {
    Approved,
}

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

struct Approvals;

impl Machine for Approvals {
    type State = State;
    type Event = Event;
    type Context = Facts;
    type Transition = Transition;
    type Effect = Effect;
    type Rejection = Rejection;

    fn initial_target(&self) -> State {
        State::Pending
    }

    fn react(
        &self,
        _active: &State,
        at: &State,
        event: &Event,
        facts: &Facts,
    ) -> Reaction<State, Transition, Effect, Rejection> {
        match (at, event) {
            (State::Pending, Event::Approve { reference }) if facts.may_approve => {
                Reaction::transition_with(
                    Transition::Approved,
                    State::Approved {
                        reference: reference.clone(),
                    },
                    Effect::WriteAudit {
                        actor: facts.actor.clone(),
                    },
                )
            }
            (State::Pending, Event::Approve { .. }) => Reaction::Reject(Rejection::NotAllowed),
            (State::Approved { .. }, Event::Approve { .. }) => {
                Reaction::Reject(Rejection::AlreadyApproved)
            }
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
    machine: &Approvals,
    row: &mut Row,
    facts: &Facts,
    reference: String,
) -> Result<Committed<State, Transition, Effect>, Box<dyn Error>> {
    // The database row remains authoritative while selection stays pure.
    let event = Event::Approve { reference };
    let plan = machine.plan_from(&row.state, &event, facts)?;

    let mut transaction = Transaction::begin().await?;
    transaction.store_state(plan.to()).await?;
    if let Some(effect) = plan.effect() {
        transaction.apply_effect(effect).await?;
    }
    transaction.commit(row).await?;

    // The library cannot observe the transaction; this is the caller's claim.
    Ok(plan.committed_by_caller())
}

fn main() {
    // An application supplies its executor. Referencing the handler keeps this
    // example compile-checked without selecting one for the library.
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

        let committed = run_ready(approve(
            &Approvals,
            &mut row,
            &facts,
            "approval-42".to_owned(),
        ))
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
        assert_eq!(committed.to(), &row.state);
        assert_eq!(committed.effect(), row.effects.first());
    }
}
