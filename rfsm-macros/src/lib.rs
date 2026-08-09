//! Proc-macro implementation for `rfsm`.
#![forbid(unsafe_code)]

mod expand;
mod model;
mod parse;
mod validate;

use proc_macro::TokenStream;

/// Defines a finite state machine from one state tree and transition table.
///
/// `*` marks the initial state at the root and inside each compound state.
/// Accepted rows have a stable transition label. Rejection rows stop
/// propagation without changing state. Transition labels must be unique.
///
/// ```text
/// machine! {
///     name: Door,
///     states: { *Closed, Open },
///     events: { Open, Close },
///     transitions: {
///         Opened: Closed + Open => Open,
///         Open + Open => reject AlreadyOpen,
///     }
/// }
/// ```
///
/// `context` is required only for machines with callbacks. `effect` is
/// required only when an effect factory is used. Rejection reasons are
/// collected into a generated `Rejection` enum.
/// Set `serde: true` with the `rfsm/serde` Cargo feature to derive serde's
/// `Serialize` and `Deserialize` traits for the generated `State` enum.
///
/// A guard is written as `[guard(arguments)]`; an effect factory is written as
/// `/ effect(arguments)`. Prefix either callback with `async` to generate an
/// async `process` method. Callbacks borrow context and payload bindings
/// immutably, but interior or external side effects are not rolled back on
/// rejection or async cancellation. Callbacks must be logically read-only and
/// cancellation-safe.
///
/// Rows are selected from the active leaf through its ancestors, then from
/// wildcard `_` sources. Declaration order applies within one level. A failed
/// guard falls through; an explicit rejection stops selection. `=> _` accepts
/// a stay transition.
///
/// One invocation emits the fixed names `State`, `StateId`, `Event`,
/// `Transition`, and `Rejection`; define separate machines in separate modules.
/// Generated code expects the runtime dependency to be available under the
/// name `rfsm`.
#[proc_macro]
pub fn machine(input: TokenStream) -> TokenStream {
    let definition = syn::parse_macro_input!(input as model::MachineDef);
    match validate::validate(&definition).and_then(|model| expand::expand(&definition, &model)) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}
