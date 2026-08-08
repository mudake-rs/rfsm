use std::error::Error;

use fsm::{Machine, MachineFailure, Reaction, RejectReason, StateKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChargeId(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Draft,
    Payment,
    Authorizing,
    Authorized { charge_id: ChargeId },
    Cancelled,
    Completed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Event {
    BeginPayment,
    Authorize { charge_id: ChargeId },
    Cancel,
    Complete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Transition {
    BeganPayment,
    AuthorizedPayment,
    CancelledPayment,
    CompletedOrder,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Effect {
    RecordCharge(ChargeId),
    RefundCharge(ChargeId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Rejection {
    AlreadyAuthorized,
    CancellationBlocked,
}

struct Facts {
    may_cancel: bool,
}

struct Orders {
    initial: State,
}

impl Machine for Orders {
    type State = State;
    type Event = Event;
    type Context = Facts;
    type Transition = Transition;
    type Effect = Effect;
    type Rejection = Rejection;

    fn initial_target(&self) -> State {
        self.initial
    }

    fn parent(&self, state: &State) -> Option<State> {
        match state {
            State::Authorizing | State::Authorized { .. } => Some(State::Payment),
            _ => None,
        }
    }

    fn state_kind(&self, state: &State) -> StateKind<State> {
        match state {
            State::Payment => StateKind::Compound(State::Authorizing),
            _ => StateKind::Leaf,
        }
    }

    fn react(
        &self,
        active: &State,
        at: &State,
        event: &Event,
        facts: &Facts,
    ) -> Reaction<State, Transition, Effect, Rejection> {
        match (at, event) {
            (State::Draft, Event::BeginPayment) => {
                Reaction::transition(Transition::BeganPayment, State::Payment)
            }
            (State::Authorizing, Event::Authorize { charge_id }) => Reaction::transition_with(
                Transition::AuthorizedPayment,
                State::Authorized {
                    charge_id: *charge_id,
                },
                Effect::RecordCharge(*charge_id),
            ),
            (State::Authorized { .. }, Event::Authorize { .. }) => {
                Reaction::Reject(Rejection::AlreadyAuthorized)
            }
            (State::Payment, Event::Cancel) if facts.may_cancel => match active {
                State::Authorized { charge_id } => Reaction::transition_with(
                    Transition::CancelledPayment,
                    State::Cancelled,
                    Effect::RefundCharge(*charge_id),
                ),
                _ => Reaction::transition(Transition::CancelledPayment, State::Cancelled),
            },
            (State::Payment, Event::Cancel) => Reaction::Reject(Rejection::CancellationBlocked),
            (State::Authorized { .. }, Event::Complete) => {
                Reaction::transition(Transition::CompletedOrder, State::Completed)
            }
            _ => Reaction::Bubble,
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let orders = Orders {
        initial: State::Draft,
    };
    let facts = Facts { may_cancel: true };
    let charge_id = ChargeId(7);

    let payment_session = Orders {
        initial: State::Payment,
    };
    assert_eq!(payment_session.initial()?, State::Authorizing);

    let mut state = orders.initial()?;
    orders.dispatch(&mut state, &Event::BeginPayment, &facts)?;
    assert_eq!(state, State::Authorizing);
    assert!(orders.is_in(&state, &State::Payment)?);

    let authorized = orders.dispatch(&mut state, &Event::Authorize { charge_id }, &facts)?;
    assert_eq!(authorized.effect(), Some(&Effect::RecordCharge(charge_id)));

    let cancelled = orders.dispatch(&mut state, &Event::Cancel, &facts)?;
    assert_eq!(
        (
            cancelled.transition(),
            cancelled.from(),
            cancelled.to(),
            cancelled.effect(),
        ),
        (
            &Transition::CancelledPayment,
            &State::Authorized { charge_id },
            &State::Cancelled,
            Some(&Effect::RefundCharge(charge_id)),
        ),
    );

    let blocked = Facts { may_cancel: false };
    let mut state = State::Authorizing;
    let rejected = orders.dispatch(&mut state, &Event::Cancel, &blocked);
    assert!(matches!(
        rejected,
        Err(MachineFailure::Rejected(RejectReason::Refused(
            Rejection::CancellationBlocked,
        )))
    ));
    assert_eq!(state, State::Authorizing);

    let mut completed_state = State::Authorized { charge_id };
    orders.dispatch(&mut completed_state, &Event::Complete, &facts)?;
    assert_eq!(completed_state, State::Completed);

    println!("cancelled={cancelled:?}, completed_state={completed_state:?}");
    Ok(())
}
