# fsm

`fsm` is an ergonomic finite state machine library prototype for Rust. The
current vertical slice evaluates one API hypothesis: application code owns the
current state, while `Machine` owns pure transition and hierarchy rules.

This is not a final architecture selection. Payload-bearing compound states,
multiple differently shaped compound branches, guard semantics, terminal
states, and awaited dispatch remain explicit design questions.

## Samples

- `cargo run --example door` — complete flat machine.
- `cargo run --example nested_order` — initial-child resolution, child-first
  propagation, parent handling, refusal, and leaving a compound state.
- `cargo test --example async_database` — executor-neutral database planning,
  durable state-plus-effect commit, and caller confirmation.

The crate has no runtime dependencies. Async application code uses the same
pure `plan_from` operation as synchronous code; the application owns awaited
work, cancellation residue, and transaction commit.
