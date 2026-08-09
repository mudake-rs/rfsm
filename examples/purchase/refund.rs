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
