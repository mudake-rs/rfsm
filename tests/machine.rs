mod flat {
    use rfsm::{ProcessError, machine};

    machine! {
        name: Door,
        states: { *Closed, Open },
        events: { Open, Close, Knock },
        transitions: {
            _ + _ => reject InvalidEvent,
            Knocked: Closed + Knock => _,
            Opened: Closed + Open => Open,
            ClosedAgain: Open + Close => Closed,
            Open + Open => reject AlreadyOpen,
            Closed + Close => reject AlreadyClosed,
        }
    }

    #[test]
    fn accepted_transition_commits_and_reports_business_identity() {
        let mut door = Door::new();

        let knocked = door
            .process(Event::Knock)
            .unwrap_or_else(|failure| panic!("unexpected failure: {failure}"));
        assert_eq!(knocked.transition, Transition::Knocked);
        assert_eq!(knocked.from, State::Closed);
        assert_eq!(knocked.to, State::Closed);
        assert_eq!(door.state(), &State::Closed);

        let applied = door
            .process(Event::Open)
            .unwrap_or_else(|failure| panic!("unexpected failure: {failure}"));

        assert_eq!(door.state(), &State::Open);
        assert_eq!(door.state_id(), StateId::Open);
        assert_eq!(applied.transition, Transition::Opened);
        assert_eq!(applied.from, State::Closed);
        assert_eq!(applied.to, State::Open);
        assert_eq!(applied.effect, None);

        let closed = door
            .process(Event::Close)
            .unwrap_or_else(|failure| panic!("unexpected failure: {failure}"));
        assert_eq!(closed.transition, Transition::ClosedAgain);
        assert_eq!(door.state(), &State::Closed);

        let plan = Door::evaluate(&State::Closed, &Event::Open)
            .unwrap_or_else(|failure| panic!("unexpected failure: {failure}"));
        assert_eq!(plan.transition, Transition::Opened);
        assert_eq!(plan.to, State::Open);
    }

    #[test]
    fn explicit_rejection_preserves_current_state() {
        let mut door = Door::from_state(State::Open);

        let rejected = door.process(Event::Open);

        assert_eq!(
            rejected,
            Err(ProcessError::Rejected(Rejection::AlreadyOpen))
        );
        assert_eq!(door.state(), &State::Open);
        assert_eq!(
            rejected.unwrap_err().to_string(),
            "event was rejected: AlreadyOpen"
        );
    }
}

mod reject_only {
    use rfsm::{ProcessError, machine};

    machine! {
        name: RefusalOnly,
        states: { *Locked },
        events: { Try },
        transitions: {
            Locked + Try => reject Denied,
        }
    }

    #[test]
    fn rejection_only_machine_has_no_committable_transition() {
        let _uninhabited: fn(Transition) -> ! = |transition| match transition {};
        let mut machine = RefusalOnly::new();

        assert_eq!(
            machine.process(Event::Try),
            Err(ProcessError::Rejected(Rejection::Denied))
        );
        assert_eq!(machine.state(), &State::Locked);
    }
}

mod nested {
    use rfsm::{ProcessError, machine};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Token(u64);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Effect {
        Record(Token),
        Release(Token),
    }

    struct Facts {
        may_cancel: bool,
    }

    machine! {
        name: Workflow,
        context: Facts,
        effect: Effect,
        states: {
            *Idle,
            r#Flow {
                *Waiting,
                r#Ready { r#token: Token },
                Review,
            },
            Failed,
            Done,
        },
        events: {
            Begin,
            r#Accept { token: Token },
            Cancel,
            Finish,
        },
        transitions: {
            ParentAccepted: Flow + Accept { .. } => Failed,
            Flow + Cancel => reject CancellationBlocked,
            Began: Idle + Begin => Flow,
            r#Accepted: Waiting + Accept { r#token } / record(token) => Ready { token },
            Ready { .. } + Accept { .. } => reject Duplicate,
            CancelledReady: Ready { token } + Cancel [r#may_cancel]
                / release(token) => Failed,
            CancelledWaiting: Waiting + Cancel [may_cancel] => Failed,
            Finished: Ready { .. } + Finish => Done,
            _ + _ => reject InvalidEvent,
        }
    }

