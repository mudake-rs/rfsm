use std::error::Error;
use std::fmt::{self, Display, Formatter};

use crate::catalogue::{self, Product};

pub const EXPECTED_BUNDLE: &str = "com.example.app";
pub const EXPECTED_ENVIRONMENT: &str = "Production";

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TransactionId(pub String);

impl TransactionId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ChainId(pub String);

impl ChainId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Timestamp(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefundPercentage(u32);

impl RefundPercentage {
    pub const FULL: Self = Self(100_000);

    pub fn new(milliunits: u32) -> Result<Self, VerificationError> {
        if milliunits <= 100_000 {
            Ok(Self(milliunits))
        } else {
            Err(VerificationError::InvalidRefundPercentage)
        }
    }

    pub const fn milliunits(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ownership {
    Purchased,
    FamilyShared,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedTransaction {
    pub signature_valid: bool,
    pub bundle_id: String,
    pub environment: String,
    pub transaction_id: TransactionId,
    pub chain_id: ChainId,
    pub product_id: String,
    pub signed_at: Timestamp,
    pub purchased_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub quantity: Option<u32>,
    pub ownership: Ownership,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionIdentity {
    pub transaction_id: TransactionId,
    pub chain_id: ChainId,
    pub product_id: catalogue::ProductId,
    pub purchased_at: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerifiedTransaction {
    Miner {
        identity: TransactionIdentity,
        expires_at: Timestamp,
    },
    Vip {
        identity: TransactionIdentity,
        expires_at: Timestamp,
    },
    Diamonds {
        identity: TransactionIdentity,
        amount: u64,
    },
}

impl VerifiedTransaction {
    pub fn identity(&self) -> &TransactionIdentity {
        match self {
            Self::Miner { identity, .. }
            | Self::Vip { identity, .. }
            | Self::Diamonds { identity, .. } => identity,
        }
    }
}

pub fn verify_transaction(
    input: UntrustedTransaction,
) -> Result<VerifiedTransaction, VerificationError> {
    verify_envelope(input.signature_valid, &input.bundle_id, &input.environment)?;
    if input.ownership == Ownership::FamilyShared {
        return Err(VerificationError::FamilySharingUnsupported);
    }

    let product = catalogue::lookup(&input.product_id).ok_or(VerificationError::UnknownProduct)?;
    let identity = TransactionIdentity {
        transaction_id: input.transaction_id,
        chain_id: input.chain_id,
        product_id: match product {
            Product::Miner { id } | Product::Vip { id } | Product::Diamonds { id, .. } => id,
        },
        purchased_at: input.purchased_at,
    };

    match product {
        Product::Miner { .. } => {
            let expires_at = input
                .expires_at
                .ok_or(VerificationError::MissingSubscriptionExpiry)?;
            Ok(VerifiedTransaction::Miner {
                identity,
                expires_at,
            })
        }
        Product::Vip { .. } => {
            let expires_at = input
                .expires_at
                .ok_or(VerificationError::MissingSubscriptionExpiry)?;
            Ok(VerifiedTransaction::Vip {
                identity,
                expires_at,
            })
        }
        Product::Diamonds { amount, .. } => {
            if input.expires_at.is_some() {
                return Err(VerificationError::ConsumableHasExpiry);
            }
            if input.quantity != Some(1) {
                return Err(VerificationError::InvalidConsumableQuantity);
            }
            Ok(VerifiedTransaction::Diamonds { identity, amount })
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UntrustedRefundOutcome {
    Full {
        revoked_at: Timestamp,
        percentage_milliunits: u32,
    },
    Prorated {
        revoked_at: Timestamp,
        percentage_milliunits: u32,
    },
    Declined,
    Reversed,
    FamilyRevoke {
        revoked_at: Timestamp,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedRefundRevision {
    pub signature_valid: bool,
    pub bundle_id: String,
    pub environment: String,
    pub transaction: UntrustedTransaction,
    pub signed_at: Timestamp,
    pub outcome: UntrustedRefundOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerifiedRefundOutcome {
    Refunded {
        revoked_at: Timestamp,
        percentage: RefundPercentage,
    },
    Declined,
    Reversed,
    Active,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedRefundRevision {
    pub(crate) transaction: VerifiedTransaction,
    pub(crate) signed_at: Timestamp,
    pub(crate) outcome: VerifiedRefundOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedRenewalSnapshot {
    pub(crate) chain_id: ChainId,
    pub(crate) transaction_id: TransactionId,
    pub(crate) signed_at: Timestamp,
    pub(crate) auto_renew_product_id: Option<catalogue::ProductId>,
    pub(crate) billing_retry: bool,
    pub(crate) grace_until: Option<Timestamp>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedRenewalSnapshot {
    pub signature_valid: bool,
    pub bundle_id: String,
    pub environment: String,
    pub transaction: UntrustedTransaction,
    pub signed_at: Timestamp,
    pub auto_renew_product_id: Option<String>,
    pub billing_retry: bool,
    pub grace_until: Option<Timestamp>,
}

pub fn verify_refund(
    input: UntrustedRefundRevision,
) -> Result<VerifiedRefundRevision, VerificationError> {
    verify_envelope(input.signature_valid, &input.bundle_id, &input.environment)?;
    let transaction = verify_transaction(input.transaction)?;
    let outcome = match input.outcome {
        UntrustedRefundOutcome::Full {
            revoked_at,
            percentage_milliunits,
        } => {
            let percentage = RefundPercentage::new(percentage_milliunits)?;
            if percentage != RefundPercentage::FULL {
                return Err(VerificationError::FullRefundIsNotFull);
            }
            VerifiedRefundOutcome::Refunded {
                revoked_at,
                percentage,
            }
        }
        UntrustedRefundOutcome::Prorated {
            revoked_at,
            percentage_milliunits,
        } => VerifiedRefundOutcome::Refunded {
            revoked_at,
            percentage: RefundPercentage::new(percentage_milliunits)?,
        },
        UntrustedRefundOutcome::Declined => VerifiedRefundOutcome::Declined,
        UntrustedRefundOutcome::Reversed => VerifiedRefundOutcome::Reversed,
        UntrustedRefundOutcome::FamilyRevoke { .. } => {
            return Err(VerificationError::FamilySharingUnsupported);
        }
    };
    Ok(VerifiedRefundRevision {
        transaction,
        signed_at: input.signed_at,
        outcome,
    })
}

pub fn verify_recovered_active(
    transaction: UntrustedTransaction,
) -> Result<VerifiedRefundRevision, VerificationError> {
    let signed_at = transaction.signed_at;
    let transaction = verify_transaction(transaction)?;
    Ok(VerifiedRefundRevision {
        transaction,
        signed_at,
        outcome: VerifiedRefundOutcome::Active,
    })
}

pub fn verify_renewal(
    input: UntrustedRenewalSnapshot,
) -> Result<VerifiedRenewalSnapshot, VerificationError> {
    verify_envelope(input.signature_valid, &input.bundle_id, &input.environment)?;

    let transaction = verify_transaction(input.transaction)?;
    let VerifiedTransaction::Miner {
        identity,
        expires_at,
        ..
    } = transaction
    else {
        return Err(VerificationError::RenewalTransactionIsNotMiner);
    };
    let auto_renew_product_id = input
        .auto_renew_product_id
        .map(|product_id| match catalogue::lookup(&product_id) {
            Some(Product::Miner { id }) => Ok(id),
            _ => Err(VerificationError::InvalidRenewalProduct),
        })
        .transpose()?;
    if input.grace_until.is_some_and(|grace| grace <= expires_at) {
        return Err(VerificationError::InvalidGraceDeadline);
    }

    Ok(VerifiedRenewalSnapshot {
        chain_id: identity.chain_id,
        transaction_id: identity.transaction_id,
        signed_at: input.signed_at,
        auto_renew_product_id,
        billing_retry: input.billing_retry,
        grace_until: input.grace_until,
    })
}

fn verify_envelope(
    signature_valid: bool,
    bundle_id: &str,
    environment: &str,
) -> Result<(), VerificationError> {
    if !signature_valid {
        return Err(VerificationError::InvalidSignature);
    }
    if bundle_id != EXPECTED_BUNDLE {
        return Err(VerificationError::WrongBundle);
    }
    if environment != EXPECTED_ENVIRONMENT {
        return Err(VerificationError::WrongEnvironment);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationError {
    InvalidSignature,
    WrongBundle,
    WrongEnvironment,
    UnknownProduct,
    FamilySharingUnsupported,
    MissingSubscriptionExpiry,
    ConsumableHasExpiry,
    InvalidConsumableQuantity,
    InvalidRefundPercentage,
    FullRefundIsNotFull,
    RenewalTransactionIsNotMiner,
    InvalidRenewalProduct,
    InvalidGraceDeadline,
}

impl Display for VerificationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "Apple verification failed: {self:?}")
    }
}

impl Error for VerificationError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::{DIAMONDS, MINER_RETIRED};

    fn input(product_id: &str) -> UntrustedTransaction {
        UntrustedTransaction {
            signature_valid: true,
            bundle_id: EXPECTED_BUNDLE.to_owned(),
            environment: EXPECTED_ENVIRONMENT.to_owned(),
            transaction_id: TransactionId::new("tx-1"),
            chain_id: ChainId::new("chain-1"),
            product_id: product_id.to_owned(),
            signed_at: Timestamp(10),
            purchased_at: Timestamp(10),
            expires_at: Some(Timestamp(20)),
            quantity: None,
            ownership: Ownership::Purchased,
        }
    }

    fn refund_input(outcome: UntrustedRefundOutcome) -> UntrustedRefundRevision {
        UntrustedRefundRevision {
            signature_valid: true,
            bundle_id: EXPECTED_BUNDLE.to_owned(),
            environment: EXPECTED_ENVIRONMENT.to_owned(),
            transaction: input(MINER_RETIRED.as_str()),
            signed_at: Timestamp(30),
            outcome,
        }
    }

    #[test]
    fn retired_subscription_remains_accepted_with_exact_identity() {
        let verified = verify_transaction(input(MINER_RETIRED.as_str()))
            .unwrap_or_else(|failure| panic!("unexpected verification failure: {failure}"));
        assert_eq!(verified.identity().product_id, MINER_RETIRED);
    }

    #[test]
    fn consumable_requires_one_item_and_no_expiry() {
        let mut invalid = input(DIAMONDS.as_str());
        invalid.expires_at = None;
        invalid.quantity = Some(2);
        assert_eq!(
            verify_transaction(invalid),
            Err(VerificationError::InvalidConsumableQuantity)
        );
    }

    #[test]
    fn provider_identity_is_checked_before_catalogue_or_value() {
        let mut invalid = input(MINER_RETIRED.as_str());
        invalid.signature_valid = false;
        assert_eq!(
            verify_transaction(invalid),
            Err(VerificationError::InvalidSignature)
        );

        let mut wrong_bundle = input(MINER_RETIRED.as_str());
        wrong_bundle.bundle_id = "com.example.other".to_owned();
        assert_eq!(
            verify_transaction(wrong_bundle),
            Err(VerificationError::WrongBundle)
        );
    }

    #[test]
    fn full_refund_requires_the_full_percentage() {
        assert_eq!(
            verify_refund(refund_input(UntrustedRefundOutcome::Full {
                revoked_at: Timestamp(25),
                percentage_milliunits: 50_000,
            })),
            Err(VerificationError::FullRefundIsNotFull)
        );
    }

    #[test]
    fn prorated_and_declined_outcomes_are_typed_before_dispatch() {
        let prorated = verify_refund(refund_input(UntrustedRefundOutcome::Prorated {
            revoked_at: Timestamp(25),
            percentage_milliunits: 50_000,
        }))
        .unwrap_or_else(|failure| panic!("unexpected verification failure: {failure}"));
        assert!(matches!(
            prorated.outcome,
            VerifiedRefundOutcome::Refunded { percentage, .. }
                if percentage.milliunits() == 50_000
        ));

        let declined = verify_refund(refund_input(UntrustedRefundOutcome::Declined))
            .unwrap_or_else(|failure| panic!("unexpected verification failure: {failure}"));
        assert_eq!(declined.outcome, VerifiedRefundOutcome::Declined);
    }

    #[test]
    fn family_revoke_is_rejected_before_mutation() {
        assert_eq!(
            verify_refund(refund_input(UntrustedRefundOutcome::FamilyRevoke {
                revoked_at: Timestamp(25),
            })),
            Err(VerificationError::FamilySharingUnsupported)
        );
    }

    #[test]
    fn refund_envelope_is_verified_before_its_signed_date_is_trusted() {
        let mut refund = refund_input(UntrustedRefundOutcome::Declined);
        refund.signature_valid = false;
        assert_eq!(
            verify_refund(refund),
            Err(VerificationError::InvalidSignature)
        );
    }

    #[test]
    fn renewal_requires_a_miner_target_and_future_grace() {
        let renewal = |target: Option<&str>, grace_until| UntrustedRenewalSnapshot {
            signature_valid: true,
            bundle_id: EXPECTED_BUNDLE.to_owned(),
            environment: EXPECTED_ENVIRONMENT.to_owned(),
            transaction: input(MINER_RETIRED.as_str()),
            signed_at: Timestamp(30),
            auto_renew_product_id: target.map(str::to_owned),
            billing_retry: true,
            grace_until,
        };

        assert_eq!(
            verify_renewal(renewal(Some(DIAMONDS.as_str()), None)),
            Err(VerificationError::InvalidRenewalProduct)
        );
        assert_eq!(
            verify_renewal(renewal(Some(MINER_RETIRED.as_str()), Some(Timestamp(20)),)),
            Err(VerificationError::InvalidGraceDeadline)
        );
        assert!(
            verify_renewal(renewal(Some(MINER_RETIRED.as_str()), Some(Timestamp(21)),)).is_ok()
        );
    }
}
