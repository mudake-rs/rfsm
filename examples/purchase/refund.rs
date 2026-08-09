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
        *Live {
            *Effective {
                *None,
                Requested,
                Declined,
                Reversed,
            },
            Refunded,
        },
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
        ConsumptionRequested,
    },

    transitions: {
        LegacyRevoked + _ => reject ManualRepairRequired,

        DuplicateRefund: Refunded + Refund {
            incoming_signed_at,
            incoming_revoked_at,
            incoming_percentage,
        } [same_refund(incoming_signed_at, incoming_revoked_at, incoming_percentage)] => _,
        DuplicateDecline: Declined + Decline { incoming_signed_at }
            [same_clear(incoming_signed_at)] => _,
        DuplicateReversal: Reversed + Reversal { incoming_signed_at }
            [same_clear(incoming_signed_at)] => _,
        DuplicateActiveRecovery: Effective + ActiveRecovered { incoming_signed_at }
            [same_clear(incoming_signed_at)] => _,

        Refunded + Decline { incoming_signed_at }
            [same_clock(incoming_signed_at)] => reject ConflictingRevision,
        Refunded + Decline { incoming_signed_at }
            [newer(incoming_signed_at)] => reject DeclineAfterRefund,
        Refunded + Decline { .. } => reject StaleRevision,

        RefundedRecovered: Refunded + ActiveRecovered { incoming_signed_at }
            [newer(incoming_signed_at)] / persist_clear(incoming_signed_at) => Reversed,

        ConsumptionMarked: None + ConsumptionRequested => Requested,

        Live + Refund { incoming_signed_at, .. }
            [same_clock(incoming_signed_at)] => reject ConflictingRevision,
        AppliedRefund: Live + Refund {
            incoming_signed_at,
            incoming_revoked_at,
            incoming_percentage,
        } [newer(incoming_signed_at)]
            / persist_refund(incoming_signed_at, incoming_revoked_at, incoming_percentage)
            => Refunded,
        Live + Refund { .. } => reject StaleRevision,

        Effective + Decline { incoming_signed_at }
            [same_clock(incoming_signed_at)] => reject ConflictingRevision,
        AppliedDecline: Effective + Decline { incoming_signed_at }
            [newer(incoming_signed_at)] / persist_clear(incoming_signed_at) => Declined,
        Effective + Decline { .. } => reject StaleRevision,

        Live + Reversal { incoming_signed_at }
            [same_clock(incoming_signed_at)] => reject ConflictingRevision,
        AppliedReversal: Live + Reversal { incoming_signed_at }
            [newer(incoming_signed_at)] / persist_clear(incoming_signed_at) => Reversed,
        Live + Reversal { .. } => reject StaleRevision,

        Live + ActiveRecovered { incoming_signed_at }
            [same_clock(incoming_signed_at)] => reject ConflictingRevision,
        AppliedActiveRecovery: Effective + ActiveRecovered { incoming_signed_at }
            [newer(incoming_signed_at)] / persist_clear(incoming_signed_at) => _,
        Live + ActiveRecovered { .. } => reject StaleRevision,

        ConsumptionUnchanged: Live + ConsumptionRequested => _,
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
}
