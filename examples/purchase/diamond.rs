use std::error::Error;
use std::fmt::{self, Display, Formatter};

use rfsm::machine;

use crate::verify::RefundPercentage;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Delivery {
    pub credit: u64,
    pub refund_debit: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Rules;

machine! {
    name: Diamonds,
    context: Rules,
    effect: Delivery,
    states: { *Undelivered, Delivered },
    events: {
        Deliver {
            grant: u64,
            current_refund_percentage: Option<RefundPercentage>,
        },
    },
    transitions: {
        DeliveredOnce: Undelivered + Deliver { grant, current_refund_percentage }
            / deliver(grant, current_refund_percentage) => Delivered,
        DuplicateDelivery: Delivered + Deliver { .. } => _,
    }
}

impl DiamondsContext for Rules {
    fn deliver(
        &self,
        grant: &u64,
        current_refund_percentage: &Option<RefundPercentage>,
    ) -> Delivery {
        Delivery {
            credit: *grant,
            refund_debit: current_refund_percentage
                .map_or(0, |percentage| refund_target(*grant, percentage)),
        }
    }
}

pub fn refund_target(grant: u64, percentage: RefundPercentage) -> u64 {
    let numerator = u128::from(grant) * u128::from(percentage.milliunits());
    (numerator / 100_000) as u64
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Adjustment {
    None,
    Debit(u64),
    Credit(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdjustedDebit {
    pub current_debit: u64,
    pub adjustment: Adjustment,
}

pub fn adjust_refund(
    grant: u64,
    current_debit: u64,
    percentage: RefundPercentage,
    remaining_unspent: u64,
    balance: u64,
) -> Result<AdjustedDebit, MathError> {
    if current_debit > grant {
        return Err(MathError::DebitExceedsGrant);
    }
    let target = refund_target(grant, percentage);
    if target >= current_debit {
        let debit = (target - current_debit).min(remaining_unspent).min(balance);
        Ok(AdjustedDebit {
            current_debit: current_debit + debit,
            adjustment: if debit == 0 {
                Adjustment::None
            } else {
                Adjustment::Debit(debit)
            },
        })
    } else {
        let credit = current_debit - target;
        Ok(AdjustedDebit {
            current_debit: target,
            adjustment: Adjustment::Credit(credit),
        })
    }
}

pub fn reverse_refund(current_debit: u64) -> AdjustedDebit {
    AdjustedDebit {
        current_debit: 0,
        adjustment: if current_debit == 0 {
            Adjustment::None
        } else {
            Adjustment::Credit(current_debit)
        },
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MathError {
    DebitExceedsGrant,
}

impl Display for MathError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("the exact Apple refund debit exceeds its catalogue grant")
    }
}

impl Error for MathError {}
