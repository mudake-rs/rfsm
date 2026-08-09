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
dependency name in `Cargo.toml`.

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

Prefix a callback with `async` in the table to generate an async `process`
method:

```rust,ignore
Approved: Pending + Approve { reference } [async allowed(reference)]
    / audit(reference) => Approved { reference },
```

The crate has no async-runtime dependency. State commits after awaited
selection completes, so dropping a pending `process` future leaves state
unchanged. Work already performed inside a callback is not rolled back when
the future is dropped; externally visible writes belong in the returned effect
boundary.

## Serializing state

Enable the optional `serde` feature and opt in per machine to derive
`Serialize` and `Deserialize` for its generated `State` enum:

```toml
[dependencies]
rfsm = { version = "0.2.1", features = ["serde"] }
serde_json = "1"
```

```rust,ignore
machine! {
    name: Approvals,
    serde: true,
    states: { *Pending, Approved { reference: String } },
    events: { Approve { reference: String } },
    transitions: {
        Approved: Pending + Approve { reference } => Approved { reference },
        Approved { .. } + Approve { .. } => reject AlreadyApproved,
    }
}

let encoded = serde_json::to_string(machine.state())?;
let state: State = serde_json::from_str(&encoded)?;
let restored = Approvals::from_state(state);
```

Only active leaf state is serialized. Compound ancestry is derived from the
machine definition. `Machine`, context, events, transition results, and effects
are not serialized by this option. A machine with payload-bearing states can
opt in only when every state payload implements serde's traits.

The `serde_json` representation shown above is externally tagged and uses
generated variant and field names. Raw identifiers use their unraw spelling:
`r#Type` and `r#ref` are stored as `Type` and `ref`. Unknown JSON variants and
payload fields are rejected instead of being silently discarded. Other serde
formats may use a different representation; some binary formats encode enum
variants by declaration order. The application owns its serializer and stored
format.

Deserialization errors surface before `from_state`; constructing a machine from
restored state does not process an event or produce an effect.

Serde encodes state as bytes. It does not provide durable storage, schema
versioning, migrations, concurrency control, transactions, or effect delivery.

## Database-owned state

The database row and its concurrency version remain authoritative. Restore a
disposable machine from a snapshot, process the event without holding a
database lock, then conditionally publish the state and effect in one
application-owned transaction:

```rust,ignore
let snapshot = db.load_approval(id).await?;
let mut machine = Approvals::from_state(snapshot.state, facts);
let applied = machine.process(event).await?;

let mut tx = db.begin().await?;
tx.store_state_if_version(id, snapshot.version, machine.state())
    .await?;
if let Some(effect) = &applied.effect {
    tx.apply_effect(effect).await?;
}
tx.commit().await?;

Ok(applied)
```

The conditional state write must report a conflict when no row matches the
expected version. On conflict, failure, or cancellation before commit, discard
the transaction and machine. Return `Applied` from the application only after
the durable commit succeeds: it proves the local machine transition, not the
database commit. If the connection is lost during `COMMIT`, read the database
before retrying because the outcome may be unknown.

Typed effects can share a transaction with database writes. HTTP calls, email,
and other external systems are not made atomic by a database transaction and
need an application-owned delivery protocol. Automatic retries are safe only
when callbacks are read-only and retry-safe.

## Migrating from 0.1

Version 0.2 removes `Plan`, `Machine::evaluate`, and `Plan::confirm`. Restore a
disposable machine with `from_state`, call `process`, and publish
`machine.state()` plus `applied.effect` in the application transaction. The
`Applied` value is already the transition result; return it only after a
durable commit when the database is authoritative.

## Samples

- `cargo run --example door`
- `cargo run --example nested_order`
- `cargo test --example async_database`
- `cargo run --example purchase`
- `cargo test --example purchase`
- `cargo test --workspace --all-targets`

The current scope excludes persistence adapters and schema migrations,
entry/exit hooks, history, parallel regions, visualization, actor runtimes, and
executor abstractions.
