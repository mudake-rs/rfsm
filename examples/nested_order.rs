use std::error::Error;

use rfsm::{ProcessError, machine};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChargeId(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Effect {
    RecordCharge(ChargeId),
    RefundCharge(ChargeId),
}

struct Facts {
    may_cancel: bool,
}

machine! {
    name: Orders,
    context: Facts,
    effect: Effect,

    states: {
        *Draft,
        Payment {
            *Authorizing,
            Authorized { charge_id: ChargeId },
        },
        Cancelled,
        Completed,
    },
    events: {
        BeginPayment,
        Authorize { charge_id: ChargeId },
        Cancel,
        Complete,
    },

    transitions: {
        BeganPayment: Draft + BeginPayment => Payment,
        AuthorizedPayment: Authorizing + Authorize { charge_id }
            / record_charge(charge_id) => Authorized { charge_id },
        Authorized { .. } + Authorize { .. } => reject AlreadyAuthorized,
        CancelledPayment: Authorized { charge_id } + Cancel [may_cancel]
            / refund_charge(charge_id) => Cancelled,
        CancelledUnchargedPayment: Authorizing + Cancel [may_cancel] => Cancelled,
        Payment + Cancel => reject CancellationBlocked,
        CompletedOrder: Authorized { .. } + Complete => Completed,
        _ + _ => reject InvalidEvent,
    }
}

impl OrdersContext for Facts {
    fn may_cancel(&self) -> bool {
        self.may_cancel
    }

    fn record_charge(&self, charge_id: &ChargeId) -> Effect {
        Effect::RecordCharge(*charge_id)
    }

    fn refund_charge(&self, charge_id: &ChargeId) -> Effect {
        Effect::RefundCharge(*charge_id)
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let charge_id = ChargeId(7);
    let mut orders = Orders::new(Facts { may_cancel: true });

    let began = orders.process(Event::BeginPayment)?;
    assert_eq!(began.to, State::Authorizing);
    assert_eq!(orders.state(), &State::Authorizing);
    assert!(orders.is_in(StateId::Payment));

    let authorized = orders.process(Event::Authorize { charge_id })?;
    assert_eq!(authorized.effect, Some(Effect::RecordCharge(charge_id)));
    assert_eq!(orders.state(), &State::Authorized { charge_id });

    let duplicate = orders.process(Event::Authorize { charge_id });
    assert_eq!(
        duplicate,
        Err(ProcessError::Rejected(Rejection::AlreadyAuthorized))
    );
    assert_eq!(orders.state(), &State::Authorized { charge_id });

    let cancelled = orders.process(Event::Cancel)?;
    assert_eq!(cancelled.effect, Some(Effect::RefundCharge(charge_id)));
    assert_eq!(orders.state(), &State::Cancelled);

    let mut blocked = Orders::from_state(State::Authorizing, Facts { may_cancel: false });
    assert_eq!(
        blocked.process(Event::Cancel),
        Err(ProcessError::Rejected(Rejection::CancellationBlocked))
    );
    assert_eq!(blocked.state(), &State::Authorizing);

    println!("cancelled={cancelled:?}");
    Ok(())
}
