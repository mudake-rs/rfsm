// The runnable path demonstrates one flow through the composed domain model.
#![allow(dead_code)]

mod catalogue;
mod diamond;
mod miner;
mod model;
mod refund;
mod verify;

use std::error::Error;

use catalogue::{DIAMONDS, MINER_CURRENT, MINER_RETIRED, VIP};
use model::{Event, Purchase, Transition};
use verify::{
    ChainId, Ownership, Timestamp, TransactionId, UntrustedRefundOutcome, UntrustedRefundRevision,
    UntrustedRenewalSnapshot, UntrustedTransaction,
};

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
    let purchase_event = verify::verify_transaction(signed_purchase.clone())?;

    let mut purchase = Purchase::default();
    let purchased = purchase.process(Event::Transaction(purchase_event))?;

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
    let renewal = purchase.process(Event::RenewalSnapshot(renewal_snapshot))?;

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
    let refunded = purchase.process(Event::RefundNotification(refund))?;

    let mut recovered_transaction = signed_purchase;
    recovered_transaction.signed_at = Timestamp(22);
    let recovered = verify::verify_recovered_active(recovered_transaction)?;
    let restored = purchase.process(Event::RecoveredStatus(recovered))?;

    let chain = purchase
        .miner(&chain_id)
        .ok_or("the sample Miner chain was not persisted")?
        .clone();
    assert_eq!(chain.state, miner::State::Bound);
    assert!(chain.state.is_in(miner::StateId::Tracking));
    assert_eq!(
        miner::pending_product(chain.head.as_ref(), chain.renewal.as_ref()),
        Some(MINER_RETIRED)
    );
    let exact = purchase
        .transaction(&transaction_id)
        .ok_or("the sample exact transaction was not retained")?;
    assert!(exact.refund_state.is_in(refund::StateId::Effective));

    let diamond_id = TransactionId::new("transaction-2");
    let signed_diamonds = UntrustedTransaction {
        signature_valid: true,
        bundle_id: verify::EXPECTED_BUNDLE.to_owned(),
        environment: verify::EXPECTED_ENVIRONMENT.to_owned(),
        transaction_id: diamond_id.clone(),
        chain_id: ChainId::new("transaction-2"),
        product_id: DIAMONDS.as_str().to_owned(),
        signed_at: Timestamp(30),
        purchased_at: Timestamp(30),
        expires_at: None,
        quantity: Some(1),
        ownership: Ownership::Purchased,
    };
    let refund_first = verify::verify_refund(UntrustedRefundRevision {
        signature_valid: true,
        bundle_id: verify::EXPECTED_BUNDLE.to_owned(),
        environment: verify::EXPECTED_ENVIRONMENT.to_owned(),
        transaction: signed_diamonds.clone(),
        signed_at: Timestamp(31),
        outcome: UntrustedRefundOutcome::Prorated {
            revoked_at: Timestamp(31),
            percentage_milliunits: 50_000,
        },
    })?;
    purchase.process(Event::RefundNotification(refund_first))?;
    let delivered = purchase.process(Event::Transaction(verify::verify_transaction(
        signed_diamonds,
    )?))?;
    let delivery = delivered
        .transitions
        .iter()
        .find_map(|transition| match transition {
            Transition::Diamond(applied) => applied.effect,
            _ => None,
        })
        .ok_or("Diamond delivery did not return its catalogue grant")?;
    assert_eq!(delivery.grant, 2_000_000);
    assert_eq!(
        delivery.refund_percentage.map(|value| value.milliunits()),
        Some(50_000)
    );
    assert_eq!(
        purchase
            .diamond(&diamond_id)
            .ok_or("Diamond delivery state was not retained")?
            .state,
        diamond::State::Delivered
    );

    let floor = purchase.process(Event::VipFloorChanged {
        expires_at: Some(Timestamp(100)),
    })?;
    assert_eq!(purchase.vip_expires_at(), Some(Timestamp(100)));

    let signed_vip = UntrustedTransaction {
        signature_valid: true,
        bundle_id: verify::EXPECTED_BUNDLE.to_owned(),
        environment: verify::EXPECTED_ENVIRONMENT.to_owned(),
        transaction_id: TransactionId::new("transaction-3"),
        chain_id: ChainId::new("transaction-3"),
        product_id: VIP.as_str().to_owned(),
        signed_at: Timestamp(40),
        purchased_at: Timestamp(40),
        expires_at: Some(Timestamp(120)),
        quantity: None,
        ownership: Ownership::Purchased,
    };
    let vip_purchase = purchase.process(Event::Transaction(verify::verify_transaction(
        signed_vip.clone(),
    )?))?;
    assert_eq!(purchase.vip_expires_at(), Some(Timestamp(120)));
    assert!(vip_purchase.transitions.iter().any(|transition| matches!(
        transition,
        Transition::VipProjectionChanged {
            from: Some(Timestamp(100)),
            to: Some(Timestamp(120))
        }
    )));

    let vip_refund = verify::verify_refund(UntrustedRefundRevision {
        signature_valid: true,
        bundle_id: verify::EXPECTED_BUNDLE.to_owned(),
        environment: verify::EXPECTED_ENVIRONMENT.to_owned(),
        transaction: signed_vip,
        signed_at: Timestamp(41),
        outcome: UntrustedRefundOutcome::Full {
            revoked_at: Timestamp(41),
            percentage_milliunits: 100_000,
        },
    })?;
    let vip_refunded = purchase.process(Event::RefundNotification(vip_refund))?;
    assert_eq!(purchase.vip_expires_at(), Some(Timestamp(100)));

    assert!(purchased.changed);
    assert!(renewal.changed);
    assert!(refunded.changed);
    assert!(restored.changed);
    assert!(floor.changed);
    assert!(vip_refunded.changed);

    println!(
        "purchase={purchased:?} renewal={renewal:?} refund={refunded:?} reversal={restored:?}"
    );
    println!("refund-first delivery={delivered:?}");
    println!("vip purchase={vip_purchase:?} refund={vip_refunded:?}");
    println!("chain={chain:?}");
    Ok(())
}
