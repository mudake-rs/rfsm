// Alternate Apple paths are exercised by the example's specification tests.
#![cfg_attr(not(test), allow(dead_code))]

mod app;
mod catalogue;
mod diamond;
mod miner;
mod refund;
mod store;
mod verify;
mod vip;

use std::error::Error;
use std::future::Future;
use std::task::{Context, Poll, Waker};

use app::{PurchaseState, PurchaseStore};
use catalogue::{MINER_CURRENT, MINER_RETIRED};
use verify::{
    ChainId, Ownership, Timestamp, TransactionId, UntrustedRefundOutcome, UntrustedRefundRevision,
    UntrustedRenewalSnapshot, UntrustedTransaction,
};

fn run_ready<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("the in-memory sample unexpectedly suspended"),
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let transaction_id = TransactionId::new("transaction-1");
    let chain_id = ChainId::new("chain-1");
    let signed_purchase = UntrustedTransaction {
        signature_valid: true,
        bundle_id: verify::EXPECTED_BUNDLE.to_owned(),
        environment: verify::EXPECTED_ENVIRONMENT.to_owned(),
        transaction_id: transaction_id.clone(),
        chain_id: chain_id.clone(),
        product_id: MINER_CURRENT.as_str().to_owned(),
        signed_at: Timestamp(10),
        purchased_at: Timestamp(10),
        expires_at: Some(Timestamp(30)),
        quantity: None,
        ownership: Ownership::Purchased,
    };
    let purchase = verify::verify_transaction(signed_purchase.clone())?;

    let mut store = PurchaseStore::new(PurchaseState::default());
    let purchased = run_ready(app::apply_transaction(&mut store, purchase, Timestamp(11)))?;

    let renewal_snapshot = verify::verify_renewal(UntrustedRenewalSnapshot {
        signature_valid: true,
        bundle_id: verify::EXPECTED_BUNDLE.to_owned(),
        environment: verify::EXPECTED_ENVIRONMENT.to_owned(),
        transaction: signed_purchase.clone(),
        signed_at: Timestamp(20),
        auto_renew_product_id: Some(MINER_RETIRED.as_str().to_owned()),
        billing_retry: true,
        grace_until: Some(Timestamp(35)),
    })?;
    let renewal = run_ready(app::apply_renewal_snapshot(
        &mut store,
        renewal_snapshot,
        Timestamp(20),
    ))?;

    let refund = verify::verify_refund(UntrustedRefundRevision {
        signature_valid: true,
        bundle_id: verify::EXPECTED_BUNDLE.to_owned(),
        environment: verify::EXPECTED_ENVIRONMENT.to_owned(),
        transaction: signed_purchase.clone(),
        signed_at: Timestamp(21),
        outcome: UntrustedRefundOutcome::Full {
            revoked_at: Timestamp(21),
            percentage_milliunits: 100_000,
        },
    })?;
    let refunded = run_ready(app::apply_refund_notification(
        &mut store,
        refund,
        Timestamp(21),
        None,
    ))?;

    let reversal = verify::verify_refund(UntrustedRefundRevision {
        signature_valid: true,
        bundle_id: verify::EXPECTED_BUNDLE.to_owned(),
        environment: verify::EXPECTED_ENVIRONMENT.to_owned(),
        transaction: signed_purchase,
        signed_at: Timestamp(22),
        outcome: UntrustedRefundOutcome::Reversed,
    })?;
    let restored = run_ready(app::apply_refund_notification(
        &mut store,
        reversal,
        Timestamp(22),
        None,
    ))?;

    let chain = store
        .value()
        .miners
        .get(&chain_id)
        .ok_or("the sample Miner chain was not persisted")?;
    assert_eq!(chain.state, miner::State::Bound);
    assert_eq!(
        miner::pending_product(chain.head.as_ref(), chain.renewal.as_ref()),
        Some(MINER_RETIRED)
    );

    println!(
        "purchase={purchased:?} renewal={renewal:?} refund={refunded:?} reversal={restored:?}"
    );
    println!("durable_version={} chain={chain:?}", store.version());
    Ok(())
}