    impl WorkflowContext for Facts {
        fn may_cancel(&self) -> bool {
            self.may_cancel
        }

        fn record(&self, token: &Token) -> Effect {
            Effect::Record(*token)
        }

        fn release(&self, token: &Token) -> Effect {
            Effect::Release(*token)
        }
    }

    #[test]
    fn compound_target_enters_initial_leaf_and_reports_membership() {
        let mut workflow = Workflow::new(Facts { may_cancel: true });

        let applied = workflow
            .process(Event::Begin)
            .unwrap_or_else(|failure| panic!("unexpected failure: {failure}"));

        assert_eq!(applied.to, State::Waiting);
        assert_eq!(workflow.state(), &State::Waiting);
        assert!(workflow.is_in(StateId::Flow));
        assert!(workflow.is_in(StateId::Waiting));
        assert!(!workflow.is_in(StateId::Idle));
    }

    #[test]
    fn payload_binding_builds_target_and_effect() {
        let token = Token(7);
        let mut workflow = Workflow::from_state(State::Waiting, Facts { may_cancel: true });

        let applied = workflow
            .process(Event::Accept { token })
            .unwrap_or_else(|failure| panic!("unexpected failure: {failure}"));

        assert_eq!(workflow.state(), &State::Ready { token });
        assert_eq!(applied.transition, Transition::Accepted);
        assert_eq!(applied.effect, Some(Effect::Record(token)));
    }

    #[test]
    fn child_rejection_precedes_parent_transition() {
        let token = Token(7);
        let mut workflow = Workflow::from_state(State::Ready { token }, Facts { may_cancel: true });

        let rejected = workflow.process(Event::Accept { token: Token(8) });

        assert_eq!(rejected, Err(ProcessError::Rejected(Rejection::Duplicate)));
        assert_eq!(workflow.state(), &State::Ready { token });
    }

    #[test]
    fn parent_transition_handles_event_unmatched_by_child() {
        let mut workflow = Workflow::from_state(State::Review, Facts { may_cancel: true });

        let applied = workflow
            .process(Event::Accept { token: Token(9) })
            .unwrap_or_else(|failure| panic!("unexpected failure: {failure}"));

        assert_eq!(applied.transition, Transition::ParentAccepted);
        assert_eq!(workflow.state(), &State::Failed);
    }

    #[test]
    fn failed_child_guard_falls_through_then_context_update_selects_the_leaf_row() {
        let token = Token(7);
        let mut workflow =
            Workflow::from_state(State::Ready { token }, Facts { may_cancel: false });

        let rejected = workflow.process(Event::Cancel);

        assert_eq!(
            rejected,
            Err(ProcessError::Rejected(Rejection::CancellationBlocked))
        );
        assert_eq!(workflow.state(), &State::Ready { token });

        workflow.context_mut().may_cancel = true;
        let applied = workflow
            .process(Event::Cancel)
            .unwrap_or_else(|failure| panic!("unexpected failure: {failure}"));

        assert_eq!(applied.transition, Transition::CancelledReady);
        assert_eq!(applied.effect, Some(Effect::Release(token)));
        assert_eq!(workflow.state(), &State::Failed);
    }

    #[test]
    fn leaf_transition_can_leave_compound_state() {
        let token = Token(7);
        let mut workflow = Workflow::from_state(State::Ready { token }, Facts { may_cancel: true });

        let applied = workflow
            .process(Event::Finish)
            .unwrap_or_else(|failure| panic!("unexpected failure: {failure}"));

        assert_eq!(applied.transition, Transition::Finished);
        assert_eq!(workflow.state(), &State::Done);
        assert!(!workflow.is_in(StateId::Flow));
    }
}

mod deep_hierarchy {
    use rfsm::{ProcessError, machine};

    struct Facts {
        allow_inner: bool,
    }

