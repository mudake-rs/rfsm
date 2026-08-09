#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

use core::fmt::{self, Debug, Display, Formatter};

pub use rfsm_macros::machine;

#[doc(hidden)]
#[cfg(feature = "serde")]
pub use serde;

/// A transition committed to a machine instance.
///
/// This value does not prove that caller-owned storage was updated. A
/// database-backed application should publish it only after its transaction
/// commits.
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
