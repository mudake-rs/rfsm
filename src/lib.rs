//! Caller-owned finite state machines.
//!
//! This crate is a compileable API prototype. A [`Machine`] owns immutable
//! transition and hierarchy rules; the application or database owns current
//! state and external effects.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use std::error::Error;
use std::fmt::{self, Debug, Display, Formatter};

/// The result of selecting a rule at one hierarchy level.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reaction<S, T, F, R> {
    /// Selects a transition and its target, with an optional caller-owned effect.
    Transition {
        /// Stable business identity of the selected transition.
        transition: T,
        /// Leaf or compound transition target.
        target: S,
        /// Data for caller-owned work after selection.
        effect: Option<F>,
    },
    /// Lets the next ancestor try the event.
    Bubble,
    /// Stops propagation because the event was understood and refused.
    Reject(R),
}

impl<S, T, F, R> Reaction<S, T, F, R> {
    /// Creates a transition without an effect.
    pub fn transition(transition: T, target: S) -> Self {
        Self::Transition {
            transition,
            target,
            effect: None,
        }
    }

    /// Creates a transition carrying caller-owned effect data.
    pub fn transition_with(transition: T, target: S, effect: F) -> Self {
        Self::Transition {
            transition,
            target,
            effect: Some(effect),
        }
    }
}

/// A state's role in a hierarchy and its entry state when compound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StateKind<S> {
    /// A state that can be active.
    Leaf,
    /// A hierarchy marker that resolves to an initial child on entry.
    Compound(S),
}

/// A selected transition that has not committed caller-owned state.
///
/// Plans can only be created by [`Machine::plan_from`]; callers may inspect but
/// cannot retarget them. Plans intentionally are not [`Clone`], so one
/// selection has one commit path.
///
/// ```compile_fail
/// use fsm::TransitionPlan;
///
/// let _forged = TransitionPlan::<u8, u8, u8> {
///     transition: 1,
///     from: 2,
///     to: 3,
///     effect: None,
/// };
/// ```
#[derive(Debug, Eq, PartialEq)]
pub struct TransitionPlan<S, T, F> {
    transition: T,
    from: S,
    to: S,
    effect: Option<F>,
}

/// A transition that the caller or library has committed.
///
/// Receipts can only be produced from a selected [`TransitionPlan`]. Cloned
/// receipts describe the same completed transition; they do not commit again.
///
/// ```compile_fail
/// use fsm::Committed;
///
/// let _forged = Committed::<u8, u8, u8> {
///     transition: 1,
///     from: 2,
///     to: 3,
///     effect: None,
/// };
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Committed<S, T, F> {
    transition: T,
    from: S,
    to: S,
    effect: Option<F>,
}

/// The reason an event did not select a transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RejectReason<R> {
    /// No rule handled the event at the active leaf or any ancestor.
    Unhandled,
    /// A rule understood the event and explicitly refused it.
    Refused(R),
}

impl<R: Debug> Display for RejectReason<R> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unhandled => formatter.write_str("event was not handled"),
            Self::Refused(reason) => write!(formatter, "event was refused: {reason:?}"),
        }
    }
}

impl<R: Debug> Error for RejectReason<R> {}

/// A malformed hierarchy or invalid active-state input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HierarchyError<S> {
    /// Parent traversal revisited a state.
    ParentCycle {
        /// State revisited by parent traversal.
        state: S,
    },
    /// Initial-child resolution revisited a state.
    InitialChildCycle {
        /// State revisited by initial-child resolution.
        state: S,
    },
    /// An initial child does not name the expected state as its parent.
    InitialChildOutsideParent {
        /// Compound state declaring the child.
        parent: S,
        /// Invalid initial child.
        child: S,
    },
    /// A parent mapping points to a state declared as a leaf.
    ParentIsLeaf {
        /// State incorrectly used as a parent.
        parent: S,
        /// Child that names the invalid parent.
        child: S,
    },
    /// A compound marker was supplied where an active leaf is required.
    ActiveStateIsCompound {
        /// Invalid active state.
        state: S,
    },
}

impl<S: Debug> Display for HierarchyError<S> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::ParentCycle { state } => {
                write!(formatter, "parent traversal revisited {state:?}")
            }
            Self::InitialChildCycle { state } => {
                write!(formatter, "initial-child traversal revisited {state:?}")
            }
            Self::InitialChildOutsideParent { parent, child } => write!(
                formatter,
                "initial child {child:?} does not belong to {parent:?}"
            ),
            Self::ParentIsLeaf { parent, child } => {
                write!(
                    formatter,
                    "leaf state {parent:?} is the parent of {child:?}"
                )
            }
            Self::ActiveStateIsCompound { state } => {
                write!(formatter, "active state must be a leaf, got {state:?}")
            }
        }
    }
}

impl<S: Debug> Error for HierarchyError<S> {}

