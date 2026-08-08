#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

use core::fmt::{self, Debug, Display, Formatter};

pub use fsm_macros::machine;

/// A selected transition that has not been confirmed by its state owner.
///
/// In-memory generated machines confirm plans themselves. Database-backed
/// callers inspect a plan, commit its state and effect in their transaction,
/// and call [`Plan::confirm`] only after that transaction succeeds.
#[derive(Clone, Debug, Eq, PartialEq)]
#[must_use = "a plan must be committed by its state owner or discarded"]
pub struct Plan<S, T, F> {
    /// Stable identity of the selected transition.
    pub transition: T,
    /// Active leaf before the transition.
    pub from: S,
    /// Active leaf selected as the target.
    pub to: S,
    /// Caller-owned external work selected by the transition.
    pub effect: Option<F>,
}

impl<S, T, F> Plan<S, T, F> {
    /// Converts this plan into an applied transition after its owner commits.
    pub fn confirm(self) -> Applied<S, T, F> {
        Applied {
            transition: self.transition,
            from: self.from,
            to: self.to,
            effect: self.effect,
        }
    }
}

/// A transition confirmed by its in-memory or durable state owner.
#[derive(Clone, Debug, Eq, PartialEq)]
#[must_use = "the applied transition contains observable business output"]
pub struct Applied<S, T, F> {
    /// Stable identity of the selected transition.
    pub transition: T,
    /// Active leaf before the transition.
    pub from: S,
    /// Active leaf after the transition.
    pub to: S,
    /// Caller-owned external work selected by the transition.
    pub effect: Option<F>,
}

/// The reason no transition was selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessError<S, E, R> {
    /// No row handled the event at the active leaf or any ancestor.
    ///
    /// Generated machines use `StateId` for `S`, reporting state identity
    /// without cloning a potentially payload-bearing active state.
    Unhandled {
        /// Active leaf identity at selection time.
        state: S,
        /// Event that no row handled.
        event: E,
    },
    /// A matching row understood and explicitly refused the event.
    Rejected(R),
}

impl<S: Debug, E: Debug, R: Debug> Display for ProcessError<S, E, R> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unhandled { state, event } => {
                write!(
                    formatter,
                    "event {event:?} was not handled in state {state:?}"
                )
            }
            Self::Rejected(reason) => write!(formatter, "event was rejected: {reason:?}"),
        }
    }
}

impl<S: Debug, E: Debug, R: Debug> std::error::Error for ProcessError<S, E, R> {}
