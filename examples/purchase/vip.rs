use crate::verify::Timestamp;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AppleVipPeriod {
    pub expires_at: Timestamp,
    pub active: bool,
}

pub fn effective_expiry(
    floor: Option<Timestamp>,
    apple_periods: impl IntoIterator<Item = AppleVipPeriod>,
) -> Option<Timestamp> {
    apple_periods
        .into_iter()
        .filter(|period| period.active)
        .map(|period| period.expires_at)
        .chain(floor)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refund_removes_only_its_exact_apple_period() {
        let periods = [
            AppleVipPeriod {
                expires_at: Timestamp(30),
                active: false,
            },
            AppleVipPeriod {
                expires_at: Timestamp(20),
                active: true,
            },
        ];
        assert_eq!(
            effective_expiry(Some(Timestamp(10)), periods),
            Some(Timestamp(20))
        );
    }

    #[test]
    fn non_apple_floor_survives_refund_and_reversal_restores_maximum() {
        let refunded = [AppleVipPeriod {
            expires_at: Timestamp(30),
            active: false,
        }];
        assert_eq!(
            effective_expiry(Some(Timestamp(25)), refunded),
            Some(Timestamp(25))
        );

        let reversed = [AppleVipPeriod {
            expires_at: Timestamp(30),
            active: true,
        }];
        assert_eq!(
            effective_expiry(Some(Timestamp(25)), reversed),
            Some(Timestamp(30))
        );
    }
}