/// A dispatch failure that preserves caller-owned state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MachineFailure<R, S> {
    /// The event was unhandled or explicitly refused.
    Rejected(RejectReason<R>),
    /// The hierarchy or active-state input was invalid.
    InvalidHierarchy(HierarchyError<S>),
}

impl<R: Debug, S: Debug> Display for MachineFailure<R, S> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(reason) => Display::fmt(reason, formatter),
            Self::InvalidHierarchy(error) => Display::fmt(error, formatter),
        }
    }
}

impl<R: Debug, S: Debug> Error for MachineFailure<R, S> {}

/// A staged plan whose source no longer matches caller-owned state.
///
/// Its fields are public error data, not transition provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StalePlan<S> {
    /// Source state captured during selection.
    pub expected: S,
    /// Current state observed at commit time.
    pub actual: S,
}

impl<S: Debug> Display for StalePlan<S> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "stale transition plan: expected {:?}, found {:?}",
            self.expected, self.actual
        )
    }
}

impl<S: Debug> Error for StalePlan<S> {}

impl<S, T, F> TransitionPlan<S, T, F> {
    /// Returns the selected transition identity.
    pub fn transition(&self) -> &T {
        &self.transition
    }

    /// Returns the active leaf used for selection.
    pub fn from(&self) -> &S {
        &self.from
    }

    /// Returns the resolved target leaf.
    pub fn to(&self) -> &S {
        &self.to
    }

    /// Returns effect data for caller-owned handling.
    pub fn effect(&self) -> Option<&F> {
        self.effect.as_ref()
    }

    /// Commits to caller-owned memory if its source still matches.
    ///
    /// Equality detects interleaved mutation, not ABA changes. Callers using
    /// shared or durable state still need an application-owned version or lock.
    pub fn commit(self, current: &mut S) -> Result<Committed<S, T, F>, StalePlan<S>>
    where
        S: Clone + Eq,
    {
        if *current != self.from {
            return Err(StalePlan {
                expected: self.from,
                actual: current.clone(),
            });
        }

        *current = self.to.clone();
        Ok(self.committed_by_caller())
    }

    /// Converts a plan after the caller confirms its own durable commit.
    ///
    /// The library cannot verify an application-owned transaction. Calling this
    /// method is the caller's explicit assertion that the transition committed.
    pub fn committed_by_caller(self) -> Committed<S, T, F> {
        Committed {
            transition: self.transition,
            from: self.from,
            to: self.to,
            effect: self.effect,
        }
    }
}

impl<S, T, F> Committed<S, T, F> {
    /// Returns the committed transition identity.
    pub fn transition(&self) -> &T {
        &self.transition
    }

    /// Returns the active leaf before commit.
    pub fn from(&self) -> &S {
        &self.from
    }

    /// Returns the active leaf after commit.
    pub fn to(&self) -> &S {
        &self.to
    }

    /// Returns effect data produced during selection.
    pub fn effect(&self) -> Option<&F> {
        self.effect.as_ref()
    }

    /// Consumes the receipt and returns its transition, states, and effect.
    pub fn into_parts(self) -> (T, S, S, Option<F>) {
        (self.transition, self.from, self.to, self.effect)
    }
}

/// Pure transition and hierarchy rules over caller-owned current state.
pub trait Machine {
    /// Active leaf and hierarchy marker type.
    type State: Clone + Eq;
    /// Accepted event type.
    type Event;
    /// Read-only facts prepared before selection.
    type Context;
    /// Stable business transition identity.
    type Transition;
    /// Data returned for caller-owned effect handling.
    type Effect;
    /// Domain-specific refusal reason.
    type Rejection;

    /// Returns a leaf or compound initial target from the definition.
    fn initial_target(&self) -> Self::State;

    /// Returns the parent of a state, or `None` at the hierarchy root.
    ///
    /// Implementations must be pure and deterministic. Valid chains are finite
    /// and end at `None`; repeated-state cycles are reported as hierarchy
    /// errors, while an infinitely growing chain violates this trait contract.
    fn parent(&self, _state: &Self::State) -> Option<Self::State> {
        None
    }

    /// Classifies a state and defines compound-state entry.
    ///
    /// Implementations must be pure and deterministic. Valid initial-child
    /// chains are finite and reach a leaf; repeated-state cycles are reported
    /// as hierarchy errors, while an infinitely growing chain violates this
    /// trait contract. Every state returned by [`Machine::parent`] must be
    /// classified as [`StateKind::Compound`], and every compound initial child
    /// must return that compound from [`Machine::parent`].
    fn state_kind(&self, _state: &Self::State) -> StateKind<Self::State> {
        StateKind::Leaf
    }

