# Apple purchase domain model

This example models the complete stateful core of a production Apple in-app
purchase workflow. The main entry point is one synchronous domain call:

```rust,ignore
let outcome = purchase.process(Event::Transaction(verified_transaction))?;
```

`Purchase` routes the verified event to the state machine that owns the fact.
It does not contain database transactions, async adapters, CAS, outbox delivery,
HTTP types, or App Store client calls. Those mechanisms persist or transport a
domain transition; they do not select it.

The returned `Outcome` keeps `changed` separate from the selected transitions.
Each child transition retains its name, `from`, `to`, and typed effect. Accepted
no-ops therefore remain distinguishable from stale notifications and duplicate
exact transactions.

Run the example with:

```console
cargo run --example purchase
```

## Complete model

```text
Purchase::process(event)
│
├── exact Apple transaction ──> Refunds
│                              ├── Live
│                              │   ├── Effective
│                              │   │   ├── None
│                              │   │   ├── Requested
│                              │   │   ├── Declined
│                              │   │   └── Reversed
│                              │   └── Refunded
│                              └── LegacyRevoked
│
├── Miner subscription chain ─> MinerChain
│                              ├── Unbound
│                              └── Tracking
│                                  ├── Bound
│                                  └── Refunded
│
├── exact Diamond delivery ───> Undelivered | Delivered
│
└── VIP projection ───────────> max(value-effective Apple periods, local floor)
```

The nested states are domain partitions, not presentation groups. A refund
event handled by `Live` applies to every live leaf. A consumption request
handled by `None` takes precedence over the fallback inherited from `Live`.
`State::is_in(StateId::Effective)` lets the VIP projection consume the same
partition without reconstructing a machine or repeating the leaf list.

The three machines are composed rather than placed below one artificial root
state. They have different owners and clocks, and they can change
independently. A Diamond refund may exist before delivery. A Miner chain head
and its renewal snapshot use different ordering clocks. Encoding these facts
as one active leaf would require a Cartesian product and invent ordering that
Apple does not provide.

## Owners and clocks

| Model | Owner | Ordering rule |
|---|---|---|
| `refund::Refunds` | one exact Apple transaction | `refund_signed_at` |
| `miner::MinerChain` | one subscription chain | purchase date for the head; `renewal_signed_at` for its snapshot |
| `diamond::Diamonds` | one exact delivery record | value is delivered once |
| VIP projection | user view over exact transactions | maximum active expiry |

An application may persist these owners in separate rows. It should load the
affected states, run the same transition selection, and commit the returned
domain changes atomically. Database locks, retries, unknown commit outcomes,
and external effect delivery stay application responsibilities.

## Stateful behavior

### Exact transaction

- `Live::Effective` is the value-granting refund partition.
- `Refunded` withdraws value but remains a recoverable live Apple state.
- `LegacyRevoked` rejects every event and requires manual repair.
- A newer signed refund revision replaces the current revision.
- An older revision is stale. An equal identical revision is a duplicate; an
  equal contradictory revision is rejected.
- A decline cannot supersede a completed refund.
- Active recovery maps `Refunded` to `Reversed` and otherwise preserves the
  effective leaf.
- A consumption request only changes `None` to `Requested`. Repeats are
  accepted no-ops and do not advance the refund clock; transport auditing stays
  outside the state graph.

### Miner chain

- The first accepted paid transaction enters `Tracking::Bound`.
- A later purchase date advances the head and clears the previous renewal
  snapshot.
- Equal purchase dates for different transactions are ambiguous; transaction
  ID is not used as a tie-breaker.
- A retained predecessor snapshot is stale. A snapshot for an unknown head is
  rejected because the current Apple chain must be recovered first.
- Snapshot revisions use their own signed clock. Equal contradictory snapshots
  are rejected.
- Refunding the current head enters `Tracking::Refunded`; refunding a
  predecessor leaves the current leaf unchanged.
- Reversing that same head returns to `Tracking::Bound`.
- Paid expiry and signed grace determine access. Billing retry and automatic
  renewal target are snapshot facts, not extra states.

### Diamonds and VIP

- Diamond delivery records the complete catalogue grant once.
- Refund-first and delivery-first order converge on the same two owner states:
  the exact refund lifecycle and the delivery-once gate.
- The delivery transition returns the catalogue grant and the current refund
  percentage. The application can persist value and reconcile a prior refund
  without moving ledger policy into the FSM.
- Balance arithmetic, spend evidence, and ledger adjustments are application
  policy and deliberately do not appear in the FSM model.
- VIP is a maximum projection, not another state machine. Refunding one period
  reveals the next value-effective period or the non-Apple floor.

## Verification boundary

`verify.rs` admits only typed facts to the model. It preserves the Apple rules
that affect state semantics:

- valid signature marker, exact bundle, and exact environment;
- closed product catalogue, including retained product IDs;
- purchased ownership while Family Sharing remains unsupported;
- subscription expiry and consumable quantity requirements;
- bounded prorated percentage and exact full-refund percentage;
- Miner-only renewal targets and grace later than paid expiry;
- immutable exact transaction identity.

The Apple JWS signing time is not part of immutable transaction identity. A
recovery fetch may re-sign the same exact transaction; only the refund and
renewal revision clocks order their respective state.

Real JWS cryptography is deliberately replaced by `signature_valid`. The
sample also excludes account deletion and restoration, purchase intents,
mining leases and pools, Diamond lot accounting, notification UUID transport
idempotency, and external delivery. None changes the transition ownership or
state hierarchy shown above.

## Field-test result

The full model fits `rfsm` as composed machines with honest hierarchical
partitions; parallel regions are unnecessary. The field test added one small
ergonomic capability: a persisted generated `State` can now answer `is_in`
directly. Application code no longer needs a disposable machine merely to ask
whether a stored leaf belongs to a compound state.
