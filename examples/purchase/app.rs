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