    /// Selects a reaction at one hierarchy level without mutation or I/O.
    ///
    /// `active` remains the original active leaf while `at` moves from that leaf
    /// through its ancestors. Dispatch calls this once per visited level, leaf
    /// first. Implementations must be pure and idempotent across those calls.
    fn react(
        &self,
        active: &Self::State,
        at: &Self::State,
        event: &Self::Event,
        context: &Self::Context,
    ) -> Reaction<Self::State, Self::Transition, Self::Effect, Self::Rejection>;

    /// Resolves the declared initial target to a validated active leaf.
    fn initial(&self) -> Result<Self::State, HierarchyError<Self::State>> {
        resolve_target(self, self.initial_target())
    }

    /// Selects a transition from caller-owned state without mutation.
    #[allow(
        clippy::type_complexity,
        reason = "a public alias would add API without simplifying call sites"
    )]
    fn plan_from(
        &self,
        state: &Self::State,
        event: &Self::Event,
        context: &Self::Context,
    ) -> Result<
        TransitionPlan<Self::State, Self::Transition, Self::Effect>,
        MachineFailure<Self::Rejection, Self::State>,
    > {
        validate_active_leaf(self, state).map_err(MachineFailure::InvalidHierarchy)?;

        let mut at = state.clone();

        loop {
            match self.react(state, &at, event, context) {
                Reaction::Transition {
                    transition,
                    target,
                    effect,
                } => {
                    let to =
                        resolve_target(self, target).map_err(MachineFailure::InvalidHierarchy)?;
                    return Ok(TransitionPlan {
                        transition,
                        from: state.clone(),
                        to,
                        effect,
                    });
                }
                Reaction::Reject(reason) => {
                    return Err(MachineFailure::Rejected(RejectReason::Refused(reason)));
                }
                Reaction::Bubble => match self.parent(&at) {
                    Some(parent) => at = parent,
                    None => {
                        return Err(MachineFailure::Rejected(RejectReason::Unhandled));
                    }
                },
            }
        }
    }

    /// Selects and atomically commits a transition to caller-owned memory.
    #[allow(
        clippy::type_complexity,
        reason = "a public alias would add API without simplifying call sites"
    )]
    fn dispatch(
        &self,
        state: &mut Self::State,
        event: &Self::Event,
        context: &Self::Context,
    ) -> Result<
        Committed<Self::State, Self::Transition, Self::Effect>,
        MachineFailure<Self::Rejection, Self::State>,
    > {
        let plan = self.plan_from(state, event, context)?;
        *state = plan.to.clone();
        Ok(plan.committed_by_caller())
    }

    /// Reports whether a caller-owned active leaf is inside an ancestor.
    ///
    /// State comparisons use [`Eq`], including any payload carried by a state.
    fn is_in(
        &self,
        active: &Self::State,
        ancestor: &Self::State,
    ) -> Result<bool, HierarchyError<Self::State>> {
        validate_active_leaf(self, active)?;

        let mut at = active.clone();

        loop {
            if &at == ancestor {
                return Ok(true);
            }

            match self.parent(&at) {
                Some(parent) => at = parent,
                None => return Ok(false),
            }
        }
    }
}

fn validate_active_leaf<M>(machine: &M, active: &M::State) -> Result<(), HierarchyError<M::State>>
where
    M: Machine + ?Sized,
{
    if matches!(machine.state_kind(active), StateKind::Compound(_)) {
        return Err(HierarchyError::ActiveStateIsCompound {
            state: active.clone(),
        });
    }

    let Some(mut at) = machine.parent(active) else {
        return Ok(());
    };
    let mut child = active.clone();
    let mut visited = vec![child.clone()];

    loop {
        if visited.contains(&at) {
            return Err(HierarchyError::ParentCycle { state: at });
        }
        visited.push(at.clone());

        match machine.state_kind(&at) {
            StateKind::Leaf => {
                return Err(HierarchyError::ParentIsLeaf { parent: at, child });
            }
            StateKind::Compound(initial) => {
                if machine.parent(&initial).as_ref() != Some(&at) {
                    return Err(HierarchyError::InitialChildOutsideParent {
                        parent: at,
                        child: initial,
                    });
                }
            }
        }

        child = at.clone();
        match machine.parent(&at) {
            Some(parent) => at = parent,
            None => return Ok(()),
        }
    }
}

fn resolve_target<M>(machine: &M, target: M::State) -> Result<M::State, HierarchyError<M::State>>
where
    M: Machine + ?Sized,
{
    let mut at = target;
    let mut visited = Vec::new();

    loop {
        let child = match machine.state_kind(&at) {
            StateKind::Leaf => {
                validate_active_leaf(machine, &at)?;
                return Ok(at);
            }
            StateKind::Compound(child) => {
                if visited.contains(&at) {
                    return Err(HierarchyError::InitialChildCycle { state: at });
                }
                visited.push(at.clone());
                child
            }
        };

        if machine.parent(&child).as_ref() != Some(&at) {
            return Err(HierarchyError::InitialChildOutsideParent { parent: at, child });
        }
        at = child;
    }
}
