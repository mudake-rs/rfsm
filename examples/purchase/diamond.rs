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

#[cfg(test)]
mod tests {
    use super::*;

    fn percentage(value: u32) -> RefundPercentage {
        RefundPercentage::new(value)
            .unwrap_or_else(|failure| panic!("invalid test percentage: {failure}"))
    }

    #[test]
    fn prorated_refund_floors_in_the_users_favor() {
        assert_eq!(refund_target(5, percentage(33_333)), 1);
    }

    #[test]
    fn refund_first_and_delivery_first_converge() {
        let grant = 2_000_000;
        let half = percentage(50_000);

        let mut refund_first = Diamonds::new(Rules);
        let applied = refund_first
            .process(Event::Deliver {
                grant,
                current_refund_percentage: Some(half),
            })
            .unwrap_or_else(|failure| panic!("unexpected delivery failure: {failure}"));
        let Some(refund_first_delivery) = applied.effect else {
            panic!("refund-first delivery emitted no value");
        };

        let mut delivery_first = Diamonds::new(Rules);
        let applied = delivery_first
            .process(Event::Deliver {
                grant,
                current_refund_percentage: None,
            })
            .unwrap_or_else(|failure| panic!("unexpected delivery failure: {failure}"));
        let Some(delivery_first_delivery) = applied.effect else {
            panic!("delivery-first purchase emitted no value");
        };
        let later_refund = adjust_refund(grant, 0, half, grant, grant)
            .unwrap_or_else(|failure| panic!("unexpected refund failure: {failure}"));

        assert_eq!(refund_first_delivery.credit, delivery_first_delivery.credit);
        assert_eq!(
            refund_first_delivery.refund_debit,
            later_refund.current_debit
        );
        assert_eq!(refund_first.state(), delivery_first.state());
    }

    #[test]
    fn duplicate_delivery_has_no_value_effect() {
        let mut machine = Diamonds::from_state(State::Delivered, Rules);
        let applied = machine
            .process(Event::Deliver {
                grant: 100,
                current_refund_percentage: None,
            })
            .unwrap_or_else(|failure| panic!("unexpected replay failure: {failure}"));
        assert_eq!(applied.transition, Transition::DuplicateDelivery);
        assert_eq!(applied.effect, None);
        assert_eq!(machine.state(), &State::Delivered);
    }

    #[test]
    fn stronger_refund_never_turns_an_existing_debit_into_credit() {
        let adjusted = adjust_refund(100, 50, percentage(100_000), 10, 10)
            .unwrap_or_else(|failure| panic!("unexpected adjustment failure: {failure}"));
        assert_eq!(adjusted.current_debit, 60);
        assert_eq!(adjusted.adjustment, Adjustment::Debit(10));
    }

    #[test]
    fn lower_percentage_credits_only_the_difference_and_reversal_credits_the_rest() {
        let weaker = adjust_refund(100, 80, percentage(50_000), 0, 0)
            .unwrap_or_else(|failure| panic!("unexpected adjustment failure: {failure}"));
        assert_eq!(weaker.current_debit, 50);
        assert_eq!(weaker.adjustment, Adjustment::Credit(30));
        assert_eq!(
            reverse_refund(weaker.current_debit),
            AdjustedDebit {
                current_debit: 0,
                adjustment: Adjustment::Credit(50),
            }
        );
    }

    #[test]
    fn refund_reversal_before_or_after_delivery_converges() {
        let grant = 100;
        let full = RefundPercentage::FULL;

        let reversed_before = Delivery {
            credit: grant,
            refund_debit: 0,
        };
        let delivered_refunded = Delivery {
            credit: grant,
            refund_debit: refund_target(grant, full),
        };
        let reversed_after = reverse_refund(delivered_refunded.refund_debit);

        assert_eq!(reversed_before.credit, delivered_refunded.credit);
        assert_eq!(reversed_before.refund_debit, reversed_after.current_debit);
    }
}
