# rfsm

`rfsm` defines a finite state machine as one state tree and one transition
table. The macro generates the state, event, transition, rejection, and machine
types.

```rust
use rfsm::machine;

machine! {
    name: Door,
    states: { *Closed, Open },
    events: { Open, Close },

    transitions: {
        Opened: Closed + Open => Open,
        ClosedAgain: Open + Close => Closed,
        Open + Open => reject AlreadyOpen,
        Closed + Close => reject AlreadyClosed,
    }
}

let mut door = Door::new();
let applied = door.process(Event::Open)?;

assert_eq!(door.state(), &State::Open);
assert_eq!(applied.transition, Transition::Opened);
# Ok::<(), Box<dyn std::error::Error>>(())
```

An accepted row is `Transition: State + Event => Target`. A rejection has no
transition label because nothing commits. Optional guards use `[guard]`; typed
effect factories use `/ effect`. Every accepted row has a unique transition
label. Use `=> _` for an accepted stay transition that keeps the active state
and still returns an `Applied` result.

`context:` is required only when the table uses guard or effect callbacks.
`effect:` is required only when a `/ effect` callback is present. Rejection
reasons are collected from `reject Reason` rows into the generated `Rejection`
enum.

## Generated scope

Each `machine!` invocation generates `State`, `StateId`, `Event`, `Transition`,
and `Rejection` in its scope. Put each machine in its own module so these
short, call-site-friendly names do not collide:

```rust,ignore
mod door {
    use rfsm::machine;

    machine! { /* one complete machine */ }
}

mod order {
    use rfsm::machine;

    machine! { /* another complete machine */ }
}
```

Generated code currently refers to the runtime crate as `rfsm`, so keep that
dependency name in `Cargo.toml` in the 0.1 series.

## Nested states

Nesting is declared directly. The application does not implement parent maps,
initial-child lookup, or bubbling.

```rust,ignore
states: {
    *Draft,
    Payment {
        *Authorizing,
        Authorized { charge_id: ChargeId },
    },
    Cancelled,
}

transitions: {
    BeganPayment: Draft + BeginPayment => Payment,
    AuthorizedPayment: Authorizing + Authorize { charge_id }
        / record_charge(charge_id) => Authorized { charge_id },
    Authorized { .. } + Authorize { .. } => reject AlreadyAuthorized,
    Payment + Cancel => reject CancellationBlocked,
}
```

## Selection semantics

Selection is deterministic:

1. Try rows for the active leaf, then its ancestors from nearest to farthest.
2. Within one hierarchy level, try rows in declaration order.
3. Try `_` source rows only after the entire ancestor chain, wherever those
   rows appear in the table.

`_` matches any source or event in that position. A failed guard falls through
to the next eligible row. An explicit rejection stops selection immediately.
If every matching row has a failed guard and no fallback handles the event,
`process` returns `ProcessError::Unhandled`; the error carries `StateId` rather
than cloning a payload-bearing state.

Targeting `Payment` recursively enters its `*Authorizing` initial leaf.
`machine.is_in(StateId::Payment)` inspects ancestry.

Leaf states and events may carry named payloads. Compound states are dataless
tree nodes and cannot become active states.

## Guards and effects

Referenced callbacks form a generated `<Name>Context` trait:

```rust,ignore
impl OrdersContext for Facts {
    fn may_cancel(&self) -> bool {
        self.may_cancel
    }

    fn record_charge(&self, charge_id: &ChargeId) -> Effect {
        Effect::RecordCharge(*charge_id)
    }
}
```

Callbacks receive `&Context`, but Rust interior mutability and external handles
can still produce side effects. Callbacks must therefore be logically
read-only and cancellation-safe. They should compute typed effect data;
external writes stay in application code. Rejection never changes machine
state. The crate does not snapshot or roll back context or external resources.

Prefix a callback with `async` in the table to generate async `evaluate` and
`process` methods:

```rust,ignore
Approved: Pending + Approve { reference } [async allowed(reference)]
    / audit(reference) => Approved { reference },
```

The crate has no async-runtime dependency. State commits after awaited
selection completes, so dropping a pending `process` future leaves state
unchanged. Work already performed inside a callback is not rolled back when
the future is dropped; externally visible writes belong in the returned effect
boundary.

## Database-owned state

`process` is the direct in-memory path. `evaluate` selects the same rule without
mutating state, for an application-owned transaction:

```rust,ignore
let plan = Approvals::evaluate(&row.state, &event, &facts).await?;

let mut tx = db.begin().await?;
tx.store_state(&plan.to).await?;
if let Some(effect) = &plan.effect {
    tx.apply_effect(effect).await?;
}
tx.commit().await?;

let applied = plan.confirm();
```

The database row remains authoritative. `confirm` is the caller's assertion
that its durable transaction succeeded.

## Samples

- `cargo run --example door`
- `cargo run --example nested_order`
- `cargo test --example async_database`
- `cargo test --workspace --all-targets`

The current scope excludes persistence formats, entry/exit hooks, history,
parallel regions, visualization, actor runtimes, and executor abstractions.
