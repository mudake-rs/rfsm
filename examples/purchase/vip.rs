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
