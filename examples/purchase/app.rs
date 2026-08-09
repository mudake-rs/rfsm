use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};

use rfsm::ProcessError;

use crate::diamond;
use crate::miner;
use crate::refund;
use crate::store::{StoreError, VersionedStore};
use crate::verify::{
    ChainId, Timestamp, TransactionId, VerifiedRefundOutcome, VerifiedRefundRevision,
    VerifiedRenewalSnapshot, VerifiedTransaction,
};
use crate::vip::{self, AppleVipPeriod};

#[derive(Clone, Debug, PartialEq)]
pub struct TransactionRecord {
    pub transaction: VerifiedTransaction,
    pub refund_state: refund::State,
    pub refund_revision: Option<refund::Revision>,
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

#[derive(Clone, Debug, PartialEq)]
pub struct DiamondRecord {
    pub state: diamond::State,
    pub current_debit: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PurchaseState {
    pub transactions: HashMap<TransactionId, TransactionRecord>,
    pub miners: HashMap<ChainId, MinerRecord>,
    pub diamonds: HashMap<TransactionId, DiamondRecord>,
    pub diamond_balance: u64,
    pub vip_floor: Option<Timestamp>,
    pub vip_expires_at: Option<Timestamp>,
    pub consumption_audits: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PostCommitEffect {
    MinerAccess { chain_id: ChainId, active: bool },
    DiamondBalance { balance: u64 },
    VipExpiry { expires_at: Option<Timestamp> },
}

pub type PurchaseStore = VersionedStore<PurchaseState, PostCommitEffect>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DispatchOutcome {
    Unchanged,
    Applied { effects: Vec<PostCommitEffect> },
}

pub async fn apply_transaction(
    store: &mut PurchaseStore,
    transaction: VerifiedTransaction,
    observed_at: Timestamp,
) -> Result<DispatchOutcome, AppError> {
    let snapshot = store.load().await;
    let transaction_id = transaction.identity().transaction_id.clone();
    if let Some(existing) = snapshot.value.transactions.get(&transaction_id) {
        if existing.transaction != transaction {
            return Err(AppError::ImmutableTransactionConflict);
        }
        if let VerifiedTransaction::Diamonds { amount, .. } = transaction
            && snapshot
                .value
                .diamonds
                .get(&transaction_id)
                .is_some_and(|record| record.state == diamond::State::Undelivered)
        {
            let mut working = snapshot.value;
            let percentage = working
                .transactions
                .get(&transaction_id)
                .and_then(|record| record.refund_revision)
                .and_then(|revision| revision.refund)
                .map(|payload| payload.percentage);
            let mut effects = Vec::new();
            deliver_diamonds(
                &mut working,
                &transaction_id,
                amount,
                percentage,
                &mut effects,
            )?;
            return commit(store, snapshot.version, working, effects).await;
        }
        return Ok(DispatchOutcome::Unchanged);
    }

    let mut working = snapshot.value;
    let mut effects = Vec::new();
    match &transaction {
        VerifiedTransaction::Miner {
            identity,
            expires_at,
            ..
        } => {
            let retained = retained_transactions(&working, &identity.chain_id);
            let chain = working.miners.entry(identity.chain_id.clone()).or_default();
            let access_before = miner_access(chain, observed_at);
            let rules = miner_rules(chain, retained);
            let mut machine = miner::MinerChain::from_state(chain.state.clone(), rules);
            let applied = machine
                .process(miner::Event::Paid {
                    incoming_transaction_id: identity.transaction_id.clone(),
                    incoming_product_id: identity.product_id,
                    incoming_purchased_at: identity.purchased_at,
                    incoming_paid_until: *expires_at,
                })
                .map_err(map_miner_error)?;
            chain.state = machine.state().clone();
            if let Some(effect) = applied.effect {
                apply_miner_effect(chain, effect);
            }
            let access_after = miner_access(chain, observed_at);
            if access_before != access_after {
                effects.push(PostCommitEffect::MinerAccess {
                    chain_id: identity.chain_id.clone(),
                    active: access_after,
                });
            }
        }
        VerifiedTransaction::Vip { .. } => {}
        VerifiedTransaction::Diamonds {
            identity, amount, ..
        } => {
            working.diamonds.insert(
                identity.transaction_id.clone(),
                DiamondRecord {
                    state: diamond::State::Undelivered,
                    current_debit: 0,
                },
            );
            deliver_diamonds(
                &mut working,
                &identity.transaction_id,
                *amount,
                None,
                &mut effects,
            )?;
        }
    }

    working.transactions.insert(
        transaction_id,
        TransactionRecord {
            transaction,
            refund_state: refund::State::None,
            refund_revision: None,
        },
    );
    recompute_vip(&mut working, &mut effects);
    commit(store, snapshot.version, working, effects).await
}

pub async fn apply_renewal_snapshot(
    store: &mut PurchaseStore,
    snapshot_input: VerifiedRenewalSnapshot,
    observed_at: Timestamp,
) -> Result<DispatchOutcome, AppError> {
    let snapshot = store.load().await;
    let mut working = snapshot.value;
    let retained = retained_transactions(&working, &snapshot_input.chain_id);
    let chain = working
        .miners
        .get_mut(&snapshot_input.chain_id)
        .ok_or(AppError::UnknownChain)?;
    let access_before = miner_access(chain, observed_at);
    let rules = miner_rules(chain, retained);
    let mut machine = miner::MinerChain::from_state(chain.state.clone(), rules);
    let applied = machine
        .process(miner::Event::RenewalSnapshot {
            incoming_transaction_id: snapshot_input.transaction_id,
            incoming_signed_at: snapshot_input.signed_at,
            incoming_auto_renew_product_id: snapshot_input.auto_renew_product_id,
            incoming_billing_retry: snapshot_input.billing_retry,
            incoming_grace_until: snapshot_input.grace_until,
        })
        .map_err(map_miner_error)?;
    chain.state = machine.state().clone();
    let Some(effect) = applied.effect else {
        return Ok(DispatchOutcome::Unchanged);
    };
    apply_miner_effect(chain, effect);
    let access_after = miner_access(chain, observed_at);
    let effects = (access_before != access_after)
        .then_some(PostCommitEffect::MinerAccess {
            chain_id: snapshot_input.chain_id,
            active: access_after,
        })
        .into_iter()
        .collect();
    commit(store, snapshot.version, working, effects).await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RefundSource {
    Notification,
    Recovery,
}

pub async fn apply_refund_notification(
    store: &mut PurchaseStore,
    revision: VerifiedRefundRevision,
    observed_at: Timestamp,
    diamond_spent_after_delivery: Option<u64>,
) -> Result<DispatchOutcome, AppError> {
    let snapshot = store.load().await;
    let transaction_id = revision.transaction.identity().transaction_id.clone();
    let mut working = snapshot.value;
    if !working.transactions.contains_key(&transaction_id) {
        let VerifiedTransaction::Diamonds { .. } = &revision.transaction else {
            return Err(AppError::UnknownTransaction);
        };
        working.transactions.insert(
            transaction_id.clone(),
            TransactionRecord {
                transaction: revision.transaction.clone(),
                refund_state: refund::State::None,
                refund_revision: None,
            },
        );
        working.diamonds.insert(
            transaction_id.clone(),
            DiamondRecord {
                state: diamond::State::Undelivered,
                current_debit: 0,
            },
        );
    }

    apply_refund_to_snapshot(
        store,
        snapshot.version,
        working,
        revision,
        RefundSource::Notification,
        observed_at,
        diamond_spent_after_delivery,
    )
    .await
}

async fn apply_refund_to_snapshot(
    store: &mut PurchaseStore,
    expected_version: u64,
    mut working: PurchaseState,
    revision: VerifiedRefundRevision,
    source: RefundSource,
    observed_at: Timestamp,
    diamond_spent_after_delivery: Option<u64>,
) -> Result<DispatchOutcome, AppError> {
    let transaction_id = revision.transaction.identity().transaction_id.clone();
    let (previous_state, current_revision) = {
        let record = working
            .transactions
            .get(&transaction_id)
            .ok_or(AppError::UnknownTransaction)?;
        if record.transaction != revision.transaction {
            return Err(AppError::ImmutableTransactionConflict);
        }
        (record.refund_state.clone(), record.refund_revision)
    };
    let mut machine =
        refund::Refunds::from_state(previous_state.clone(), refund::Rules::new(current_revision));
    let event = match revision.outcome {
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
        VerifiedRefundOutcome::Active if source == RefundSource::Recovery => {
            refund::Event::ActiveRecovered {
                incoming_signed_at: revision.signed_at,
            }
        }
        VerifiedRefundOutcome::Active => {
            return Err(AppError::Invariant(
                "active Apple status is accepted only from recovery",
            ));
        }
    };
    let applied = match machine.process(event) {
        Ok(applied) => applied,
        Err(ProcessError::Rejected(refund::Rejection::StaleRevision))
            if source == RefundSource::Notification =>
        {
            return Ok(DispatchOutcome::Unchanged);
        }
        Err(ProcessError::Rejected(refund::Rejection::StaleRevision)) => {
            return Err(AppError::StaleRecovery);
        }
        Err(error) => return Err(map_refund_error(error)),
    };
    let Some(refund::Effect::Persist(persisted)) = applied.effect else {
        return Ok(DispatchOutcome::Unchanged);
    };
    let current_state = machine.state().clone();

    let record = working
        .transactions
        .get_mut(&transaction_id)
        .ok_or(AppError::UnknownTransaction)?;
    record.refund_state = current_state.clone();
    record.refund_revision = Some(persisted);

    let mut effects = Vec::new();
    match &revision.transaction {
        VerifiedTransaction::Miner { identity, .. } => {
            let retained = retained_transactions(&working, &identity.chain_id);
            let chain = working
                .miners
                .get_mut(&identity.chain_id)
                .ok_or(AppError::UnknownChain)?;
            let access_before = miner_access(chain, observed_at);
            let rules = miner_rules(chain, retained);
            let mut miner_machine = miner::MinerChain::from_state(chain.state.clone(), rules);
            let miner_event = if current_state == refund::State::Refunded {
                Some(miner::Event::RefundObserved {
                    incoming_transaction_id: transaction_id.clone(),
                })
            } else if previous_state == refund::State::Refunded {
                Some(miner::Event::ReversalObserved {
                    incoming_transaction_id: transaction_id.clone(),
                })
            } else {
                None
            };
            if let Some(miner_event) = miner_event {
                let family_applied = miner_machine
                    .process(miner_event)
                    .map_err(map_miner_error)?;
                chain.state = miner_machine.state().clone();
                if let Some(effect) = family_applied.effect {
                    apply_miner_effect(chain, effect);
                }
                let access_after = miner_access(chain, observed_at);
                if access_before != access_after {
                    effects.push(PostCommitEffect::MinerAccess {
                        chain_id: identity.chain_id.clone(),
                        active: access_after,
                    });
                }
            }
        }
        VerifiedTransaction::Diamonds {
            identity, amount, ..
        } => {
            apply_diamond_refund(
                &mut working,
                &identity.transaction_id,
                *amount,
                &previous_state,
                diamond_spent_after_delivery,
                &mut effects,
            )?;
        }
        VerifiedTransaction::Vip { .. } => recompute_vip(&mut working, &mut effects),
    }

    commit(store, expected_version, working, effects).await
}

pub async fn request_consumption(
    store: &mut PurchaseStore,
    transaction_id: &TransactionId,
    request_id: u64,
) -> Result<DispatchOutcome, AppError> {
    let snapshot = store.load().await;
    let mut working = snapshot.value;
    let record = working
        .transactions
        .get_mut(transaction_id)
        .ok_or(AppError::UnknownTransaction)?;
    let mut machine = refund::Refunds::from_state(
        record.refund_state.clone(),
        refund::Rules::new(record.refund_revision),
    );
    let applied = machine
        .process(refund::Event::ConsumptionRequested { request_id })
        .map_err(map_refund_error)?;
    record.refund_state = machine.state().clone();
    let Some(refund::Effect::AuditConsumption { request_id }) = applied.effect else {
        return Err(AppError::Invariant("consumption request was not audited"));
    };
    working.consumption_audits.push(request_id);
    commit(store, snapshot.version, working, Vec::new()).await
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundRecoveryGuard {
    version: u64,
    transaction_id: TransactionId,
}

pub async fn begin_refund_recovery(
    store: &PurchaseStore,
    transaction_id: &TransactionId,
) -> Result<RefundRecoveryGuard, AppError> {
    let snapshot = store.load().await;
    if !snapshot.value.transactions.contains_key(transaction_id) {
        return Err(AppError::UnknownTransaction);
    }
    Ok(RefundRecoveryGuard {
        version: snapshot.version,
        transaction_id: transaction_id.clone(),
    })
}

pub async fn finish_refund_recovery(
    store: &mut PurchaseStore,
    guard: RefundRecoveryGuard,
    recovered: VerifiedRefundRevision,
    observed_at: Timestamp,
    diamond_spent_after_delivery: Option<u64>,
) -> Result<DispatchOutcome, AppError> {
    let snapshot = store.load().await;
    if snapshot.version != guard.version {
        return Err(AppError::Store(StoreError::VersionConflict));
    }
    if recovered.transaction.identity().transaction_id != guard.transaction_id {
        return Err(AppError::RecoveryTargetMismatch);
    }
    apply_refund_to_snapshot(
        store,
        guard.version,
        snapshot.value,
        recovered,
        RefundSource::Recovery,
        observed_at,
        diamond_spent_after_delivery,
    )
    .await
}

fn retained_transactions(state: &PurchaseState, chain_id: &ChainId) -> Vec<TransactionId> {
    state
        .transactions
        .values()
        .filter_map(|record| {
            let VerifiedTransaction::Miner { identity, .. } = &record.transaction else {
                return None;
            };
            (identity.chain_id == *chain_id).then(|| identity.transaction_id.clone())
        })
        .collect()
}

fn miner_rules(record: &MinerRecord, retained_transactions: Vec<TransactionId>) -> miner::Rules {
    miner::Rules::new(
        record.head.clone(),
        record.renewal.clone(),
        retained_transactions,
    )
}

fn miner_access(record: &MinerRecord, observed_at: Timestamp) -> bool {
    miner::has_access(
        &record.state,
        record.head.as_ref(),
        record.renewal.as_ref(),
        observed_at,
    )
}

fn apply_miner_effect(record: &mut MinerRecord, effect: miner::Effect) {
    match effect {
        miner::Effect::AdvanceHead(head) => {
            record.head = Some(head);
            record.renewal = None;
        }
        miner::Effect::ReplaceRenewal(renewal) => record.renewal = Some(renewal),
    }
}

fn apply_diamond_refund(
    working: &mut PurchaseState,
    transaction_id: &TransactionId,
    grant: u64,
    previous_state: &refund::State,
    spent_after_delivery: Option<u64>,
    effects: &mut Vec<PostCommitEffect>,
) -> Result<(), AppError> {
    let exact = working
        .transactions
        .get(transaction_id)
        .ok_or(AppError::UnknownTransaction)?;
    let current_state = exact.refund_state.clone();
    let revision = exact
        .refund_revision
        .ok_or(AppError::Invariant("refunded Diamond row has no revision"))?;
    let Some(record) = working.diamonds.get_mut(transaction_id) else {
        return Ok(());
    };
    if record.state != diamond::State::Delivered {
        return Ok(());
    }

    let adjusted = if current_state == refund::State::Refunded {
        let payload = revision
            .refund
            .ok_or(AppError::Invariant("refunded row has no refund payload"))?;
        let spent_after_delivery =
            spent_after_delivery.ok_or(AppError::MissingDiamondSpendEvidence)?;
        let remaining_unspent = grant
            .saturating_sub(spent_after_delivery)
            .saturating_sub(record.current_debit);
        diamond::adjust_refund(
            grant,
            record.current_debit,
            payload.percentage,
            remaining_unspent,
            working.diamond_balance,
        )
        .map_err(|_| AppError::Invariant("invalid exact Diamond debit"))?
    } else if previous_state == &refund::State::Refunded {
        diamond::reverse_refund(record.current_debit)
    } else {
        return Ok(());
    };

    match adjusted.adjustment {
        diamond::Adjustment::None => {}
        diamond::Adjustment::Debit(amount) => {
            working.diamond_balance = working
                .diamond_balance
                .checked_sub(amount)
                .ok_or(AppError::Invariant("Diamond refund exceeded balance"))?;
            effects.push(PostCommitEffect::DiamondBalance {
                balance: working.diamond_balance,
            });
        }
        diamond::Adjustment::Credit(amount) => {
            working.diamond_balance = working
                .diamond_balance
                .checked_add(amount)
                .ok_or(AppError::Invariant("Diamond balance overflow"))?;
            effects.push(PostCommitEffect::DiamondBalance {
                balance: working.diamond_balance,
            });
        }
    }
    record.current_debit = adjusted.current_debit;
    Ok(())
}

fn deliver_diamonds(
    working: &mut PurchaseState,
    transaction_id: &TransactionId,
    grant: u64,
    current_refund_percentage: Option<crate::verify::RefundPercentage>,
    effects: &mut Vec<PostCommitEffect>,
) -> Result<(), AppError> {
    let record = working
        .diamonds
        .get_mut(transaction_id)
        .ok_or(AppError::Invariant("missing exact Diamond delivery row"))?;
    let mut machine = diamond::Diamonds::from_state(record.state.clone(), diamond::Rules);
    let applied = machine
        .process(diamond::Event::Deliver {
            grant,
            current_refund_percentage,
        })
        .map_err(|_| AppError::Invariant("Diamond delivery was unhandled"))?;
    let Some(delivery) = applied.effect else {
        return Ok(());
    };
    working.diamond_balance = working
        .diamond_balance
        .checked_add(delivery.credit - delivery.refund_debit)
        .ok_or(AppError::Invariant("Diamond balance overflow"))?;
    record.state = machine.state().clone();
    record.current_debit = delivery.refund_debit;
    effects.push(PostCommitEffect::DiamondBalance {
        balance: working.diamond_balance,
    });
    Ok(())
}

fn recompute_vip(working: &mut PurchaseState, effects: &mut Vec<PostCommitEffect>) {
    let periods = working.transactions.values().filter_map(|record| {
        let VerifiedTransaction::Vip { expires_at, .. } = &record.transaction else {
            return None;
        };
        Some(AppleVipPeriod {
            expires_at: *expires_at,
            active: matches!(
                record.refund_state,
                refund::State::None
                    | refund::State::Requested
                    | refund::State::Declined
                    | refund::State::Reversed
            ),
        })
    });
    let next = vip::effective_expiry(working.vip_floor, periods);
    if next != working.vip_expires_at {
        working.vip_expires_at = next;
        effects.push(PostCommitEffect::VipExpiry { expires_at: next });
    }
}

async fn commit(
    store: &mut PurchaseStore,
    expected_version: u64,
    working: PurchaseState,
    effects: Vec<PostCommitEffect>,
) -> Result<DispatchOutcome, AppError> {
    let outcome = DispatchOutcome::Applied {
        effects: effects.clone(),
    };
    let mut transaction = store.begin(expected_version);
    transaction.store(working).await;
    for effect in effects {
        transaction.push_effect(effect).await;
    }
    transaction.commit().await.map_err(AppError::Store)?;
    Ok(outcome)
}

fn map_refund_error(
    error: ProcessError<refund::StateId, refund::Event, refund::Rejection>,
) -> AppError {
    match error {
        ProcessError::Rejected(rejection) => AppError::RefundRejected(rejection),
        ProcessError::Unhandled { .. } => AppError::Invariant("refund event was unhandled"),
    }
}

fn map_miner_error(
    error: ProcessError<miner::StateId, miner::Event, miner::Rejection>,
) -> AppError {
    match error {
        ProcessError::Rejected(rejection) => AppError::MinerRejected(rejection),
        ProcessError::Unhandled { .. } => AppError::Invariant("Miner event was unhandled"),
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum AppError {
    UnknownTransaction,
    UnknownChain,
    ImmutableTransactionConflict,
    RecoveryTargetMismatch,
    MissingDiamondSpendEvidence,
    StaleRecovery,
    RefundRejected(refund::Rejection),
    MinerRejected(miner::Rejection),
    Store(StoreError),
    Invariant(&'static str),
}

impl Display for AppError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "purchase processing failed: {self:?}")
    }
}

impl Error for AppError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::{DIAMONDS, MINER_CURRENT, MINER_RETIRED, VIP};
    use crate::run_ready;
    use crate::verify::{
        Ownership, RefundPercentage, TransactionIdentity, UntrustedTransaction,
        verify_recovered_active, verify_transaction,
    };

    fn signed_miner_transaction(transaction: &str, purchased_at: u64) -> UntrustedTransaction {
        UntrustedTransaction {
            signature_valid: true,
            bundle_id: crate::verify::EXPECTED_BUNDLE.to_owned(),
            environment: crate::verify::EXPECTED_ENVIRONMENT.to_owned(),
            transaction_id: TransactionId::new(transaction),
            chain_id: ChainId::new("chain-1"),
            product_id: MINER_CURRENT.as_str().to_owned(),
            signed_at: Timestamp(purchased_at),
            purchased_at: Timestamp(purchased_at),
            expires_at: Some(Timestamp(purchased_at + 10)),
            quantity: None,
            ownership: Ownership::Purchased,
        }
    }

    fn miner_transaction(transaction: &str, purchased_at: u64) -> VerifiedTransaction {
        verify_transaction(signed_miner_transaction(transaction, purchased_at))
            .unwrap_or_else(|failure| panic!("invalid test transaction: {failure}"))
    }

    fn full_refund(transaction: VerifiedTransaction, signed_at: u64) -> VerifiedRefundRevision {
        VerifiedRefundRevision {
            transaction,
            signed_at: Timestamp(signed_at),
            outcome: VerifiedRefundOutcome::Refunded {
                revoked_at: Timestamp(signed_at - 1),
                percentage: RefundPercentage::FULL,
            },
        }
    }

    fn diamond_transaction(transaction: &str) -> VerifiedTransaction {
        VerifiedTransaction::Diamonds {
            identity: TransactionIdentity {
                transaction_id: TransactionId::new(transaction),
                chain_id: ChainId::new(transaction),
                product_id: DIAMONDS,
                purchased_at: Timestamp(10),
            },
            amount: 2_000_000,
        }
    }

    fn vip_transaction(transaction: &str, expires_at: u64) -> VerifiedTransaction {
        VerifiedTransaction::Vip {
            identity: TransactionIdentity {
                transaction_id: TransactionId::new(transaction),
                chain_id: ChainId::new(transaction),
                product_id: VIP,
                purchased_at: Timestamp(10),
            },
            expires_at: Timestamp(expires_at),
        }
    }

    fn half_refund(transaction: VerifiedTransaction) -> VerifiedRefundRevision {
        VerifiedRefundRevision {
            transaction,
            signed_at: Timestamp(20),
            outcome: VerifiedRefundOutcome::Refunded {
                revoked_at: Timestamp(19),
                percentage: RefundPercentage::new(50_000)
                    .unwrap_or_else(|failure| panic!("invalid test percentage: {failure}")),
            },
        }
    }

    fn reversal(transaction: VerifiedTransaction, signed_at: u64) -> VerifiedRefundRevision {
        VerifiedRefundRevision {
            transaction,
            signed_at: Timestamp(signed_at),
            outcome: VerifiedRefundOutcome::Reversed,
        }
    }

    #[test]
    fn exact_transaction_replay_applies_value_and_effect_once() {
        let mut store = PurchaseStore::new(PurchaseState::default());
        let transaction = miner_transaction("t1", 10);
        let first = run_ready(apply_transaction(
            &mut store,
            transaction.clone(),
            Timestamp(15),
        ))
        .unwrap_or_else(|failure| panic!("unexpected purchase failure: {failure}"));
        let version = store.version();
        let effect_count = store.committed_effects().len();

        assert!(matches!(first, DispatchOutcome::Applied { .. }));
        assert_eq!(
            run_ready(apply_transaction(&mut store, transaction, Timestamp(15)))
                .unwrap_or_else(|failure| panic!("unexpected replay failure: {failure}")),
            DispatchOutcome::Unchanged
        );
        assert_eq!(store.version(), version);
        assert_eq!(store.committed_effects().len(), effect_count);
    }

    #[test]
    fn contradictory_immutable_transaction_is_not_a_replay() {
        let mut store = PurchaseStore::new(PurchaseState::default());
        run_ready(apply_transaction(
            &mut store,
            miner_transaction("t1", 10),
            Timestamp(15),
        ))
        .unwrap_or_else(|failure| panic!("unexpected purchase failure: {failure}"));
        let state = store.value().clone();
        let version = store.version();

        assert_eq!(
            run_ready(apply_transaction(
                &mut store,
                miner_transaction("t1", 11),
                Timestamp(15),
            )),
            Err(AppError::ImmutableTransactionConflict)
        );
        assert_eq!(store.value(), &state);
        assert_eq!(store.version(), version);
    }

    #[test]
    fn refund_identity_cannot_be_retargeted_to_an_existing_transaction_id() {
        let mut store = PurchaseStore::new(PurchaseState::default());
        run_ready(apply_transaction(
            &mut store,
            miner_transaction("t1", 10),
            Timestamp(15),
        ))
        .unwrap_or_else(|failure| panic!("unexpected purchase failure: {failure}"));
        let state = store.value().clone();
        let version = store.version();

        assert_eq!(
            run_ready(apply_refund_notification(
                &mut store,
                full_refund(miner_transaction("t1", 11), 20),
                Timestamp(20),
                None,
            )),
            Err(AppError::ImmutableTransactionConflict)
        );
        assert_eq!(store.value(), &state);
        assert_eq!(store.version(), version);
    }

    #[test]
    fn diamond_refund_first_and_delivery_first_commit_the_same_value() {
        let transaction = diamond_transaction("diamond-1");

        let mut refund_first = PurchaseStore::new(PurchaseState::default());
        run_ready(apply_refund_notification(
            &mut refund_first,
            half_refund(transaction.clone()),
            Timestamp(20),
            None,
        ))
        .unwrap_or_else(|failure| panic!("unexpected refund-first failure: {failure}"));
        assert_eq!(refund_first.value().diamond_balance, 0);
        assert_eq!(
            refund_first
                .value()
                .diamonds
                .get(&TransactionId::new("diamond-1"))
                .map(|record| &record.state),
            Some(&diamond::State::Undelivered)
        );
        run_ready(apply_transaction(
            &mut refund_first,
            transaction.clone(),
            Timestamp(20),
        ))
        .unwrap_or_else(|failure| panic!("unexpected delayed delivery failure: {failure}"));

        let mut delivery_first = PurchaseStore::new(PurchaseState::default());
        run_ready(apply_transaction(
            &mut delivery_first,
            transaction.clone(),
            Timestamp(15),
        ))
        .unwrap_or_else(|failure| panic!("unexpected delivery failure: {failure}"));
        run_ready(apply_refund_notification(
            &mut delivery_first,
            half_refund(transaction),
            Timestamp(20),
            Some(0),
        ))
        .unwrap_or_else(|failure| panic!("unexpected later refund failure: {failure}"));

        assert_eq!(
            refund_first.value().diamond_balance,
            delivery_first.value().diamond_balance
        );
        assert_eq!(
            refund_first
                .value()
                .diamonds
                .get(&TransactionId::new("diamond-1")),
            delivery_first
                .value()
                .diamonds
                .get(&TransactionId::new("diamond-1"))
        );
    }

    #[test]
    fn diamond_refund_requires_and_uses_the_proven_post_delivery_spend() {
        let first = diamond_transaction("diamond-1");
        let second = diamond_transaction("diamond-2");
        let mut delivered = PurchaseStore::new(PurchaseState::default());
        run_ready(apply_transaction(
            &mut delivered,
            first.clone(),
            Timestamp(11),
        ))
        .unwrap_or_else(|failure| panic!("unexpected delivery failure: {failure}"));
        run_ready(apply_transaction(&mut delivered, second, Timestamp(12)))
            .unwrap_or_else(|failure| panic!("unexpected delivery failure: {failure}"));

        let state = delivered.value().clone();
        let mut missing_evidence = PurchaseStore::new(state.clone());
        assert_eq!(
            run_ready(apply_refund_notification(
                &mut missing_evidence,
                full_refund(first.clone(), 20),
                Timestamp(20),
                None,
            )),
            Err(AppError::MissingDiamondSpendEvidence)
        );
        assert_eq!(missing_evidence.value(), &state);

        let mut spent_state = state;
        spent_state.diamond_balance = 1_000_000;
        let mut shared_spend = PurchaseStore::new(spent_state);
        let outcome = run_ready(apply_refund_notification(
            &mut shared_spend,
            full_refund(first, 20),
            Timestamp(20),
            Some(3_000_000),
        ))
        .unwrap_or_else(|failure| panic!("unexpected refund failure: {failure}"));

        assert_eq!(outcome, DispatchOutcome::Applied { effects: vec![] });
        assert_eq!(shared_spend.value().diamond_balance, 1_000_000);
        assert_eq!(
            shared_spend
                .value()
                .diamonds
                .get(&TransactionId::new("diamond-1"))
                .map(|record| record.current_debit),
            Some(0)
        );
    }

    #[test]
    fn vip_purchase_and_refund_publish_the_maximum_with_the_non_apple_floor() {
        let transaction = vip_transaction("vip-1", 120);
        let mut store = PurchaseStore::new(PurchaseState {
            vip_floor: Some(Timestamp(100)),
            vip_expires_at: Some(Timestamp(100)),
            ..PurchaseState::default()
        });

        let purchased = run_ready(apply_transaction(
            &mut store,
            transaction.clone(),
            Timestamp(10),
        ))
        .unwrap_or_else(|failure| panic!("unexpected VIP purchase failure: {failure}"));
        assert_eq!(
            purchased,
            DispatchOutcome::Applied {
                effects: vec![PostCommitEffect::VipExpiry {
                    expires_at: Some(Timestamp(120)),
                }],
            }
        );

        let refunded = run_ready(apply_refund_notification(
            &mut store,
            full_refund(transaction, 130),
            Timestamp(130),
            None,
        ))
        .unwrap_or_else(|failure| panic!("unexpected VIP refund failure: {failure}"));
        assert_eq!(
            refunded,
            DispatchOutcome::Applied {
                effects: vec![PostCommitEffect::VipExpiry {
                    expires_at: Some(Timestamp(100)),
                }],
            }
        );
        assert_eq!(store.value().vip_expires_at, Some(Timestamp(100)));
    }

    #[test]
    fn paid_head_advances_even_when_renewal_evidence_is_not_yet_usable() {
        let mut store = PurchaseStore::new(PurchaseState::default());
        run_ready(apply_transaction(
            &mut store,
            miner_transaction("t1", 10),
            Timestamp(15),
        ))
        .unwrap_or_else(|failure| panic!("unexpected initial purchase failure: {failure}"));
        run_ready(apply_transaction(
            &mut store,
            miner_transaction("t2", 20),
            Timestamp(21),
        ))
        .unwrap_or_else(|failure| panic!("unexpected renewal failure: {failure}"));

        let chain = store
            .value()
            .miners
            .get(&ChainId::new("chain-1"))
            .unwrap_or_else(|| panic!("missing Miner chain"));
        assert_eq!(
            chain.head.as_ref().map(|head| &head.transaction_id),
            Some(&TransactionId::new("t2"))
        );
        assert_eq!(chain.renewal, None);
    }

    #[test]
    fn access_effects_follow_paid_expiry_grace_and_reversal_time() {
        let transaction = miner_transaction("t1", 10);

        let mut expired = PurchaseStore::new(PurchaseState::default());
        let outcome = run_ready(apply_transaction(
            &mut expired,
            transaction.clone(),
            Timestamp(25),
        ))
        .unwrap_or_else(|failure| panic!("unexpected purchase failure: {failure}"));
        assert_eq!(outcome, DispatchOutcome::Applied { effects: vec![] });

        let mut reversed_late = PurchaseStore::new(PurchaseState::default());
        run_ready(apply_transaction(
            &mut reversed_late,
            transaction.clone(),
            Timestamp(15),
        ))
        .unwrap_or_else(|failure| panic!("unexpected purchase failure: {failure}"));
        run_ready(apply_refund_notification(
            &mut reversed_late,
            full_refund(transaction.clone(), 16),
            Timestamp(16),
            None,
        ))
        .unwrap_or_else(|failure| panic!("unexpected refund failure: {failure}"));
        let outcome = run_ready(apply_refund_notification(
            &mut reversed_late,
            reversal(transaction.clone(), 17),
            Timestamp(25),
            None,
        ))
        .unwrap_or_else(|failure| panic!("unexpected reversal failure: {failure}"));
        assert_eq!(outcome, DispatchOutcome::Applied { effects: vec![] });

        let mut grace = PurchaseStore::new(PurchaseState::default());
        run_ready(apply_transaction(&mut grace, transaction, Timestamp(15)))
            .unwrap_or_else(|failure| panic!("unexpected purchase failure: {failure}"));
        let activated = run_ready(apply_renewal_snapshot(
            &mut grace,
            VerifiedRenewalSnapshot {
                chain_id: ChainId::new("chain-1"),
                transaction_id: TransactionId::new("t1"),
                signed_at: Timestamp(21),
                auto_renew_product_id: Some(MINER_CURRENT),
                billing_retry: true,
                grace_until: Some(Timestamp(30)),
            },
            Timestamp(25),
        ))
        .unwrap_or_else(|failure| panic!("unexpected grace failure: {failure}"));
        assert_eq!(
            activated,
            DispatchOutcome::Applied {
                effects: vec![PostCommitEffect::MinerAccess {
                    chain_id: ChainId::new("chain-1"),
                    active: true,
                }],
            }
        );

        let removed = run_ready(apply_renewal_snapshot(
            &mut grace,
            VerifiedRenewalSnapshot {
                chain_id: ChainId::new("chain-1"),
                transaction_id: TransactionId::new("t1"),
                signed_at: Timestamp(22),
                auto_renew_product_id: None,
                billing_retry: true,
                grace_until: None,
            },
            Timestamp(25),
        ))
        .unwrap_or_else(|failure| panic!("unexpected grace removal failure: {failure}"));
        assert_eq!(
            removed,
            DispatchOutcome::Applied {
                effects: vec![PostCommitEffect::MinerAccess {
                    chain_id: ChainId::new("chain-1"),
                    active: false,
                }],
            }
        );
    }

    #[test]
    fn renewal_and_refund_clocks_advance_independently() {
        let mut store = PurchaseStore::new(PurchaseState::default());
        run_ready(apply_transaction(
            &mut store,
            miner_transaction("t1", 10),
            Timestamp(15),
        ))
        .unwrap_or_else(|failure| panic!("unexpected purchase failure: {failure}"));
        run_ready(apply_renewal_snapshot(
            &mut store,
            VerifiedRenewalSnapshot {
                chain_id: ChainId::new("chain-1"),
                transaction_id: TransactionId::new("t1"),
                signed_at: Timestamp(30),
                auto_renew_product_id: Some(MINER_RETIRED),
                billing_retry: true,
                grace_until: Some(Timestamp(25)),
            },
            Timestamp(15),
        ))
        .unwrap_or_else(|failure| panic!("unexpected snapshot failure: {failure}"));
        run_ready(apply_refund_notification(
            &mut store,
            full_refund(miner_transaction("t1", 10), 20),
            Timestamp(20),
            None,
        ))
        .unwrap_or_else(|failure| panic!("unexpected refund failure: {failure}"));

        let chain = store
            .value()
            .miners
            .get(&ChainId::new("chain-1"))
            .unwrap_or_else(|| panic!("missing Miner chain"));
        let transaction = store
            .value()
            .transactions
            .get(&TransactionId::new("t1"))
            .unwrap_or_else(|| panic!("missing exact transaction"));
        assert_eq!(
            chain.renewal.as_ref().map(|renewal| renewal.signed_at),
            Some(Timestamp(30))
        );
        assert_eq!(
            transaction
                .refund_revision
                .map(|revision| revision.signed_at),
            Some(Timestamp(20))
        );
        assert_eq!(chain.state, miner::State::Refunded);
    }

    #[test]
    fn retained_snapshot_is_repeatedly_unchanged_without_a_second_audit_log() {
        let mut store = PurchaseStore::new(PurchaseState::default());
        run_ready(apply_transaction(
            &mut store,
            miner_transaction("t1", 10),
            Timestamp(15),
        ))
        .unwrap_or_else(|failure| panic!("unexpected purchase failure: {failure}"));
        run_ready(apply_transaction(
            &mut store,
            miner_transaction("t2", 20),
            Timestamp(21),
        ))
        .unwrap_or_else(|failure| panic!("unexpected renewal failure: {failure}"));
        let version = store.version();
        let effect_count = store.committed_effects().len();

        for _ in 0..2 {
            let outcome = run_ready(apply_renewal_snapshot(
                &mut store,
                VerifiedRenewalSnapshot {
                    chain_id: ChainId::new("chain-1"),
                    transaction_id: TransactionId::new("t1"),
                    signed_at: Timestamp(30),
                    auto_renew_product_id: Some(MINER_CURRENT),
                    billing_retry: false,
                    grace_until: None,
                },
                Timestamp(21),
            ))
            .unwrap_or_else(|failure| panic!("unexpected retained snapshot failure: {failure}"));
            assert_eq!(outcome, DispatchOutcome::Unchanged);
        }

        assert_eq!(store.version(), version);
        assert_eq!(store.committed_effects().len(), effect_count);
    }

    #[test]
    fn stale_notification_is_acknowledged_but_stale_recovery_is_retryable() {
        let mut store = PurchaseStore::new(PurchaseState::default());
        run_ready(apply_transaction(
            &mut store,
            miner_transaction("t1", 10),
            Timestamp(15),
        ))
        .unwrap_or_else(|failure| panic!("unexpected purchase failure: {failure}"));
        run_ready(apply_refund_notification(
            &mut store,
            full_refund(miner_transaction("t1", 10), 20),
            Timestamp(20),
            None,
        ))
        .unwrap_or_else(|failure| panic!("unexpected refund failure: {failure}"));
        let version = store.version();

        assert_eq!(
            run_ready(apply_refund_notification(
                &mut store,
                full_refund(miner_transaction("t1", 10), 19),
                Timestamp(20),
                None,
            ))
            .unwrap_or_else(|failure| panic!("unexpected stale notification failure: {failure}")),
            DispatchOutcome::Unchanged
        );
        assert_eq!(store.version(), version);
        let guard = run_ready(begin_refund_recovery(&store, &TransactionId::new("t1")))
            .unwrap_or_else(|failure| panic!("unexpected guard failure: {failure}"));
        assert_eq!(
            run_ready(finish_refund_recovery(
                &mut store,
                guard,
                full_refund(miner_transaction("t1", 10), 19),
                Timestamp(20),
                None,
            )),
            Err(AppError::StaleRecovery)
        );
    }

    #[test]
    fn active_recovery_is_bound_to_the_guarded_exact_transaction() {
        let mut store = PurchaseStore::new(PurchaseState::default());
        let transaction = miner_transaction("t1", 10);
        run_ready(apply_transaction(
            &mut store,
            transaction.clone(),
            Timestamp(15),
        ))
        .unwrap_or_else(|failure| panic!("unexpected purchase failure: {failure}"));
        run_ready(apply_refund_notification(
            &mut store,
            full_refund(transaction, 16),
            Timestamp(16),
            None,
        ))
        .unwrap_or_else(|failure| panic!("unexpected refund failure: {failure}"));
        let guard = run_ready(begin_refund_recovery(&store, &TransactionId::new("t1")))
            .unwrap_or_else(|failure| panic!("unexpected guard failure: {failure}"));
        let mut wrong_transaction = signed_miner_transaction("t2", 10);
        wrong_transaction.signed_at = Timestamp(17);
        let wrong = verify_recovered_active(wrong_transaction)
            .unwrap_or_else(|failure| panic!("unexpected verification failure: {failure}"));
        assert_eq!(
            run_ready(finish_refund_recovery(
                &mut store,
                guard.clone(),
                wrong,
                Timestamp(17),
                None,
            )),
            Err(AppError::RecoveryTargetMismatch)
        );

        let mut active_transaction = signed_miner_transaction("t1", 10);
        active_transaction.signed_at = Timestamp(17);
        let active = verify_recovered_active(active_transaction)
            .unwrap_or_else(|failure| panic!("unexpected verification failure: {failure}"));
        let outcome = run_ready(finish_refund_recovery(
            &mut store,
            guard,
            active,
            Timestamp(17),
            None,
        ))
        .unwrap_or_else(|failure| panic!("unexpected recovery failure: {failure}"));

        assert_eq!(
            outcome,
            DispatchOutcome::Applied {
                effects: vec![PostCommitEffect::MinerAccess {
                    chain_id: ChainId::new("chain-1"),
                    active: true,
                }],
            }
        );
        assert_eq!(
            store
                .value()
                .transactions
                .get(&TransactionId::new("t1"))
                .map(|record| &record.refund_state),
            Some(&refund::State::Reversed)
        );

        let version = store.version();
        let effect_count = store.committed_effects().len();
        assert_eq!(
            run_ready(apply_refund_notification(
                &mut store,
                reversal(miner_transaction("t1", 10), 17),
                Timestamp(17),
                None,
            ))
            .unwrap_or_else(|failure| panic!("unexpected replay failure: {failure}")),
            DispatchOutcome::Unchanged
        );
        assert_eq!(store.version(), version);
        assert_eq!(store.committed_effects().len(), effect_count);
    }

    #[test]
    fn active_recovery_preserves_requested_label_and_advances_watermark() {
        let mut store = PurchaseStore::new(PurchaseState::default());
        run_ready(apply_transaction(
            &mut store,
            miner_transaction("t1", 10),
            Timestamp(15),
        ))
        .unwrap_or_else(|failure| panic!("unexpected purchase failure: {failure}"));
        run_ready(request_consumption(
            &mut store,
            &TransactionId::new("t1"),
            7,
        ))
        .unwrap_or_else(|failure| panic!("unexpected consumption failure: {failure}"));
        let guard = run_ready(begin_refund_recovery(&store, &TransactionId::new("t1")))
            .unwrap_or_else(|failure| panic!("unexpected guard failure: {failure}"));
        let mut active_transaction = signed_miner_transaction("t1", 10);
        active_transaction.signed_at = Timestamp(17);
        let active = verify_recovered_active(active_transaction)
            .unwrap_or_else(|failure| panic!("unexpected verification failure: {failure}"));

        let outcome = run_ready(finish_refund_recovery(
            &mut store,
            guard,
            active,
            Timestamp(17),
            None,
        ))
        .unwrap_or_else(|failure| panic!("unexpected recovery failure: {failure}"));

        assert_eq!(outcome, DispatchOutcome::Applied { effects: vec![] });
        let record = store
            .value()
            .transactions
            .get(&TransactionId::new("t1"))
            .unwrap_or_else(|| panic!("missing exact transaction"));
        assert_eq!(record.refund_state, refund::State::Requested);
        assert_eq!(store.value().consumption_audits, vec![7]);
        assert_eq!(
            record.refund_revision,
            Some(refund::Revision {
                signed_at: Timestamp(17),
                refund: None,
            })
        );
        assert_eq!(
            store
                .value()
                .miners
                .get(&ChainId::new("chain-1"))
                .map(|chain| &chain.state),
            Some(&miner::State::Bound)
        );
    }

    #[test]
    fn superseded_refund_commits_exact_audit_without_post_commit_effect() {
        let mut store = PurchaseStore::new(PurchaseState::default());
        run_ready(apply_transaction(
            &mut store,
            miner_transaction("t1", 10),
            Timestamp(15),
        ))
        .unwrap_or_else(|failure| panic!("unexpected purchase failure: {failure}"));
        run_ready(apply_transaction(
            &mut store,
            miner_transaction("t2", 20),
            Timestamp(21),
        ))
        .unwrap_or_else(|failure| panic!("unexpected renewal failure: {failure}"));
        let effect_count = store.committed_effects().len();

        let outcome = run_ready(apply_refund_notification(
            &mut store,
            full_refund(miner_transaction("t1", 10), 30),
            Timestamp(30),
            None,
        ))
        .unwrap_or_else(|failure| panic!("unexpected refund failure: {failure}"));

        assert_eq!(outcome, DispatchOutcome::Applied { effects: vec![] });
        assert_eq!(store.committed_effects().len(), effect_count);
        assert_eq!(
            store
                .value()
                .transactions
                .get(&TransactionId::new("t1"))
                .map(|record| &record.refund_state),
            Some(&refund::State::Refunded)
        );
        assert_eq!(
            store
                .value()
                .miners
                .get(&ChainId::new("chain-1"))
                .map(|chain| &chain.state),
            Some(&miner::State::Bound)
        );
    }

    #[test]
    fn recovery_guard_change_preserves_state_and_effects() {
        let mut store = PurchaseStore::new(PurchaseState::default());
        run_ready(apply_transaction(
            &mut store,
            miner_transaction("t1", 10),
            Timestamp(15),
        ))
        .unwrap_or_else(|failure| panic!("unexpected purchase failure: {failure}"));
        let guard = run_ready(begin_refund_recovery(&store, &TransactionId::new("t1")))
            .unwrap_or_else(|failure| panic!("unexpected guard failure: {failure}"));

        run_ready(request_consumption(
            &mut store,
            &TransactionId::new("t1"),
            7,
        ))
        .unwrap_or_else(|failure| panic!("unexpected concurrent mutation failure: {failure}"));
        let state = store.value().clone();
        let effect_count = store.committed_effects().len();

        assert_eq!(
            run_ready(finish_refund_recovery(
                &mut store,
                guard,
                full_refund(miner_transaction("t1", 10), 20),
                Timestamp(20),
                None,
            )),
            Err(AppError::Store(StoreError::VersionConflict))
        );
        assert_eq!(store.value(), &state);
        assert_eq!(store.committed_effects().len(), effect_count);
    }
}
