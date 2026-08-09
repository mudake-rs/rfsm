use rfsm::machine;

use crate::verify::RefundPercentage;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Delivery {
    pub grant: u64,
    pub refund_percentage: Option<RefundPercentage>,
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
            refund_percentage: Option<RefundPercentage>,
        },
    },
    transitions: {
        DeliveredOnce: Undelivered + Deliver { grant, refund_percentage }
            / deliver(grant, refund_percentage) => Delivered,
        DuplicateDelivery: Delivered + Deliver { .. } => _,
    }
}

impl DiamondsContext for Rules {
    fn deliver(&self, grant: &u64, refund_percentage: &Option<RefundPercentage>) -> Delivery {
        Delivery {
            grant: *grant,
            refund_percentage: *refund_percentage,
        }
    }
}