    machine! {
        name: Deep,
        context: Facts,
        states: {
            *Outside,
            Outer {
                *Inner {
                    *A,
                    B,
                },
                C,
            },
            Done,
        },
        events: { Enter, Go, Stop, Leave },
        transitions: {
            Entered: Outside + Enter => Outer,
            InnerMoved: Inner + Go [allow_inner] => B,
            OuterMoved: Outer + Go => C,
            A + Stop => reject ChildStopped,
            InnerStopped: Inner + Stop => C,
            Left: A + Leave => Done,
            _ + _ => reject Fallback,
        }
    }

    impl DeepContext for Facts {
        fn allow_inner(&self) -> bool {
            self.allow_inner
        }
    }

    #[test]
    fn recursive_entry_and_three_level_precedence_are_explicit() {
        let mut entered = Deep::new(Facts { allow_inner: false });
        let applied = entered
            .process(Event::Enter)
            .unwrap_or_else(|failure| panic!("unexpected failure: {failure}"));
        assert_eq!(applied.to, State::A);
        assert!(entered.is_in(StateId::Outer));
        assert!(entered.is_in(StateId::Inner));
        assert!(entered.is_in(StateId::A));

        let moved = entered
            .process(Event::Go)
            .unwrap_or_else(|failure| panic!("unexpected failure: {failure}"));
        assert_eq!(moved.transition, Transition::OuterMoved);
        assert_eq!(entered.state(), &State::C);

        let mut stopped = Deep::from_state(State::A, Facts { allow_inner: true });
        assert_eq!(
            stopped.process(Event::Stop),
            Err(ProcessError::Rejected(Rejection::ChildStopped))
        );
        assert_eq!(stopped.state(), &State::A);

        let mut middle = Deep::from_state(State::B, Facts { allow_inner: true });
        let applied = middle
            .process(Event::Stop)
            .unwrap_or_else(|failure| panic!("unexpected failure: {failure}"));
        assert_eq!(applied.transition, Transition::InnerStopped);
        assert_eq!(middle.state(), &State::C);

        let mut left = Deep::from_state(State::A, Facts { allow_inner: true });
        let applied = left
            .process(Event::Leave)
            .unwrap_or_else(|failure| panic!("unexpected failure: {failure}"));
        assert_eq!(applied.transition, Transition::Left);
        assert_eq!(left.state(), &State::Done);
        assert!(!left.is_in(StateId::Outer));
        assert!(!left.is_in(StateId::Inner));
    }
}

mod async_machine {
    use std::cell::Cell;
    use std::future::{self, Future};
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    use rfsm::{ProcessError, machine};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Effect {
        Entered,
    }

    enum Decision {
        Allow,
        Deny,
        PendingGuard,
        PendingEffect,
    }

    struct Facts {
        decision: Decision,
        callback_steps: Cell<u32>,
    }

    impl Facts {
        fn new(decision: Decision) -> Self {
            Self {
                decision,
                callback_steps: Cell::new(0),
            }
        }
    }

    machine! {
        name: AsyncGate,
        context: Facts,
        effect: Effect,
        states: { *Closed, Open },
        events: { Enter },
        transitions: {
            Entered: Closed + Enter [async allowed] / async effect => Open,
            Closed + Enter => reject Denied,
            Open + Enter => reject Denied,
        }
    }

    impl AsyncGateContext for Facts {
        async fn allowed(&self) -> bool {
            self.callback_steps.set(self.callback_steps.get() + 1);
            match self.decision {
                Decision::Allow => true,
                Decision::Deny => false,
                Decision::PendingGuard => future::pending().await,
                Decision::PendingEffect => true,
            }
        }

        async fn effect(&self) -> Effect {
            self.callback_steps.set(self.callback_steps.get() + 1);
            match self.decision {
                Decision::PendingEffect => future::pending().await,
                _ => Effect::Entered,
            }
        }
    }

