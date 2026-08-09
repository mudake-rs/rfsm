use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};

use rfsm::{Applied, ProcessError};

use crate::diamond;
use crate::miner;
use crate::refund;
use crate::verify::{
    ChainId, Timestamp, TransactionId, VerifiedRefundOutcome, VerifiedRefundRevision,
    VerifiedRenewalSnapshot, VerifiedTransaction,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Transition {
    TransactionAccepted {
        transaction_id: TransactionId,
    },
    Refund(Applied<refund::State, refund::Transition, refund::Effect>),
    Miner(Applied<miner::State, miner::Transition, miner::Effect>),
    Diamond(Applied<diamond::State, diamond::Transition, diamond::Delivery>),
    VipFloorChanged {
        from: Option<Timestamp>,
        to: Option<Timestamp>,
    },
    VipProjectionChanged {
        from: Option<Timestamp>,
        to: Option<Timestamp>,
    },
    Ignored(IgnoreReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IgnoreReason {
    DuplicateTransaction,
    StaleRefundNotification,
    UnchangedVipFloor,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub changed: bool,
    pub transitions: Vec<Transition>,
}

impl Outcome {
    fn one(changed: bool, transition: Transition) -> Self {
        Self {
            changed,
            transitions: vec![transition],
        }
    }

    fn ignored(reason: IgnoreReason) -> Self {
        Self::one(false, Transition::Ignored(reason))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransactionRecord {
    pub transaction: VerifiedTransaction,
    pub refund_state: refund::State,
    pub refund_revision: Option<refund::Revision>,
}

impl TransactionRecord {
    fn new(transaction: VerifiedTransaction) -> Self {
        Self {
            transaction,
            refund_state: refund::State::None,
            refund_revision: None,
        }
    }

    fn process_refund(&mut self, event: refund::Event) -> Result<(bool, Transition), DomainError> {
        let mut machine = refund::Refunds::from_state(
            self.refund_state.clone(),
            refund::Rules::new(self.refund_revision),
        );
        let applied = machine.process(event).map_err(map_refund_error)?;
        let changed = applied.from != applied.to || applied.effect.is_some();
        self.refund_state = applied.to.clone();
        if let Some(refund::Effect::Persist(revision)) = applied.effect {
            self.refund_revision = Some(revision);
        }
        Ok((changed, Transition::Refund(applied)))
    }

    fn request_consumption(&mut self) -> Result<(bool, Transition), DomainError> {
        self.process_refund(refund::Event::ConsumptionRequested)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MinerRecord {
    pub state: miner::State,
    pub head: Option<miner::Head>,
    pub renewal: Option<miner::Renewal>,
}

impl Default for MinerRecord {
    fn default() -> Self {
        Self {
            state: miner::State::Unbound,
            head: None,
            renewal: None,
        }
    }
}

impl MinerRecord {
    fn process(
        &mut self,
        event: miner::Event,
        retained_transactions: Vec<TransactionId>,
    ) -> Result<(bool, Transition), DomainError> {
        let rules = miner::Rules::new(
            self.head.clone(),
            self.renewal.clone(),
            retained_transactions,
        );
        let mut machine = miner::MinerChain::from_state(self.state.clone(), rules);
        let applied = machine.process(event).map_err(map_miner_error)?;
        let changed = applied.from != applied.to || applied.effect.is_some();
        self.state = applied.to.clone();
        if let Some(effect) = &applied.effect {
            match effect {
                miner::Effect::AdvanceHead(head) => {
                    self.head = Some(head.clone());
                    self.renewal = None;
                }
                miner::Effect::ReplaceRenewal(renewal) => {
                    self.renewal = Some(renewal.clone());
                }
            }
        }
        Ok((changed, Transition::Miner(applied)))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiamondRecord {
    pub state: diamond::State,
}

impl Default for DiamondRecord {
    fn default() -> Self {
        Self {
            state: diamond::State::Undelivered,
        }
    }
}

impl DiamondRecord {
    fn deliver(
        &mut self,
        grant: u64,
        refund_percentage: Option<crate::verify::RefundPercentage>,
    ) -> Result<(bool, Transition), DomainError> {
        let mut machine = diamond::Diamonds::from_state(self.state.clone(), diamond::Rules);
        let applied = machine
            .process(diamond::Event::Deliver {
                grant,
                refund_percentage,
            })
            .map_err(|_| DomainError::Invariant("Diamond delivery was unhandled"))?;
        let changed = applied.from != applied.to || applied.effect.is_some();
        self.state = applied.to.clone();
        Ok((changed, Transition::Diamond(applied)))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Transaction(VerifiedTransaction),
    RenewalSnapshot(VerifiedRenewalSnapshot),
    RefundNotification(VerifiedRefundRevision),
    RecoveredStatus(VerifiedRefundRevision),
    ConsumptionRequested { transaction_id: TransactionId },
    VipFloorChanged { expires_at: Option<Timestamp> },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Purchase {
    transactions: HashMap<TransactionId, TransactionRecord>,
    miners: HashMap<ChainId, MinerRecord>,
    diamonds: HashMap<TransactionId, DiamondRecord>,
    vip_floor: Option<Timestamp>,
    vip_expires_at: Option<Timestamp>,
}

impl Purchase {
    pub fn process(&mut self, event: Event) -> Result<Outcome, DomainError> {
        match event {
            Event::Transaction(transaction) => self.apply_transaction(transaction),
            Event::RenewalSnapshot(snapshot) => self.apply_renewal_snapshot(snapshot),
            Event::RefundNotification(revision) => match self.apply_refund(revision, false) {
                Err(DomainError::RefundRejected(refund::Rejection::StaleRevision)) => {
                    Ok(Outcome::ignored(IgnoreReason::StaleRefundNotification))
                }
                result => result,
            },
            Event::RecoveredStatus(revision) => self.apply_refund(revision, true),
            Event::ConsumptionRequested { transaction_id } => {
                self.request_consumption(&transaction_id)
            }
            Event::VipFloorChanged { expires_at } => self.change_vip_floor(expires_at),
        }
    }

    pub fn transaction(&self, id: &TransactionId) -> Option<&TransactionRecord> {
        self.transactions.get(id)
    }

    pub fn miner(&self, id: &ChainId) -> Option<&MinerRecord> {
        self.miners.get(id)
    }

    pub fn diamond(&self, id: &TransactionId) -> Option<&DiamondRecord> {
        self.diamonds.get(id)
    }

    pub const fn vip_expires_at(&self) -> Option<Timestamp> {
        self.vip_expires_at
    }

    fn apply_transaction(
        &mut self,
        transaction: VerifiedTransaction,
    ) -> Result<Outcome, DomainError> {
        let transaction_id = transaction.identity().transaction_id.clone();
        if let Some(existing) = self.transactions.get(&transaction_id) {
            if existing.transaction != transaction {
                return Err(DomainError::ImmutableTransactionConflict);
            }
            return match transaction {
                VerifiedTransaction::Diamonds { amount, .. } => {
                    self.deliver_diamonds(&transaction_id, amount)
                }
                VerifiedTransaction::Miner { .. } | VerifiedTransaction::Vip { .. } => {
                    Ok(Outcome::ignored(IgnoreReason::DuplicateTransaction))
                }
            };
        }

        let mut transitions = Vec::with_capacity(3);
        transitions.push(Transition::TransactionAccepted {
            transaction_id: transaction_id.clone(),
        });
        let next_miner = match &transaction {
            VerifiedTransaction::Miner {
                identity,
                expires_at,
            } => {
                let mut chain = self
                    .miners
                    .get(&identity.chain_id)
                    .cloned()
                    .unwrap_or_default();
                let (_, transition) = chain.process(
                    miner::Event::Paid {
                        incoming_transaction_id: identity.transaction_id.clone(),
                        incoming_product_id: identity.product_id,
                        incoming_purchased_at: identity.purchased_at,
                        incoming_paid_until: *expires_at,
                    },
                    self.retained_transactions(&identity.chain_id),
                )?;
                transitions.push(transition);
                Some((identity.chain_id.clone(), chain))
            }
            VerifiedTransaction::Vip { .. } | VerifiedTransaction::Diamonds { .. } => None,
        };
        let next_diamond = match &transaction {
            VerifiedTransaction::Diamonds { identity, amount } => {
                let mut delivery = DiamondRecord::default();
                let (_, transition) = delivery.deliver(*amount, None)?;
                transitions.push(transition);
                Some((identity.transaction_id.clone(), delivery))
            }
            VerifiedTransaction::Miner { .. } | VerifiedTransaction::Vip { .. } => None,
        };

        if let Some((chain_id, record)) = next_miner {
            self.miners.insert(chain_id, record);
        }
        if let Some((exact_id, record)) = next_diamond {
            self.diamonds.insert(exact_id, record);
        }
        self.transactions
            .insert(transaction_id, TransactionRecord::new(transaction));
        if let Some(transition) = self.recompute_vip() {
            transitions.push(transition);
        }
        Ok(Outcome {
            changed: true,
            transitions,
        })
    }

    fn deliver_diamonds(
        &mut self,
        transaction_id: &TransactionId,
        grant: u64,
    ) -> Result<Outcome, DomainError> {
        let mut record =
            self.diamonds
                .get(transaction_id)
                .cloned()
                .ok_or(DomainError::Invariant(
                    "exact Diamond transaction has no delivery state",
                ))?;
        let percentage = self
            .transactions
            .get(transaction_id)
            .and_then(|record| record.refund_revision)
            .and_then(|revision| revision.refund)
            .map(|payload| payload.percentage);
        let (changed, transition) = record.deliver(grant, percentage)?;
        if changed {
            self.diamonds.insert(transaction_id.clone(), record);
        }
        Ok(Outcome::one(changed, transition))
    }

    fn apply_renewal_snapshot(
        &mut self,
        snapshot: VerifiedRenewalSnapshot,
    ) -> Result<Outcome, DomainError> {
        let mut chain = self
            .miners
            .get(&snapshot.chain_id)
            .cloned()
            .ok_or(DomainError::UnknownChain)?;
        let (changed, transition) = chain.process(
            miner::Event::RenewalSnapshot {
                incoming_transaction_id: snapshot.transaction_id,
                incoming_signed_at: snapshot.signed_at,
                incoming_auto_renew_product_id: snapshot.auto_renew_product_id,
                incoming_billing_retry: snapshot.billing_retry,
                incoming_grace_until: snapshot.grace_until,
            },
            self.retained_transactions(&snapshot.chain_id),
        )?;
        if changed {
            self.miners.insert(snapshot.chain_id, chain);
        }
        Ok(Outcome::one(changed, transition))
    }

    fn apply_refund(
        &mut self,
        revision: VerifiedRefundRevision,
        recovered: bool,
    ) -> Result<Outcome, DomainError> {
        if matches!(revision.outcome, VerifiedRefundOutcome::Active) && !recovered {
            return Err(DomainError::Invariant(
                "active Apple status is accepted only from recovery",
            ));
        }

        let transaction_id = revision.transaction.identity().transaction_id.clone();
        let (mut exact, missing_diamond) = match self.transactions.get(&transaction_id) {
            Some(record) => (record.clone(), false),
            None if !recovered
                && matches!(revision.transaction, VerifiedTransaction::Diamonds { .. }) =>
            {
                (TransactionRecord::new(revision.transaction.clone()), true)
            }
            None => return Err(DomainError::UnknownTransaction),
        };
        if exact.transaction != revision.transaction {
            return Err(DomainError::ImmutableTransactionConflict);
        }

        let previous = exact.refund_state.clone();
        let (changed, refund_transition) = exact.process_refund(refund_event(&revision))?;
        if !changed {
            return Ok(Outcome::one(false, refund_transition));
        }
        let next_miner = match &revision.transaction {
            VerifiedTransaction::Miner { identity, .. } => self.project_miner_refund(
                &identity.chain_id,
                &transaction_id,
                &previous,
                &exact.refund_state,
            )?,
            VerifiedTransaction::Vip { .. } | VerifiedTransaction::Diamonds { .. } => None,
        };

        let mut transitions = Vec::with_capacity(4);
        if missing_diamond {
            transitions.push(Transition::TransactionAccepted {
                transaction_id: transaction_id.clone(),
            });
        }
        transitions.push(refund_transition);
        self.transactions.insert(transaction_id.clone(), exact);
        if let Some((chain_id, record, miner_transition)) = next_miner {
            self.miners.insert(chain_id, record);
            transitions.push(miner_transition);
        }
        if missing_diamond {
            self.diamonds
                .insert(transaction_id, DiamondRecord::default());
        }
        if let Some(transition) = self.recompute_vip() {
            transitions.push(transition);
        }
        Ok(Outcome {
            changed: true,
            transitions,
        })
    }

    fn project_miner_refund(
        &self,
        chain_id: &ChainId,
        transaction_id: &TransactionId,
        previous: &refund::State,
        current: &refund::State,
    ) -> Result<Option<(ChainId, MinerRecord, Transition)>, DomainError> {
        let event = if current == &refund::State::Refunded {
            Some(miner::Event::RefundObserved {
                incoming_transaction_id: transaction_id.clone(),
            })
        } else if previous == &refund::State::Refunded {
            Some(miner::Event::ReversalObserved {
                incoming_transaction_id: transaction_id.clone(),
            })
        } else {
            None
        };
        let Some(event) = event else {
            return Ok(None);
        };
        let mut chain = self
            .miners
            .get(chain_id)
            .cloned()
            .ok_or(DomainError::UnknownChain)?;
        let (_, transition) = chain.process(event, self.retained_transactions(chain_id))?;
        Ok(Some((chain_id.clone(), chain, transition)))
    }

    fn request_consumption(
        &mut self,
        transaction_id: &TransactionId,
    ) -> Result<Outcome, DomainError> {
        let mut exact = self
            .transactions
            .get(transaction_id)
            .cloned()
            .ok_or(DomainError::UnknownTransaction)?;
        let (changed, transition) = exact.request_consumption()?;
        if changed {
            self.transactions.insert(transaction_id.clone(), exact);
        }
        Ok(Outcome::one(changed, transition))
    }

    fn change_vip_floor(&mut self, expires_at: Option<Timestamp>) -> Result<Outcome, DomainError> {
        if self.vip_floor == expires_at {
            return Ok(Outcome::ignored(IgnoreReason::UnchangedVipFloor));
        }
        let from = self.vip_floor;
        self.vip_floor = expires_at;
        let mut transitions = vec![Transition::VipFloorChanged {
            from,
            to: expires_at,
        }];
        if let Some(transition) = self.recompute_vip() {
            transitions.push(transition);
        }
        Ok(Outcome {
            changed: true,
            transitions,
        })
    }

    fn retained_transactions(&self, chain_id: &ChainId) -> Vec<TransactionId> {
        self.transactions
            .values()
            .filter_map(|record| {
                let VerifiedTransaction::Miner { identity, .. } = &record.transaction else {
                    return None;
                };
                (identity.chain_id == *chain_id).then(|| identity.transaction_id.clone())
            })
            .collect()
    }

    fn recompute_vip(&mut self) -> Option<Transition> {
        let from = self.vip_expires_at;
        let to = self
            .transactions
            .values()
            .filter(|record| record.refund_state.is_in(refund::StateId::Effective))
            .filter_map(|record| match record.transaction {
                VerifiedTransaction::Vip { expires_at, .. } => Some(expires_at),
                VerifiedTransaction::Miner { .. } | VerifiedTransaction::Diamonds { .. } => None,
            })
            .chain(self.vip_floor)
            .max();
        if from == to {
            return None;
        }
        self.vip_expires_at = to;
        Some(Transition::VipProjectionChanged { from, to })
    }
}

fn refund_event(revision: &VerifiedRefundRevision) -> refund::Event {
    match revision.outcome {
        VerifiedRefundOutcome::Refunded {
            revoked_at,
            percentage,
        } => refund::Event::Refund {
            incoming_signed_at: revision.signed_at,
            incoming_revoked_at: revoked_at,
            incoming_percentage: percentage,
        },
        VerifiedRefundOutcome::Declined => refund::Event::Decline {
            incoming_signed_at: revision.signed_at,
        },
        VerifiedRefundOutcome::Reversed => refund::Event::Reversal {
            incoming_signed_at: revision.signed_at,
        },
        VerifiedRefundOutcome::Active => refund::Event::ActiveRecovered {
            incoming_signed_at: revision.signed_at,
        },
    }
}

fn map_refund_error(
    error: ProcessError<refund::StateId, refund::Event, refund::Rejection>,
) -> DomainError {
    match error {
        ProcessError::Rejected(rejection) => DomainError::RefundRejected(rejection),
        ProcessError::Unhandled { .. } => DomainError::Invariant("refund event was unhandled"),
    }
}

fn map_miner_error(
    error: ProcessError<miner::StateId, miner::Event, miner::Rejection>,
) -> DomainError {
    match error {
        ProcessError::Rejected(rejection) => DomainError::MinerRejected(rejection),
        ProcessError::Unhandled { .. } => DomainError::Invariant("Miner event was unhandled"),
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum DomainError {
    UnknownTransaction,
    UnknownChain,
    ImmutableTransactionConflict,
    RefundRejected(refund::Rejection),
    MinerRejected(miner::Rejection),
    Invariant(&'static str),
}

impl Display for DomainError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "purchase transition failed: {self:?}")
    }
}

impl Error for DomainError {}
