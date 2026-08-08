use std::error::Error;

use rfsm::{ProcessError, machine};

machine! {
    name: Door,
    states: { *Closed, Open },
    events: { Open, Close },

    transitions: {
        Opened: Closed + Open => Open,
        ClosedAgain: Open + Close => Closed,
        Open + Open => reject AlreadyOpen,
        Closed + Close => reject AlreadyClosed,
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut door = Door::new();

    let opened = door.process(Event::Open)?;
    assert_eq!(door.state(), &State::Open);
    assert_eq!(opened.transition, Transition::Opened);
    assert_eq!(opened.from, State::Closed);
    assert_eq!(opened.to, State::Open);
    assert_eq!(opened.effect, None);

    let rejected = door.process(Event::Open);
    assert_eq!(
        rejected,
        Err(ProcessError::Rejected(Rejection::AlreadyOpen))
    );
    assert_eq!(door.state(), &State::Open);

    let _closed = door.process(Event::Close)?;
    assert_eq!(door.state(), &State::Closed);

    println!("state={:?}, opened={opened:?}", door.state());
    Ok(())
}