    fn run_ready<F: Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut context = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("future unexpectedly suspended"),
        }
    }

    #[test]
    fn async_guard_selects_transition_or_fallback_rejection() {
        let mut allowed = AsyncGate::new(Facts::new(Decision::Allow));
        let applied = run_ready(allowed.process(Event::Enter))
            .unwrap_or_else(|failure| panic!("unexpected failure: {failure}"));
        assert_eq!(applied.transition, Transition::Entered);
        assert_eq!(applied.effect, Some(Effect::Entered));
        assert_eq!(allowed.state(), &State::Open);

        let mut denied = AsyncGate::new(Facts::new(Decision::Deny));
        assert_eq!(
            run_ready(denied.process(Event::Enter)),
            Err(ProcessError::Rejected(Rejection::Denied))
        );
        assert_eq!(denied.state(), &State::Closed);
    }

    #[test]
    fn cancellation_while_async_guard_is_pending_preserves_only_machine_state() {
        let mut gate = AsyncGate::new(Facts::new(Decision::PendingGuard));
        let mut processing = Box::pin(gate.process(Event::Enter));
        let mut context = Context::from_waker(Waker::noop());

        assert!(matches!(
            Pin::as_mut(&mut processing).poll(&mut context),
            Poll::Pending
        ));
        drop(processing);

        assert_eq!(gate.state(), &State::Closed);
        assert_eq!(gate.context().callback_steps.get(), 1);
    }

    #[test]
    fn cancellation_while_async_effect_is_pending_preserves_only_machine_state() {
        let mut gate = AsyncGate::new(Facts::new(Decision::PendingEffect));
        let mut processing = Box::pin(gate.process(Event::Enter));
        let mut context = Context::from_waker(Waker::noop());

        assert!(matches!(
            Pin::as_mut(&mut processing).poll(&mut context),
            Poll::Pending
        ));
        drop(processing);

        assert_eq!(gate.state(), &State::Closed);
        assert_eq!(gate.context().callback_steps.get(), 2);
    }
}

mod unhandled {
    use rfsm::{ProcessError, machine};

    struct Facts;

    machine! {
        name: Guarded,
        context: Facts,
        states: { *Waiting },
        events: { Try },
        transitions: {
            Accepted: Waiting + Try [allowed] => Waiting,
        }
    }

    impl GuardedContext for Facts {
        fn allowed(&self) -> bool {
            false
        }
    }

    #[test]
    fn failed_guard_without_fallback_is_unhandled_and_preserves_state() {
        let mut machine = Guarded::new(Facts);

        let unhandled = machine.process(Event::Try);

        assert_eq!(
            unhandled,
            Err(ProcessError::Unhandled {
                state: StateId::Waiting,
                event: Event::Try,
            })
        );
        assert_eq!(machine.state(), &State::Waiting);
    }
}

mod durable_state {
    use rfsm::machine;

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Effect {
        Audit(String),
    }

    struct Facts;

    machine! {
        name: Approval,
        context: Facts,
        effect: Effect,
        states: { *Pending, Approved { reference: String } },
        events: { Approve { reference: String } },
        transitions: {
            Approved: Pending + Approve { reference } / audit(reference)
                => Approved { reference },
            Approved { .. } + Approve { .. } => reject Invalid,
        }
    }

    impl ApprovalContext for Facts {
        fn audit(&self, reference: &String) -> Effect {
            Effect::Audit(reference.clone())
        }
    }

    #[test]
    fn evaluation_leaves_durable_state_unchanged_until_caller_confirmation() {
        let row_state = State::Pending;
        let event = Event::Approve {
            reference: "approval-42".to_owned(),
        };

        let plan = Approval::evaluate(&row_state, &event, &Facts)
            .unwrap_or_else(|failure| panic!("unexpected failure: {failure}"));

        assert_eq!(row_state, State::Pending);
        assert_eq!(
            plan.to,
            State::Approved {
                reference: "approval-42".to_owned()
            }
        );
        assert_eq!(plan.effect, Some(Effect::Audit("approval-42".to_owned())));

        let applied = plan.confirm();
        assert_eq!(applied.transition, Transition::Approved);
    }
}
