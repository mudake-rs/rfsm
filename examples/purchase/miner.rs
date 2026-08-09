use rfsm::machine;

use crate::catalogue::ProductId;
use crate::verify::{Timestamp, TransactionId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Head {
    pub transaction_id: TransactionId,
    pub product_id: ProductId,
    pub purchased_at: Timestamp,
    pub paid_until: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Renewal {
    pub transaction_id: TransactionId,
    pub signed_at: Timestamp,
    pub auto_renew_product_id: Option<ProductId>,
    pub billing_retry: bool,
    pub grace_until: Option<Timestamp>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    AdvanceHead(Head),
    ReplaceRenewal(Renewal),
}

#[derive(Clone, Debug, Default)]
pub struct Rules {
    head: Option<Head>,
    renewal: Option<Renewal>,
    retained_transactions: Vec<TransactionId>,
}

impl Rules {
    pub fn new(
        head: Option<Head>,
        renewal: Option<Renewal>,
        retained_transactions: Vec<TransactionId>,
    ) -> Self {
        Self {
            head,
            renewal,
            retained_transactions,
        }
    }
}

machine! {
    name: MinerChain,
    context: Rules,
    effect: Effect,

    states: {
        *Unbound,
        Tracking {
            *Bound,
            Refunded,
        },
    },
    events: {
        Paid {
            incoming_transaction_id: TransactionId,
            incoming_product_id: ProductId,
            incoming_purchased_at: Timestamp,
            incoming_paid_until: Timestamp,
        },
        RenewalSnapshot {
            incoming_transaction_id: TransactionId,
            incoming_signed_at: Timestamp,
            incoming_auto_renew_product_id: Option<ProductId>,
            incoming_billing_retry: bool,
            incoming_grace_until: Option<Timestamp>,
        },
        RefundObserved { incoming_transaction_id: TransactionId },
        ReversalObserved { incoming_transaction_id: TransactionId },
    },

    transitions: {
        BoundInitialHead: Unbound + Paid {
            incoming_transaction_id,
            incoming_product_id,
            incoming_purchased_at,
            incoming_paid_until,
        } / advance_head(
            incoming_transaction_id,
            incoming_product_id,
            incoming_purchased_at,
            incoming_paid_until,
        ) => Bound,
        Unbound + _ => reject UnknownChain,

        DuplicatePaid: Tracking + Paid { incoming_transaction_id, .. }
            [is_head(incoming_transaction_id)] => _,
        Tracking + Paid { incoming_transaction_id, incoming_purchased_at, .. }
            [ambiguous_head(incoming_transaction_id, incoming_purchased_at)]
            => reject AmbiguousHead,
        IgnoredHistoricalPaid: Tracking + Paid { incoming_purchased_at, .. }
            [older_head(incoming_purchased_at)] => _,
        AdvancedHead: Tracking + Paid {
            incoming_transaction_id,
            incoming_product_id,
            incoming_purchased_at,
            incoming_paid_until,
        } [newer_head(incoming_purchased_at)]
            / advance_head(
                incoming_transaction_id,
                incoming_product_id,
                incoming_purchased_at,
                incoming_paid_until,
            ) => Bound,

        IgnoredRetainedSnapshot: Tracking + RenewalSnapshot { incoming_transaction_id, .. }
            [retained_predecessor(incoming_transaction_id)] => _,
        Tracking + RenewalSnapshot { incoming_transaction_id, .. }
            [not_head(incoming_transaction_id)] => reject DetachedSnapshot,
        DuplicateSnapshot: Tracking + RenewalSnapshot {
            incoming_transaction_id,
            incoming_signed_at,
            incoming_auto_renew_product_id,
            incoming_billing_retry,
            incoming_grace_until,
        } [same_snapshot(
            incoming_transaction_id,
            incoming_signed_at,
            incoming_auto_renew_product_id,
            incoming_billing_retry,
            incoming_grace_until,
        )] => _,
        Tracking + RenewalSnapshot { incoming_signed_at, .. }
            [same_snapshot_clock(incoming_signed_at)] => reject ConflictingSnapshot,
        AppliedSnapshot: Tracking + RenewalSnapshot {
            incoming_transaction_id,
            incoming_signed_at,
            incoming_auto_renew_product_id,
            incoming_billing_retry,
            incoming_grace_until,
        } [newer_snapshot(incoming_signed_at)]
            / replace_snapshot(
                incoming_transaction_id,
                incoming_signed_at,
                incoming_auto_renew_product_id,
                incoming_billing_retry,
                incoming_grace_until,
            ) => _,
        IgnoredStaleSnapshot: Tracking + RenewalSnapshot { .. } => _,

        IgnoredSupersededRefund: Tracking + RefundObserved { incoming_transaction_id, .. }
            [not_head(incoming_transaction_id)] => _,
        RefundedHead: Bound + RefundObserved { incoming_transaction_id }
            [is_head(incoming_transaction_id)] => Refunded,
        DuplicateHeadRefund: Refunded + RefundObserved { incoming_transaction_id, .. }
            [is_head(incoming_transaction_id)] => _,

        IgnoredSupersededReversal: Tracking + ReversalObserved { incoming_transaction_id }
            [not_head(incoming_transaction_id)] => _,
        RestoredHead: Refunded + ReversalObserved { incoming_transaction_id }
            [is_head(incoming_transaction_id)] => Bound,
        DuplicateHeadReversal: Bound + ReversalObserved { incoming_transaction_id }
            [is_head(incoming_transaction_id)] => _,
    }
}

impl MinerChainContext for Rules {
    fn is_head(&self, incoming_transaction_id: &TransactionId) -> bool {
        self.head
            .as_ref()
            .is_some_and(|head| head.transaction_id == *incoming_transaction_id)
    }

    fn not_head(&self, incoming_transaction_id: &TransactionId) -> bool {
        !self.is_head(incoming_transaction_id)
    }

    fn ambiguous_head(
        &self,
        incoming_transaction_id: &TransactionId,
        incoming_purchased_at: &Timestamp,
    ) -> bool {
        self.head.as_ref().is_some_and(|head| {
            head.transaction_id != *incoming_transaction_id
                && head.purchased_at == *incoming_purchased_at
        })
    }

    fn older_head(&self, incoming_purchased_at: &Timestamp) -> bool {
        self.head
            .as_ref()
            .is_some_and(|head| *incoming_purchased_at < head.purchased_at)
    }

    fn newer_head(&self, incoming_purchased_at: &Timestamp) -> bool {
        self.head
            .as_ref()
            .is_some_and(|head| *incoming_purchased_at > head.purchased_at)
    }

    fn retained_predecessor(&self, incoming_transaction_id: &TransactionId) -> bool {
        !self.is_head(incoming_transaction_id)
            && self.retained_transactions.contains(incoming_transaction_id)
    }

    fn same_snapshot(
        &self,
        incoming_transaction_id: &TransactionId,
        incoming_signed_at: &Timestamp,
        incoming_auto_renew_product_id: &Option<ProductId>,
        incoming_billing_retry: &bool,
        incoming_grace_until: &Option<Timestamp>,
    ) -> bool {
        self.renewal.as_ref()
            == Some(&Renewal {
                transaction_id: incoming_transaction_id.clone(),
                signed_at: *incoming_signed_at,
                auto_renew_product_id: *incoming_auto_renew_product_id,
                billing_retry: *incoming_billing_retry,
                grace_until: *incoming_grace_until,
            })
    }

    fn same_snapshot_clock(&self, incoming_signed_at: &Timestamp) -> bool {
        self.renewal
            .as_ref()
            .is_some_and(|renewal| renewal.signed_at == *incoming_signed_at)
    }

    fn newer_snapshot(&self, incoming_signed_at: &Timestamp) -> bool {
        self.renewal
            .as_ref()
            .is_none_or(|renewal| *incoming_signed_at > renewal.signed_at)
    }

    fn advance_head(
        &self,
        incoming_transaction_id: &TransactionId,
        incoming_product_id: &ProductId,
        incoming_purchased_at: &Timestamp,
        incoming_paid_until: &Timestamp,
    ) -> Effect {
        Effect::AdvanceHead(Head {
            transaction_id: incoming_transaction_id.clone(),
            product_id: *incoming_product_id,
            purchased_at: *incoming_purchased_at,
            paid_until: *incoming_paid_until,
        })
    }

    fn replace_snapshot(
        &self,
        incoming_transaction_id: &TransactionId,
        incoming_signed_at: &Timestamp,
        incoming_auto_renew_product_id: &Option<ProductId>,
        incoming_billing_retry: &bool,
        incoming_grace_until: &Option<Timestamp>,
    ) -> Effect {
        Effect::ReplaceRenewal(Renewal {
            transaction_id: incoming_transaction_id.clone(),
            signed_at: *incoming_signed_at,
            auto_renew_product_id: *incoming_auto_renew_product_id,
            billing_retry: *incoming_billing_retry,
            grace_until: *incoming_grace_until,
        })
    }
}

fn effective_until(head: Option<&Head>, renewal: Option<&Renewal>) -> Option<Timestamp> {
    let paid = head?.paid_until;
    Some(
        renewal
            .and_then(|renewal| renewal.grace_until)
            .map_or(paid, |grace| paid.max(grace)),
    )
}

pub fn has_access(
    state: &State,
    head: Option<&Head>,
    renewal: Option<&Renewal>,
    now: Timestamp,
) -> bool {
    state == &State::Bound && effective_until(head, renewal).is_some_and(|until| until > now)
}

pub fn pending_product(head: Option<&Head>, renewal: Option<&Renewal>) -> Option<ProductId> {
    let current = head?.product_id;
    renewal?
        .auto_renew_product_id
        .filter(|target| *target != current)
}
