# Apple purchase field test

This example extracts the state semantics from a production Apple in-app
purchase model without copying its HTTP, database, mining, or notification
infrastructure. It is deliberately larger than a tutorial: its tests are an
executable specification for delayed, duplicated, and conflicting provider
facts.

Run it with:

```console
cargo run --example purchase
cargo test --example purchase
```

## Ownership

There is no useful single "purchase machine." The durable owner and ordering
clock determine each machine boundary:

| Model | Durable owner | Ordering rule |
|---|---|---|
| `refund::Refunds` | one exact Apple transaction | `refund_signed_at` |
| `miner::MinerChain` | one subscription chain | head purchase date and `renewal_signed_at` |
| `diamond::Diamonds` | one exact consumable transaction | value is delivered once |
| `vip::effective_expiry` | user projection | maximum active expiry; not an FSM |

The refund and renewal clocks are intentionally stored in different records
and never compared. A refund changes the exact transaction first. `app.rs`
then projects that accepted fact into the relevant family.

## Processing boundary

```text
untrusted Apple payload
        |
        v
signature / bundle / environment / exact catalogue checks
        |
        v
typed verified event
        |
        v
load durable snapshot -> run disposable machine -> stage state and effects
        |
        v
versioned commit
        |
        v
deliver committed effect records
```

`verify.rs` represents the provider boundary. It does not implement JWS
cryptography; `signature_valid` stands in for a successful verifier so the
sample can focus on the facts permitted past that boundary.

`store.rs` is an in-memory versioned transaction. A real application replaces
it with its database transaction. The application remains responsible for
schema migration, compare-and-set publication, unknown commit outcomes, and a
durable delivery protocol for external effects.

Recovery is split into `begin_refund_recovery` and
`finish_refund_recovery`. The Apple request occurs between those calls, while
no database transaction is open. The second call rejects a changed guard and
publishes nothing.

## Preserved Apple rules

- Product IDs are a closed exact catalogue. Retired IDs remain accepted while
  signed renewals, refunds, and restores can reference them.
- Signature, bundle, environment, ownership, product family, required expiry,
  consumable quantity, and refund percentage are checked before mutation.
- Family Sharing revocation is rejected while Family Sharing is unsupported.
- Exact transaction ID is the value-once business key. An identical replay is
  unchanged; contradictory immutable facts are an error.
- A newer refund revision replaces the exact row. An older revision is stale,
  an equal identical revision is a duplicate, and an equal contradictory
  revision is rejected. A decline cannot replace a completed refund. The
  verified revision carries its exact signed transaction, so the caller cannot
  retarget it to another row. Active status recovery advances the same clock
  and maps only a refunded row to `Reversed`. Equal-clock replay compares the
  projected durable status and refund fields, not whether the fact arrived by
  notification or recovery.
- A consumption request is always audited, changes only `None` to `Requested`,
  and does not advance the refund clock.
- A later paid Miner transaction advances the exact head. Equal purchase dates
  for different transactions are ambiguous; lexical transaction ID does not
  break the tie.
- A renewal snapshot attaches only to its exact head. A retained predecessor is
  stale; an unknown mismatch requires recovery. Advancing the head clears the
  old snapshot.
- Refunding the current Miner transaction ends access without moving the head.
  A superseded refund changes only its exact transaction audit. Reversal
  restores only that same head and publishes active access only while paid or
  signed grace time remains.
- Paid expiry and signed grace derive effective access. Apple expiry
  notifications are not modeled as state transitions.
- Diamond delivery records the full catalogue grant once. Refund-first and
  delivery-first order converge on the same prorated debit. A stronger refund
  receives the application-owned post-delivery spend aggregate and debits only
  proven remaining value, a weaker refund credits only the difference, and
  reversal credits the current exact debit.
- VIP is `max(non-refunded Apple periods, non-Apple floor)`. Modeling it as a
  state machine would add state without adding a transition invariant.
- Transactional effects are staged with state. Rejection, replay, rollback,
  and version conflict publish neither state nor effects.

## Deliberate cuts

The sample excludes product prices, real product IDs, purchase-intent
placement, mining leases and pools, rewards and badges, Diamond lot accounting,
account tombstones, HTTP types, Redis, websocket emitters, migrations, and the
real App Store Server API client. Notification UUID idempotency is a transport
delivery concern; this sample keeps the value-once transaction key. Account
owner resolution, deletion, and restore are excluded entirely; recovery is a
different Apple status operation. A real application must keep an event with
no live owner retryable until explicit restore establishes ownership.

Only a Diamond refund may create an exact audit row before initial delivery in
this sample. Pre-delivery Miner and VIP refund guards are excluded, matching
the accepted first-release exposure in the source model. Diamond spend
aggregation is also not implemented here: the caller passes one proven
post-delivery aggregate from its ledger transaction. That boundary is required;
substituting total balance or `grant - current_debit` is incorrect when spend is
shared across purchases.

## Field-test result

The model fits the public `rfsm` 0.2 API without a library change. The useful
parts are typed rejections, named accepted no-ops, deterministic row ordering,
and disposable-machine publication around a database-owned row.

The main ergonomic cost is visible in `refund.rs`: an ordered provider revision
needs explicit rows for duplicate, equal conflict, newer, and stale outcomes.
This is repetitive but keeps every outcome named. Source and event patterns
also cannot rename a bound field, so realistic payloads use an `incoming_`
prefix where stored and incoming field names would otherwise collide. That is
a candidate macro improvement only if more domains reproduce the need.
