use std::convert::Infallible;
use std::error::Error;

use fsm::{Machine, MachineFailure, Reaction, RejectReason};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Closed,
    Open,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Event {
    Open,
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Transition {
    Opened,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Rejection {
    AlreadyOpen,
    AlreadyClosed,
}

struct Door;

impl Machine for Door {
    type State = State;
    type Event = Event;
    type Context = ();
    type Transition = Transition;
    type Effect = Infallible;
    type Rejection = Rejection;

    fn initial_target(&self) -> State {
        State::Closed
    }

    fn react(
        &self,
        _active: &State,
        at: &State,
        event: &Event,
        _context: &(),
    ) -> Reaction<State, Transition, Infallible, Rejection> {
        match (at, event) {
            (State::Closed, Event::Open) => Reaction::transition(Transition::Opened, State::Open),
            (State::Open, Event::Close) => Reaction::transition(Transition::Closed, State::Closed),
            (State::Open, Event::Open) => Reaction::Reject(Rejection::AlreadyOpen),
            (State::Closed, Event::Close) => Reaction::Reject(Rejection::AlreadyClosed),
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut state = Door.initial()?;

    let committed = Door.dispatch(&mut state, &Event::Open, &())?;
    assert_eq!(
        (
            committed.transition(),
            committed.from(),
            committed.to(),
            committed.effect(),
        ),
        (&Transition::Opened, &State::Closed, &State::Open, None),
    );

    let rejected = Door.dispatch(&mut state, &Event::Open, &());
    assert!(matches!(
        rejected,
        Err(MachineFailure::Rejected(RejectReason::Refused(
            Rejection::AlreadyOpen,
        )))
    ));
    assert_eq!(state, State::Open);

    Door.dispatch(&mut state, &Event::Close, &())?;
    assert_eq!(state, State::Closed);

    println!("state={state:?}, opened={committed:?}");
    Ok(())
}
