use rfsm::machine;

use crate::verify::{RefundPercentage, Timestamp};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefundPayload {
    pub revoked_at: Timestamp,
    pub percentage: RefundPercentage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Revision {
    pub signed_at: Timestamp,
    pub refund: Option<RefundPayload>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    Persist(Revision),
    AuditConsumption { request_id: u64 },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Rules {
    current: Option<Revision>,
}

impl Rules {
    pub const fn new(current: Option<Revision>) -> Self {
        Self { current }
    }
}

machine! {
    name: Refunds,
    context: Rules,
    effect: Effect,

    states: {
        *None,
        Requested,
        Refunded,
        Declined,
        Reversed,
        LegacyRevoked,
    },
    events: {
        Refund {
            incoming_signed_at: Timestamp,
            incoming_revoked_at: Timestamp,
            incoming_percentage: RefundPercentage,
        },
        Decline { incoming_signed_at: Timestamp },
        Reversal { incoming_signed_at: Timestamp },
        ActiveRecovered { incoming_signed_at: Timestamp },
        ConsumptionRequested { request_id: u64 },
    },

    transitions: {
        LegacyRevoked + Refund { .. } => reject ManualRepairRequired,
        LegacyRevoked + Decline { .. } => reject ManualRepairRequired,
        LegacyRevoked + Reversal { .. } => reject ManualRepairRequired,
        LegacyRevoked + ActiveRecovered { .. } => reject ManualRepairRequired,
        LegacyRevoked + ConsumptionRequested { .. } => reject ManualRepairRequired,

        DuplicateRefund: Refunded + Refund {
            incoming_signed_at,
            incoming_revoked_at,
            incoming_percentage,
        } [same_refund(incoming_signed_at, incoming_revoked_at, incoming_percentage)] => _,
        DuplicateDecline: Declined + Decline { incoming_signed_at }
            [same_clear(incoming_signed_at)] => _,
        DuplicateReversal: Reversed + Reversal { incoming_signed_at }
            [same_clear(incoming_signed_at)] => _,
        DuplicateActiveNone: None + ActiveRecovered { incoming_signed_at }
            [same_clear(incoming_signed_at)] => _,
        DuplicateActiveRequested: Requested + ActiveRecovered { incoming_signed_at }
            [same_clear(incoming_signed_at)] => _,
        DuplicateActiveDeclined: Declined + ActiveRecovered { incoming_signed_at }
            [same_clear(incoming_signed_at)] => _,
        DuplicateActiveReversed: Reversed + ActiveRecovered { incoming_signed_at }
            [same_clear(incoming_signed_at)] => _,

        Refunded + Decline { incoming_signed_at }
            [same_clock(incoming_signed_at)] => reject ConflictingRevision,
        Refunded + Decline { incoming_signed_at }
            [newer(incoming_signed_at)] => reject DeclineAfterRefund,

        RefundedRecovered: Refunded + ActiveRecovered { incoming_signed_at }
            [newer(incoming_signed_at)] / persist_clear(incoming_signed_at) => Reversed,

        ConsumedFromNone: None + ConsumptionRequested { request_id }
            / audit_consumption(request_id) => Requested,

        _ + Refund { incoming_signed_at, .. }
            [same_clock(incoming_signed_at)] => reject ConflictingRevision,
        AppliedRefund: _ + Refund {
            incoming_signed_at,
            incoming_revoked_at,
            incoming_percentage,
        } [newer(incoming_signed_at)]
            / persist_refund(incoming_signed_at, incoming_revoked_at, incoming_percentage)
            => Refunded,
        _ + Refund { .. } => reject StaleRevision,

        _ + Decline { incoming_signed_at }
            [same_clock(incoming_signed_at)] => reject ConflictingRevision,
        AppliedDecline: _ + Decline { incoming_signed_at }
            [newer(incoming_signed_at)] / persist_clear(incoming_signed_at) => Declined,
        _ + Decline { .. } => reject StaleRevision,

        _ + Reversal { incoming_signed_at }
            [same_clock(incoming_signed_at)] => reject ConflictingRevision,
        AppliedReversal: _ + Reversal { incoming_signed_at }
            [newer(incoming_signed_at)] / persist_clear(incoming_signed_at) => Reversed,
        _ + Reversal { .. } => reject StaleRevision,

        _ + ActiveRecovered { incoming_signed_at }
            [same_clock(incoming_signed_at)] => reject ConflictingRevision,
        AppliedActiveRecovery: _ + ActiveRecovered { incoming_signed_at }
            [newer(incoming_signed_at)] / persist_clear(incoming_signed_at) => _,
        _ + ActiveRecovered { .. } => reject StaleRevision,

        AuditedConsumption: _ + ConsumptionRequested { request_id }
            / audit_consumption(request_id) => _,
    }
}

impl RefundsContext for Rules {
    fn same_refund(
        &self,
        incoming_signed_at: &Timestamp,
        incoming_revoked_at: &Timestamp,
        incoming_percentage: &RefundPercentage,
    ) -> bool {
        self.current
            == Some(Revision {
                signed_at: *incoming_signed_at,
                refund: Some(RefundPayload {
                    revoked_at: *incoming_revoked_at,
                    percentage: *incoming_percentage,
                }),
            })
    }

    fn same_clock(&self, incoming_signed_at: &Timestamp) -> bool {
        self.current
            .is_some_and(|current| current.signed_at == *incoming_signed_at)
    }

    fn same_clear(&self, incoming_signed_at: &Timestamp) -> bool {
        self.current
            == Some(Revision {
                signed_at: *incoming_signed_at,
                refund: None,
            })
    }

    fn newer(&self, incoming_signed_at: &Timestamp) -> bool {
        self.current
            .is_none_or(|current| *incoming_signed_at > current.signed_at)
    }

    fn persist_refund(
        &self,
        incoming_signed_at: &Timestamp,
        incoming_revoked_at: &Timestamp,
        incoming_percentage: &RefundPercentage,
    ) -> Effect {
        Effect::Persist(Revision {
            signed_at: *incoming_signed_at,
            refund: Some(RefundPayload {
                revoked_at: *incoming_revoked_at,
                percentage: *incoming_percentage,
            }),
        })
    }

    fn persist_clear(&self, incoming_signed_at: &Timestamp) -> Effect {
        Effect::Persist(Revision {
            signed_at: *incoming_signed_at,
            refund: None,
        })
    }

    fn audit_consumption(&self, request_id: &u64) -> Effect {
        Effect::AuditConsumption {
            request_id: *request_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use rfsm::ProcessError;

    use super::*;

    fn refund(at: u64, revoked_at: u64, percentage: u32) -> Event {
        Event::Refund {
            incoming_signed_at: Timestamp(at),
            incoming_revoked_at: Timestamp(revoked_at),
            incoming_percentage: RefundPercentage::new(percentage)
                .unwrap_or_else(|failure| panic!("invalid test percentage: {failure}")),
        }
    }

    fn current_refund(at: u64, revoked_at: u64, percentage: u32) -> Revision {
        Revision {
            signed_at: Timestamp(at),
            refund: Some(RefundPayload {
                revoked_at: Timestamp(revoked_at),
                percentage: RefundPercentage::new(percentage)
                    .unwrap_or_else(|failure| panic!("invalid test percentage: {failure}")),
            }),
        }
    }

    #[test]
    fn signed_clock_classifies_newer_stale_duplicate_and_conflict() {
        let current = current_refund(20, 15, 50_000);

        let mut newer = Refunds::from_state(State::Refunded, Rules::new(Some(current)));
        let applied = newer
            .process(refund(21, 16, 100_000))
            .unwrap_or_else(|failure| panic!("unexpected transition failure: {failure}"));
        assert_eq!(newer.state(), &State::Refunded);
        assert!(matches!(applied.effect, Some(Effect::Persist(_))));

        let mut stale = Refunds::from_state(State::Refunded, Rules::new(Some(current)));
        assert_eq!(
            stale.process(refund(19, 14, 40_000)),
            Err(ProcessError::Rejected(Rejection::StaleRevision))
        );
        assert_eq!(stale.state(), &State::Refunded);

        let mut duplicate = Refunds::from_state(State::Refunded, Rules::new(Some(current)));
        let applied = duplicate
            .process(refund(20, 15, 50_000))
            .unwrap_or_else(|failure| panic!("unexpected duplicate failure: {failure}"));
        assert_eq!(applied.transition, Transition::DuplicateRefund);
        assert_eq!(applied.effect, None);

        let mut conflict = Refunds::from_state(State::Refunded, Rules::new(Some(current)));
        assert_eq!(
            conflict.process(refund(20, 15, 60_000)),
            Err(ProcessError::Rejected(Rejection::ConflictingRevision))
        );
        assert_eq!(conflict.state(), &State::Refunded);
    }

    #[test]
    fn declined_cannot_replace_a_newer_completed_refund() {
        let current = current_refund(20, 15, 100_000);
        let mut machine = Refunds::from_state(State::Refunded, Rules::new(Some(current)));

        assert_eq!(
            machine.process(Event::Decline {
                incoming_signed_at: Timestamp(21),
            }),
            Err(ProcessError::Rejected(Rejection::DeclineAfterRefund))
        );
        assert_eq!(machine.state(), &State::Refunded);
    }

    #[test]
    fn decline_resolves_a_pending_request_and_persists_its_clock() {
        let mut machine = Refunds::from_state(State::Requested, Rules::default());

        let applied = machine
            .process(Event::Decline {
                incoming_signed_at: Timestamp(21),
            })
            .unwrap_or_else(|failure| panic!("unexpected decline failure: {failure}"));

        assert_eq!(machine.state(), &State::Declined);
        assert_eq!(applied.transition, Transition::AppliedDecline);
        assert_eq!(
            applied.effect,
            Some(Effect::Persist(Revision {
                signed_at: Timestamp(21),
                refund: None,
            }))
        );
    }

    #[test]
    fn consumption_is_always_audited_and_only_none_becomes_requested() {
        let mut none = Refunds::new(Rules::default());
        let applied = none
            .process(Event::ConsumptionRequested { request_id: 7 })
            .unwrap_or_else(|failure| panic!("unexpected request failure: {failure}"));
        assert_eq!(none.state(), &State::Requested);
        assert_eq!(
            applied.effect,
            Some(Effect::AuditConsumption { request_id: 7 })
        );

        let revision = Revision {
            signed_at: Timestamp(20),
            refund: None,
        };
        let mut declined = Refunds::from_state(State::Declined, Rules::new(Some(revision)));
        let applied = declined
            .process(Event::ConsumptionRequested { request_id: 8 })
            .unwrap_or_else(|failure| panic!("unexpected request failure: {failure}"));
        assert_eq!(declined.state(), &State::Declined);
        assert_eq!(declined.context().current, Some(revision));
        assert_eq!(
            applied.effect,
            Some(Effect::AuditConsumption { request_id: 8 })
        );
    }

    #[test]
    fn active_recovery_maps_only_refunded_to_reversed() {
        let current = current_refund(20, 15, 100_000);
        let mut refunded = Refunds::from_state(State::Refunded, Rules::new(Some(current)));
        let applied = refunded
            .process(Event::ActiveRecovered {
                incoming_signed_at: Timestamp(21),
            })
            .unwrap_or_else(|failure| panic!("unexpected recovery failure: {failure}"));
        assert_eq!(refunded.state(), &State::Reversed);
        assert_eq!(
            applied.effect,
            Some(Effect::Persist(Revision {
                signed_at: Timestamp(21),
                refund: None,
            }))
        );

        let declined = Revision {
            signed_at: Timestamp(20),
            refund: None,
        };
        let mut active = Refunds::from_state(State::Declined, Rules::new(Some(declined)));
        let applied = active
            .process(Event::ActiveRecovered {
                incoming_signed_at: Timestamp(21),
            })
            .unwrap_or_else(|failure| panic!("unexpected recovery failure: {failure}"));
        assert_eq!(active.state(), &State::Declined);
        assert_eq!(
            applied.effect,
            Some(Effect::Persist(Revision {
                signed_at: Timestamp(21),
                refund: None,
            }))
        );

        for state in [State::None, State::Requested, State::Reversed] {
            let current = Revision {
                signed_at: Timestamp(20),
                refund: None,
            };
            let expected = state.clone();
            let mut machine = Refunds::from_state(state, Rules::new(Some(current)));
            let applied = machine
                .process(Event::ActiveRecovered {
                    incoming_signed_at: Timestamp(21),
                })
                .unwrap_or_else(|failure| panic!("unexpected recovery failure: {failure}"));
            assert_eq!(machine.state(), &expected);
            assert_eq!(
                applied.effect,
                Some(Effect::Persist(Revision {
                    signed_at: Timestamp(21),
                    refund: None,
                }))
            );
        }
    }

    #[test]
    fn equal_clock_replays_the_same_durable_projection_across_sources() {
        let clear = Revision {
            signed_at: Timestamp(21),
            refund: None,
        };

        let mut declined_from_recovery =
            Refunds::from_state(State::Declined, Rules::new(Some(clear)));
        let applied = declined_from_recovery
            .process(Event::ActiveRecovered {
                incoming_signed_at: Timestamp(21),
            })
            .unwrap_or_else(|failure| panic!("unexpected duplicate failure: {failure}"));
        assert_eq!(applied.transition, Transition::DuplicateActiveDeclined);
        assert_eq!(applied.effect, None);

        let mut declined_from_notification =
            Refunds::from_state(State::Declined, Rules::new(Some(clear)));
        let applied = declined_from_notification
            .process(Event::Decline {
                incoming_signed_at: Timestamp(21),
            })
            .unwrap_or_else(|failure| panic!("unexpected duplicate failure: {failure}"));
        assert_eq!(applied.transition, Transition::DuplicateDecline);
        assert_eq!(applied.effect, None);

        let mut reversed_from_recovery =
            Refunds::from_state(State::Reversed, Rules::new(Some(clear)));
        let applied = reversed_from_recovery
            .process(Event::ActiveRecovered {
                incoming_signed_at: Timestamp(21),
            })
            .unwrap_or_else(|failure| panic!("unexpected duplicate failure: {failure}"));
        assert_eq!(applied.transition, Transition::DuplicateActiveReversed);
        assert_eq!(applied.effect, None);

        let mut reversed_from_notification =
            Refunds::from_state(State::Reversed, Rules::new(Some(clear)));
        let applied = reversed_from_notification
            .process(Event::Reversal {
                incoming_signed_at: Timestamp(21),
            })
            .unwrap_or_else(|failure| panic!("unexpected duplicate failure: {failure}"));
        assert_eq!(applied.transition, Transition::DuplicateReversal);
        assert_eq!(applied.effect, None);
    }

    #[test]
    fn legacy_revoked_state_requires_manual_repair() {
        let mut machine = Refunds::from_state(State::LegacyRevoked, Rules::default());
        assert_eq!(
            machine.process(Event::Reversal {
                incoming_signed_at: Timestamp(21),
            }),
            Err(ProcessError::Rejected(Rejection::ManualRepairRequired))
        );
        assert_eq!(machine.state(), &State::LegacyRevoked);
    }
}
