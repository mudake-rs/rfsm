use std::convert::Infallible;
use std::future::{self, Future};
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use fsm::{HierarchyError, Machine, MachineFailure, Reaction, RejectReason, StalePlan, StateKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DoorState {
    Closed,
    Open,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DoorEvent {
    Open,
    Lock,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DoorTransition {
    Opened,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DoorRejection {
    AlreadyOpen,
}

struct Door;

impl Machine for Door {
    type State = DoorState;
    type Event = DoorEvent;
    type Context = ();
    type Transition = DoorTransition;
    type Effect = Infallible;
    type Rejection = DoorRejection;

    fn initial_target(&self) -> DoorState {
        DoorState::Closed
    }

    fn react(
        &self,
        _active: &DoorState,
        at: &DoorState,
        event: &DoorEvent,
        _context: &(),
    ) -> Reaction<DoorState, DoorTransition, Infallible, DoorRejection> {
        match (at, event) {
            (DoorState::Closed, DoorEvent::Open) => {
                Reaction::transition(DoorTransition::Opened, DoorState::Open)
            }
            (DoorState::Open, DoorEvent::Open) => Reaction::Reject(DoorRejection::AlreadyOpen),
            (_, DoorEvent::Lock) => Reaction::Bubble,
        }
    }
}

#[test]
fn accepted_transition_commits_and_reports_the_actual_transition() {
    let mut state = DoorState::Closed;

    let committed = Door
        .dispatch(&mut state, &DoorEvent::Open, &())
        .unwrap_or_else(|failure| panic!("unexpected dispatch failure: {failure}"));

    assert_eq!(
        (
            committed.transition(),
            committed.from(),
            committed.to(),
            committed.effect(),
        ),
        (
            &DoorTransition::Opened,
            &DoorState::Closed,
            &DoorState::Open,
            None,
        ),
    );
    assert_eq!(state, DoorState::Open);
}

#[test]
fn refused_and_unhandled_events_preserve_current_state() {
    let mut open = DoorState::Open;
    let refused = Door.dispatch(&mut open, &DoorEvent::Open, &());

    assert_eq!(
        refused,
        Err(MachineFailure::Rejected(RejectReason::Refused(
            DoorRejection::AlreadyOpen,
        )))
    );
    assert_eq!(open, DoorState::Open);

    let mut closed = DoorState::Closed;
    let unhandled = Door.dispatch(&mut closed, &DoorEvent::Lock, &());

    assert_eq!(
        unhandled,
        Err(MachineFailure::Rejected(RejectReason::Unhandled))
    );
    assert_eq!(closed, DoorState::Closed);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrderState {
    Draft,
    Checkout,
    Payment,
    Authorizing,
    Authorized,
    Cancelled,
    Completed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrderEvent {
    BeginPayment,
    Authorize,
    Cancel,
    Complete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrderTransition {
    BeganPayment,
    Authorized,
    AncestorAuthorizeFallback,
    Cancelled,
    Completed,
}

#[derive(Debug, Eq, PartialEq)]
struct OrderEffect(&'static str);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrderRejection {
    AlreadyAuthorized,
}

struct Orders {
    initial: OrderState,
}

impl Machine for Orders {
    type State = OrderState;
    type Event = OrderEvent;
    type Context = ();
    type Transition = OrderTransition;
    type Effect = OrderEffect;
    type Rejection = OrderRejection;

    fn initial_target(&self) -> OrderState {
        self.initial
    }

    fn parent(&self, state: &OrderState) -> Option<OrderState> {
        match state {
            OrderState::Authorizing | OrderState::Authorized => Some(OrderState::Payment),
            OrderState::Payment => Some(OrderState::Checkout),
            _ => None,
        }
    }

    fn state_kind(&self, state: &OrderState) -> StateKind<OrderState> {
        match state {
            OrderState::Checkout => StateKind::Compound(OrderState::Payment),
            OrderState::Payment => StateKind::Compound(OrderState::Authorizing),
            _ => StateKind::Leaf,
        }
    }

    fn react(
        &self,
        active: &OrderState,
        at: &OrderState,
        event: &OrderEvent,
        _context: &(),
    ) -> Reaction<OrderState, OrderTransition, OrderEffect, OrderRejection> {
        match (at, event) {
            (OrderState::Draft, OrderEvent::BeginPayment) => {
                Reaction::transition(OrderTransition::BeganPayment, OrderState::Payment)
            }
            (OrderState::Authorizing, OrderEvent::Authorize) => Reaction::transition_with(
                OrderTransition::Authorized,
                OrderState::Authorized,
                OrderEffect("record authorization"),
            ),
            (OrderState::Authorized, OrderEvent::Authorize) => {
                Reaction::Reject(OrderRejection::AlreadyAuthorized)
            }
            (OrderState::Payment, OrderEvent::Authorize) => Reaction::transition(
                OrderTransition::AncestorAuthorizeFallback,
                OrderState::Cancelled,
            ),
            (OrderState::Payment, OrderEvent::Cancel) => match active {
                OrderState::Authorized => Reaction::transition_with(
                    OrderTransition::Cancelled,
                    OrderState::Cancelled,
                    OrderEffect("release authorization"),
                ),
                _ => Reaction::transition(OrderTransition::Cancelled, OrderState::Cancelled),
            },
            (OrderState::Authorized, OrderEvent::Complete) => {
                Reaction::transition(OrderTransition::Completed, OrderState::Completed)
            }
            _ => Reaction::Bubble,
        }
    }
}

#[test]
fn compound_initial_and_transition_targets_resolve_to_their_initial_leaf() {
    let payment_session = Orders {
        initial: OrderState::Payment,
    };
    assert_eq!(payment_session.initial(), Ok(OrderState::Authorizing));
    let checkout_session = Orders {
        initial: OrderState::Checkout,
    };
    assert_eq!(checkout_session.initial(), Ok(OrderState::Authorizing));

    let orders = Orders {
        initial: OrderState::Draft,
    };
    let mut state = OrderState::Draft;
    let committed = orders
        .dispatch(&mut state, &OrderEvent::BeginPayment, &())
        .unwrap_or_else(|failure| panic!("unexpected dispatch failure: {failure}"));

    assert_eq!(
        (
            committed.transition(),
            committed.from(),
            committed.to(),
            committed.effect(),
        ),
        (
            &OrderTransition::BeganPayment,
            &OrderState::Draft,
            &OrderState::Authorizing,
            None,
        ),
    );
    assert_eq!(state, OrderState::Authorizing);
}

#[test]
fn child_transition_and_rejection_take_precedence_over_parent_fallback() {
    let orders = Orders {
        initial: OrderState::Draft,
    };
    let mut state = OrderState::Authorizing;

    let authorized = orders
        .dispatch(&mut state, &OrderEvent::Authorize, &())
        .unwrap_or_else(|failure| panic!("unexpected dispatch failure: {failure}"));
    assert_eq!(
        (
            authorized.transition(),
            authorized.from(),
            authorized.to(),
            authorized.effect(),
        ),
        (
            &OrderTransition::Authorized,
            &OrderState::Authorizing,
            &OrderState::Authorized,
            Some(&OrderEffect("record authorization")),
        ),
    );
    assert_eq!(orders.is_in(&state, &OrderState::Payment), Ok(true));
    assert_eq!(orders.is_in(&state, &state), Ok(true));

    let refused = orders.dispatch(&mut state, &OrderEvent::Authorize, &());
    assert_eq!(
        refused,
        Err(MachineFailure::Rejected(RejectReason::Refused(
            OrderRejection::AlreadyAuthorized,
        )))
    );
    assert_eq!(state, OrderState::Authorized);
}

#[test]
fn parent_rule_handles_child_event_without_losing_the_active_leaf() {
    let orders = Orders {
        initial: OrderState::Draft,
    };
    let mut state = OrderState::Authorized;

    let committed = orders
        .dispatch(&mut state, &OrderEvent::Cancel, &())
        .unwrap_or_else(|failure| panic!("unexpected dispatch failure: {failure}"));

    let (transition, from, to, effect) = committed.into_parts();
    assert_eq!(
        (transition, from, to, effect),
        (
            OrderTransition::Cancelled,
            OrderState::Authorized,
            OrderState::Cancelled,
            Some(OrderEffect("release authorization")),
        ),
    );
    assert_eq!(state, OrderState::Cancelled);
}

#[test]
fn child_rule_can_leave_its_compound_parent() {
    let orders = Orders {
        initial: OrderState::Draft,
    };
    let mut state = OrderState::Authorized;

    let committed = orders
        .dispatch(&mut state, &OrderEvent::Complete, &())
        .unwrap_or_else(|failure| panic!("unexpected dispatch failure: {failure}"));

    assert_eq!(
        (
            committed.transition(),
            committed.from(),
            committed.to(),
            committed.effect(),
        ),
        (
            &OrderTransition::Completed,
            &OrderState::Authorized,
            &OrderState::Completed,
            None,
        ),
    );
    assert_eq!(orders.is_in(&state, &OrderState::Payment), Ok(false));
}

#[test]
fn compound_state_cannot_be_used_as_current_state() {
    let orders = Orders {
        initial: OrderState::Draft,
    };
    let mut state = OrderState::Payment;

    let failure = orders.dispatch(&mut state, &OrderEvent::Cancel, &());

    assert_eq!(
        failure,
        Err(MachineFailure::InvalidHierarchy(
            HierarchyError::ActiveStateIsCompound {
                state: OrderState::Payment,
            },
        ))
    );
    assert_eq!(state, OrderState::Payment);
    assert_eq!(
        orders.is_in(&state, &OrderState::Payment),
        Err(HierarchyError::ActiveStateIsCompound {
            state: OrderState::Payment,
        })
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Node {
    Leaf,
    A,
    B,
}

#[derive(Clone, Copy)]
enum Malformation {
    ParentCycle,
    InitialCycle,
    InitialOutsideParent,
    ParentIsLeaf,
}

struct BrokenHierarchy(Malformation);

impl Machine for BrokenHierarchy {
    type State = Node;
    type Event = ();
    type Context = ();
    type Transition = ();
    type Effect = Infallible;
    type Rejection = Infallible;

    fn initial_target(&self) -> Node {
        match self.0 {
            Malformation::ParentCycle => Node::Leaf,
            _ => Node::A,
        }
    }

    fn parent(&self, state: &Node) -> Option<Node> {
        match (self.0, state) {
            (Malformation::ParentCycle, Node::Leaf) => Some(Node::A),
            (Malformation::ParentCycle, Node::A) => Some(Node::B),
            (Malformation::ParentCycle, Node::B) => Some(Node::A),
            (Malformation::InitialCycle, Node::A) => Some(Node::B),
            (Malformation::InitialCycle, Node::B) => Some(Node::A),
            (Malformation::InitialOutsideParent, Node::Leaf) => Some(Node::A),
            (Malformation::ParentIsLeaf, Node::A) => Some(Node::B),
            _ => None,
        }
    }

    fn state_kind(&self, state: &Node) -> StateKind<Node> {
        match (self.0, state) {
            (Malformation::ParentCycle, Node::A) => StateKind::Compound(Node::Leaf),
            (Malformation::ParentCycle, Node::B) => StateKind::Compound(Node::A),
            (Malformation::InitialCycle, Node::A) => StateKind::Compound(Node::B),
            (Malformation::InitialCycle, Node::B) => StateKind::Compound(Node::A),
            (Malformation::InitialOutsideParent, Node::A) => StateKind::Compound(Node::B),
            _ => StateKind::Leaf,
        }
    }

    fn react(
        &self,
        _active: &Node,
        _at: &Node,
        _event: &(),
        _context: &(),
    ) -> Reaction<Node, (), Infallible, Infallible> {
        match self.0 {
            Malformation::ParentIsLeaf => Reaction::transition((), Node::A),
            _ => Reaction::transition((), Node::Leaf),
        }
    }
}

#[test]
fn malformed_hierarchies_fail_before_state_mutation() {
    let parent_cycle = BrokenHierarchy(Malformation::ParentCycle);
    let mut state = Node::Leaf;
    let failure = parent_cycle.dispatch(&mut state, &(), &());
    assert_eq!(
        failure,
        Err(MachineFailure::InvalidHierarchy(
            HierarchyError::ParentCycle { state: Node::A },
        ))
    );
    assert_eq!(state, Node::Leaf);
    assert_eq!(
        parent_cycle.is_in(&state, &Node::B),
        Err(HierarchyError::ParentCycle { state: Node::A })
    );

    let initial_cycle = BrokenHierarchy(Malformation::InitialCycle);
    assert_eq!(
        initial_cycle.initial(),
        Err(HierarchyError::InitialChildCycle { state: Node::A })
    );

    let outside = BrokenHierarchy(Malformation::InitialOutsideParent);
    assert_eq!(
        outside.initial(),
        Err(HierarchyError::InitialChildOutsideParent {
            parent: Node::A,
            child: Node::B,
        })
    );
    let mut state = Node::Leaf;
    assert_eq!(
        outside.dispatch(&mut state, &(), &()),
        Err(MachineFailure::InvalidHierarchy(
            HierarchyError::InitialChildOutsideParent {
                parent: Node::A,
                child: Node::B,
            },
        ))
    );
    assert_eq!(state, Node::Leaf);

    let parent_is_leaf = BrokenHierarchy(Malformation::ParentIsLeaf);
    let mut state = Node::A;
    assert_eq!(
        parent_is_leaf.dispatch(&mut state, &(), &()),
        Err(MachineFailure::InvalidHierarchy(
            HierarchyError::ParentIsLeaf {
                parent: Node::B,
                child: Node::A,
            },
        ))
    );
    assert_eq!(state, Node::A);

    let mut valid_source = Node::Leaf;
    assert_eq!(
        parent_is_leaf.dispatch(&mut valid_source, &(), &()),
        Err(MachineFailure::InvalidHierarchy(
            HierarchyError::ParentIsLeaf {
                parent: Node::B,
                child: Node::A,
            },
        ))
    );
    assert_eq!(valid_source, Node::Leaf);
}

#[test]
fn stale_plan_does_not_overwrite_current_state() {
    let plan = Door
        .plan_from(&DoorState::Closed, &DoorEvent::Open, &())
        .unwrap_or_else(|failure| panic!("unexpected selection failure: {failure}"));
    let mut current = DoorState::Open;

    let stale = plan.commit(&mut current);

    assert_eq!(
        stale,
        Err(StalePlan {
            expected: DoorState::Closed,
            actual: DoorState::Open,
        })
    );
    assert_eq!(current, DoorState::Open);
}

#[test]
fn current_plan_commit_updates_state_and_returns_a_receipt() {
    let plan = Door
        .plan_from(&DoorState::Closed, &DoorEvent::Open, &())
        .unwrap_or_else(|failure| panic!("unexpected selection failure: {failure}"));
    let mut current = DoorState::Closed;

    let committed = plan
        .commit(&mut current)
        .unwrap_or_else(|failure| panic!("unexpected commit failure: {failure}"));

    assert_eq!(
        (
            committed.transition(),
            committed.from(),
            committed.to(),
            committed.effect(),
        ),
        (
            &DoorTransition::Opened,
            &DoorState::Closed,
            &DoorState::Open,
            None,
        ),
    );
    assert_eq!(current, DoorState::Open);
}

#[test]
fn cancellation_before_commit_leaves_caller_state_unchanged() {
    let mut current = DoorState::Closed;
    let plan = Door
        .plan_from(&current, &DoorEvent::Open, &())
        .unwrap_or_else(|failure| panic!("unexpected selection failure: {failure}"));

    let mut work = Box::pin(async {
        future::pending::<()>().await;
        plan.commit(&mut current)
    });
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(
        Pin::as_mut(&mut work).poll(&mut context),
        Poll::Pending,
    ));

    drop(work);

    assert_eq!(current, DoorState::Closed);
}
