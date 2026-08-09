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

    states: { *Unbound, Bound, Refunded },
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
        Unbound + RenewalSnapshot { .. } => reject UnknownChain,
        Unbound + RefundObserved { .. } => reject UnknownChain,
        Unbound + ReversalObserved { .. } => reject UnknownChain,

        DuplicatePaid: _ + Paid { incoming_transaction_id, .. }
            [is_head(incoming_transaction_id)] => _,
        _ + Paid { incoming_transaction_id, incoming_purchased_at, .. }
            [ambiguous_head(incoming_transaction_id, incoming_purchased_at)]
            => reject AmbiguousHead,
        IgnoredHistoricalPaid: _ + Paid { incoming_purchased_at, .. }
            [older_head(incoming_purchased_at)] => _,
        AdvancedHead: _ + Paid {
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

        IgnoredRetainedSnapshot: _ + RenewalSnapshot { incoming_transaction_id, .. }
            [retained_predecessor(incoming_transaction_id)] => _,
        _ + RenewalSnapshot { incoming_transaction_id, .. }
            [not_head(incoming_transaction_id)] => reject DetachedSnapshot,
        DuplicateSnapshot: _ + RenewalSnapshot {
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
        _ + RenewalSnapshot { incoming_signed_at, .. }
            [same_snapshot_clock(incoming_signed_at)] => reject ConflictingSnapshot,
        AppliedSnapshot: _ + RenewalSnapshot {
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
        IgnoredStaleSnapshot: _ + RenewalSnapshot { .. } => _,

        IgnoredSupersededRefund: _ + RefundObserved { incoming_transaction_id, .. }
            [not_head(incoming_transaction_id)] => _,
        RefundedHead: Bound + RefundObserved { incoming_transaction_id }
            [is_head(incoming_transaction_id)] => Refunded,
        DuplicateHeadRefund: Refunded + RefundObserved { incoming_transaction_id, .. }
            [is_head(incoming_transaction_id)] => _,

        IgnoredSupersededReversal: _ + ReversalObserved { incoming_transaction_id }
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

#[cfg(test)]
mod tests {
    use rfsm::ProcessError;

    use super::*;
    use crate::catalogue::{MINER_CURRENT, MINER_RETIRED};

    fn tx(value: &str) -> TransactionId {
        TransactionId::new(value)
    }

    fn head(transaction: &str, purchased_at: u64, paid_until: u64) -> Head {
        Head {
            transaction_id: tx(transaction),
            product_id: MINER_CURRENT,
            purchased_at: Timestamp(purchased_at),
            paid_until: Timestamp(paid_until),
        }
    }

    fn paid(transaction: &str, purchased_at: u64, paid_until: u64) -> Event {
        Event::Paid {
            incoming_transaction_id: tx(transaction),
            incoming_product_id: MINER_CURRENT,
            incoming_purchased_at: Timestamp(purchased_at),
            incoming_paid_until: Timestamp(paid_until),
        }
    }

    #[test]
    fn later_paid_transaction_advances_head_and_equal_date_is_ambiguous() {
        let current = head("t1", 10, 20);
        let mut upgraded = MinerChain::from_state(
            State::Bound,
            Rules::new(Some(current.clone()), None, vec![tx("t1")]),
        );
        let applied = upgraded
            .process(paid("t2", 11, 30))
            .unwrap_or_else(|failure| panic!("unexpected upgrade failure: {failure}"));
        assert!(matches!(applied.effect, Some(Effect::AdvanceHead(_))));
        assert_eq!(upgraded.state(), &State::Bound);

        let mut ambiguous = MinerChain::from_state(
            State::Bound,
            Rules::new(Some(current), None, vec![tx("t1")]),
        );
        assert_eq!(
            ambiguous.process(paid("t2", 10, 30)),
            Err(ProcessError::Rejected(Rejection::AmbiguousHead))
        );
        assert_eq!(ambiguous.state(), &State::Bound);
    }

    #[test]
    fn renewal_snapshot_belongs_only_to_the_exact_head() {
        let current = head("t2", 20, 30);
        let renewal = Renewal {
            transaction_id: tx("t2"),
            signed_at: Timestamp(25),
            auto_renew_product_id: Some(MINER_RETIRED),
            billing_retry: false,
            grace_until: None,
        };
        let rules = Rules::new(Some(current), Some(renewal), vec![tx("t1"), tx("t2")]);

        let mut retained = MinerChain::from_state(State::Bound, rules.clone());
        let applied = retained
            .process(Event::RenewalSnapshot {
                incoming_transaction_id: tx("t1"),
                incoming_signed_at: Timestamp(26),
                incoming_auto_renew_product_id: Some(MINER_CURRENT),
                incoming_billing_retry: false,
                incoming_grace_until: None,
            })
            .unwrap_or_else(|failure| panic!("unexpected retained failure: {failure}"));
        assert_eq!(applied.transition, Transition::IgnoredRetainedSnapshot);

        let mut detached = MinerChain::from_state(State::Bound, rules);
        assert_eq!(
            detached.process(Event::RenewalSnapshot {
                incoming_transaction_id: tx("unknown"),
                incoming_signed_at: Timestamp(26),
                incoming_auto_renew_product_id: Some(MINER_CURRENT),
                incoming_billing_retry: false,
                incoming_grace_until: None,
            }),
            Err(ProcessError::Rejected(Rejection::DetachedSnapshot))
        );
    }

    #[test]
    fn renewal_clock_distinguishes_duplicate_stale_conflict_and_newer() {
        let current = head("t2", 20, 30);
        let renewal = Renewal {
            transaction_id: tx("t2"),
            signed_at: Timestamp(25),
            auto_renew_product_id: Some(MINER_RETIRED),
            billing_retry: false,
            grace_until: None,
        };
        let rules = Rules::new(Some(current), Some(renewal), vec![tx("t1"), tx("t2")]);
        let event = |signed_at, target| Event::RenewalSnapshot {
            incoming_transaction_id: tx("t2"),
            incoming_signed_at: Timestamp(signed_at),
            incoming_auto_renew_product_id: target,
            incoming_billing_retry: false,
            incoming_grace_until: None,
        };

        let mut duplicate = MinerChain::from_state(State::Bound, rules.clone());
        let applied = duplicate
            .process(event(25, Some(MINER_RETIRED)))
            .unwrap_or_else(|failure| panic!("unexpected duplicate failure: {failure}"));
        assert_eq!(applied.transition, Transition::DuplicateSnapshot);
        assert_eq!(applied.effect, None);

        let mut stale = MinerChain::from_state(State::Bound, rules.clone());
        let applied = stale
            .process(event(24, Some(MINER_CURRENT)))
            .unwrap_or_else(|failure| panic!("unexpected stale failure: {failure}"));
        assert_eq!(applied.transition, Transition::IgnoredStaleSnapshot);
        assert_eq!(applied.effect, None);

        let mut conflict = MinerChain::from_state(State::Bound, rules.clone());
        assert_eq!(
            conflict.process(event(25, Some(MINER_CURRENT))),
            Err(ProcessError::Rejected(Rejection::ConflictingSnapshot))
        );
        assert_eq!(conflict.state(), &State::Bound);

        let mut newer = MinerChain::from_state(State::Bound, rules);
        let applied = newer
            .process(event(26, None))
            .unwrap_or_else(|failure| panic!("unexpected newer failure: {failure}"));
        assert!(matches!(
            applied.effect,
            Some(Effect::ReplaceRenewal(Renewal {
                signed_at: Timestamp(26),
                auto_renew_product_id: None,
                ..
            }))
        ));
    }

    #[test]
    fn refund_and_reversal_keep_the_same_authoritative_head() {
        let current = head("t2", 20, 40);
        let rules = Rules::new(Some(current.clone()), None, vec![tx("t1"), tx("t2")]);
        let mut machine = MinerChain::from_state(State::Bound, rules);

        let _ = machine
            .process(Event::RefundObserved {
                incoming_transaction_id: tx("t2"),
            })
            .unwrap_or_else(|failure| panic!("unexpected refund failure: {failure}"));
        assert_eq!(machine.state(), &State::Refunded);
        assert_eq!(machine.context().head.as_ref(), Some(&current));
        assert!(!has_access(
            machine.state(),
            machine.context().head.as_ref(),
            machine.context().renewal.as_ref(),
            Timestamp(26),
        ));

        let _ = machine
            .process(Event::ReversalObserved {
                incoming_transaction_id: tx("t2"),
            })
            .unwrap_or_else(|failure| panic!("unexpected reversal failure: {failure}"));
        assert_eq!(machine.state(), &State::Bound);
        assert_eq!(machine.context().head.as_ref(), Some(&current));
        assert!(has_access(
            machine.state(),
            machine.context().head.as_ref(),
            machine.context().renewal.as_ref(),
            Timestamp(26),
        ));
    }

    #[test]
    fn superseded_refund_is_audit_only() {
        let current = head("t2", 20, 40);
        let mut machine = MinerChain::from_state(
            State::Bound,
            Rules::new(Some(current.clone()), None, vec![tx("t1"), tx("t2")]),
        );
        let applied = machine
            .process(Event::RefundObserved {
                incoming_transaction_id: tx("t1"),
            })
            .unwrap_or_else(|failure| panic!("unexpected refund failure: {failure}"));

        assert_eq!(machine.state(), &State::Bound);
        assert_eq!(machine.context().head.as_ref(), Some(&current));
        assert_eq!(applied.transition, Transition::IgnoredSupersededRefund);
        assert_eq!(applied.effect, None);
    }

    #[test]
    fn grace_and_pending_product_are_derived_from_the_snapshot() {
        let current = head("t2", 20, 30);
        let renewal = Renewal {
            transaction_id: tx("t2"),
            signed_at: Timestamp(25),
            auto_renew_product_id: Some(MINER_RETIRED),
            billing_retry: true,
            grace_until: Some(Timestamp(35)),
        };
        let machine = MinerChain::from_state(
            State::Bound,
            Rules::new(Some(current), Some(renewal), vec![tx("t2")]),
        );

        assert!(has_access(
            machine.state(),
            machine.context().head.as_ref(),
            machine.context().renewal.as_ref(),
            Timestamp(34),
        ));
        assert!(!has_access(
            machine.state(),
            machine.context().head.as_ref(),
            machine.context().renewal.as_ref(),
            Timestamp(35),
        ));
        assert_eq!(
            pending_product(
                machine.context().head.as_ref(),
                machine.context().renewal.as_ref(),
            ),
            Some(MINER_RETIRED)
        );
    }
}
