use std::collections::HashMap;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Ident;

use crate::model::{
    BindingPattern, Callable, CallableRole, FlatState, MachineDef, Row, RowEvent, RowOutcome,
    RowSource, StateNode, TargetState, VariantDef, flatten_states,
};
use crate::validate::{CallableDef, Validated, ident_key};

pub fn expand(definition: &MachineDef, validated: &Validated) -> syn::Result<TokenStream> {
    let flat_states = flatten_states(&definition.states);
    let state_by_name: HashMap<String, &FlatState<'_>> = flat_states
        .iter()
        .map(|state| (ident_key(&state.node.variant.name), state))
        .collect();
    let event_by_name: HashMap<String, &VariantDef> = definition
        .events
        .iter()
        .map(|event| (ident_key(&event.name), event))
        .collect();

    let initial = definition
        .states
        .iter()
        .find(|state| state.initial)
        .ok_or_else(|| syn::Error::new(definition.name.span(), "missing root initial state"))?;
    let initial_state = initial_expression(initial)?;

    let name = &definition.name;
    let effect = definition.effect.as_ref().map_or_else(
        || quote!(::core::convert::Infallible),
        |effect| quote!(#effect),
    );
    let context_trait = format_ident!("{}Context", name);
    let asyncness = validated.is_async.then(|| quote!(async));
    let await_selection = validated.is_async.then(|| quote!(.await));

    let leaf_variants = flat_states
        .iter()
        .filter(|state| !state.node.is_compound())
        .map(|state| declaration_variant(&state.node.variant));
    let state_id_variants = flat_states.iter().map(|state| &state.node.variant.name);
    let event_variants = definition.events.iter().map(declaration_variant);
    let transition_variants = &validated.transitions;
    let rejection_variants = &validated.rejections;
    let context_methods: Vec<TokenStream> = validated
        .callables
        .iter()
        .map(|callable| context_method(callable, &effect))
        .collect();
    let context_trait_item = if context_methods.is_empty() {
        quote!()
    } else {
        quote! {
            #[doc = "Logically read-only guards and effect factories used by the transition table."]
            #[doc = "Interior or external side effects are not rolled back on rejection or cancellation."]
            #[doc = "Externally visible writes belong in the returned effect boundary."]
            #[allow(async_fn_in_trait, clippy::ptr_arg, missing_docs)]
            pub trait #context_trait {
                #(#context_methods)*
            }
        }
    };

    let state_id_arms = flat_states
        .iter()
        .filter(|state| !state.node.is_compound())
        .map(|state| {
            let variant = &state.node.variant;
            let name = &variant.name;
            let pattern = ignored_variant_pattern(quote!(State), variant);
            quote!(#pattern => StateId::#name,)
        });
    let parent_arms = flat_states.iter().map(|state| {
        let name = &state.node.variant.name;
        match state.parent {
            Some(parent) => {
                quote!(StateId::#name => ::core::option::Option::Some(StateId::#parent),)
            }
            None => quote!(StateId::#name => ::core::option::Option::None,),
        }
    });

    let mut level_arms = Vec::new();
    for state in &flat_states {
        let state_name = &state.node.variant.name;
        let blocks = definition
            .rows
            .iter()
            .filter(|row| {
                matches!(
                    &row.source,
                    RowSource::State(pattern)
                        if ident_key(&pattern.name) == ident_key(state_name)
                )
            })
            .map(|row| {
                row_block(
                    row,
                    definition,
                    &context_trait,
                    &state_by_name,
                    &event_by_name,
                )
            })
            .collect::<syn::Result<Vec<_>>>()?;
        level_arms.push(quote! {
            StateId::#state_name => {
                #(#blocks)*
            }
        });
    }
    let wildcard_blocks = definition
        .rows
        .iter()
        .filter(|row| matches!(row.source, RowSource::Any))
        .map(|row| {
            row_block(
                row,
                definition,
                &context_trait,
                &state_by_name,
                &event_by_name,
            )
        })
        .collect::<syn::Result<Vec<_>>>()?;

    let (context_field, constructors, selection_context, process_context) =
        match &definition.context {
            Some(context) => (
                quote!(context: #context,),
                quote! {
                    pub fn new(context: #context) -> Self {
                        Self {
                            state: #initial_state,
                            context,
                        }
                    }

                    pub fn from_state(state: State, context: #context) -> Self {
                        Self { state, context }
                    }

                    pub fn context(&self) -> &#context {
                        &self.context
                    }

                    pub fn context_mut(&mut self) -> &mut #context {
                        &mut self.context
                    }
                },
                quote!(, context: &#context),
                quote!(, &self.context),
            ),
            None => (
                quote!(),
                quote! {
                    pub fn new() -> Self {
                        Self {
                            state: #initial_state,
                        }
                    }

                    pub fn from_state(state: State) -> Self {
                        Self { state }
                    }
                },
                quote!(),
                quote!(),
            ),
        };
    let use_context = definition
        .context
        .is_some()
        .then(|| quote!(let _ = context;));

    Ok(quote! {
        #[allow(missing_docs)]
        #[derive(Clone, Debug, PartialEq)]
        pub enum State {
            #(#leaf_variants),*
        }

        #[allow(missing_docs)]
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum StateId {
            #(#state_id_variants),*
        }

        #[allow(missing_docs)]
        #[derive(Clone, Debug, PartialEq)]
        pub enum Event {
            #(#event_variants),*
        }

        #[allow(missing_docs)]
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum Transition {
            #(#transition_variants),*
        }

        #[allow(missing_docs)]
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum Rejection {
            #(#rejection_variants),*
        }

        #context_trait_item

        #[allow(missing_docs)]
        pub struct #name {
            state: State,
            #context_field
        }

        #[allow(missing_docs)]
        impl #name {
            #constructors

            pub fn state(&self) -> &State {
                &self.state
            }

            pub fn state_id(&self) -> StateId {
                Self::__state_id(&self.state)
            }

            pub fn is_in(&self, ancestor: StateId) -> bool {
                let mut current = ::core::option::Option::Some(self.state_id());
                while let ::core::option::Option::Some(state) = current {
                    if state == ancestor {
                        return true;
                    }
                    current = Self::__parent(state);
                }
                false
            }

            #[allow(irrefutable_let_patterns, unreachable_code)]
            #asyncness fn __select(
                state: &State,
                event: &Event
                #selection_context
            ) -> ::core::result::Result<
                (Transition, State, ::core::option::Option<#effect>),
                ::rfsm::ProcessError<StateId, Event, Rejection>,
            > {
                #use_context
                let from_id = Self::__state_id(state);
                let mut level = ::core::option::Option::Some(from_id);
                while let ::core::option::Option::Some(at) = level {
                    match at {
                        #(#level_arms),*
                    }
                    level = Self::__parent(at);
                }

                #(#wildcard_blocks)*

                ::core::result::Result::Err(::rfsm::ProcessError::Unhandled {
                    state: from_id,
                    event: event.clone(),
                })
            }

            pub #asyncness fn process(
                &mut self,
                event: Event,
            ) -> ::core::result::Result<
                ::rfsm::Applied<State, Transition, #effect>,
                ::rfsm::ProcessError<StateId, Event, Rejection>,
            > {
                let (transition, to, effect) =
                    Self::__select(&self.state, &event #process_context)#await_selection?;
                let from = self.state.clone();
                self.state = to.clone();
                ::core::result::Result::Ok(::rfsm::Applied {
                    transition,
                    from,
                    to,
                    effect,
                })
            }

            fn __state_id(state: &State) -> StateId {
                match state {
                    #(#state_id_arms)*
                }
            }

            fn __parent(state: StateId) -> ::core::option::Option<StateId> {
                match state {
                    #(#parent_arms)*
                }
            }
        }
    })
}

fn declaration_variant(variant: &VariantDef) -> TokenStream {
    let name = &variant.name;
    if variant.fields.is_empty() {
        quote!(#name)
    } else {
        let fields = variant.fields.iter().map(|field| {
            let name = &field.name;
            let ty = &field.ty;
            quote!(#name: #ty)
        });
        quote!(#name { #(#fields),* })
    }
}

fn context_method(callable: &CallableDef, effect: &TokenStream) -> TokenStream {
    let name = &callable.name;
    let asyncness = callable.is_async.then(|| quote!(async));
    let arguments = callable
        .arguments
        .iter()
        .map(|(name, ty)| quote!(#name: &#ty));
    let output = match callable.role {
        CallableRole::Guard => quote!(bool),
        CallableRole::Effect => quote!(#effect),
    };
    quote!(#asyncness fn #name(&self, #(#arguments),*) -> #output;)
}

fn row_block(
    row: &Row,
    definition: &MachineDef,
    context_trait: &Ident,
    states: &HashMap<String, &FlatState<'_>>,
    events: &HashMap<String, &VariantDef>,
) -> syn::Result<TokenStream> {
    let source_pattern = match &row.source {
        RowSource::Any => None,
        RowSource::State(pattern) => {
            let state = states.get(&ident_key(&pattern.name)).ok_or_else(|| {
                syn::Error::new(pattern.name.span(), "validated source state disappeared")
            })?;
            if state.node.is_compound() {
                None
            } else {
                Some(binding_pattern(quote!(State), pattern, &state.node.variant))
            }
        }
    };
    let event_pattern = match &row.event {
        RowEvent::Any => None,
        RowEvent::Event(pattern) => {
            let event = events.get(&ident_key(&pattern.name)).ok_or_else(|| {
                syn::Error::new(pattern.name.span(), "validated event disappeared")
            })?;
            Some(binding_pattern(quote!(Event), pattern, event))
        }
    };

    let body = selected_row_body(row, definition, context_trait, states)?;
    let guarded = match &row.guard {
        Some(guard) => {
            let call = callback_call(guard, definition, context_trait)?;
            quote! {
                if #call {
                    #body
                }
            }
        }
        None => body,
    };

    Ok(match (source_pattern, event_pattern) {
        (Some(source), Some(event)) => quote! {
            if let (#source, #event) = (state, event) {
                #guarded
            }
        },
        (Some(source), None) => quote! {
            if let #source = state {
                #guarded
            }
        },
        (None, Some(event)) => quote! {
            if let #event = event {
                #guarded
            }
        },
        (None, None) => guarded,
    })
}

fn selected_row_body(
    row: &Row,
    definition: &MachineDef,
    context_trait: &Ident,
    states: &HashMap<String, &FlatState<'_>>,
) -> syn::Result<TokenStream> {
    match &row.outcome {
        RowOutcome::Reject(rejection) => Ok(quote! {
            return ::core::result::Result::Err(::rfsm::ProcessError::Rejected(
                Rejection::#rejection
            ));
        }),
        RowOutcome::Transition { transition, target } => {
            let target = match target {
                Some(target) => target_expression(target, states)?,
                None => quote!(state.clone()),
            };
            let effect_ty = definition.effect.as_ref().map_or_else(
                || quote!(::core::convert::Infallible),
                |effect| quote!(#effect),
            );
            let effect = match &row.effect {
                Some(callable) => {
                    let call = callback_call(callable, definition, context_trait)?;
                    quote!(::core::option::Option::Some(#call))
                }
                None => quote!(::core::option::Option::None),
            };
            Ok(quote! {
                let to = #target;
                let effect: ::core::option::Option<#effect_ty> = #effect;
                return ::core::result::Result::Ok((Transition::#transition, to, effect));
            })
        }
    }
}

fn callback_call(
    callable: &Callable,
    definition: &MachineDef,
    context_trait: &Ident,
) -> syn::Result<TokenStream> {
    let context = definition.context.as_ref().ok_or_else(|| {
        syn::Error::new(
            callable.name.span(),
            "validated callback is missing its context type",
        )
    })?;
    let name = &callable.name;
    let arguments = &callable.arguments;
    let await_call = callable.is_async.then(|| quote!(.await));
    Ok(quote!(<#context as #context_trait>::#name(context, #(#arguments),*)#await_call))
}

fn binding_pattern(
    prefix: TokenStream,
    pattern: &BindingPattern,
    variant: &VariantDef,
) -> TokenStream {
    let name = &pattern.name;
    if variant.fields.is_empty() {
        return quote!(#prefix::#name);
    }
    if !pattern.explicit_fields {
        return quote!(#prefix::#name { .. });
    }

    let fields = &pattern.fields;
    if pattern.rest {
        if fields.is_empty() {
            quote!(#prefix::#name { .. })
        } else {
            quote!(#prefix::#name { #(#fields),*, .. })
        }
    } else {
        quote!(#prefix::#name { #(#fields),* })
    }
}

fn ignored_variant_pattern(prefix: TokenStream, variant: &VariantDef) -> TokenStream {
    let name = &variant.name;
    if variant.fields.is_empty() {
        quote!(#prefix::#name)
    } else {
        quote!(#prefix::#name { .. })
    }
}

fn target_expression(
    target: &TargetState,
    states: &HashMap<String, &FlatState<'_>>,
) -> syn::Result<TokenStream> {
    let state = states
        .get(&ident_key(&target.name))
        .ok_or_else(|| syn::Error::new(target.name.span(), "validated target state disappeared"))?;
    if state.node.is_compound() {
        return initial_expression(state.node);
    }

    let name = &target.name;
    if state.node.variant.fields.is_empty() {
        return Ok(quote!(State::#name));
    }

    let fields_by_name: HashMap<String, &syn::Type> = state
        .node
        .variant
        .fields
        .iter()
        .map(|field| (ident_key(&field.name), &field.ty))
        .collect();
    let fields = target
        .fields
        .iter()
        .map(|field| {
            let name = &field.name;
            let binding = &field.binding;
            let ty = fields_by_name.get(&ident_key(name)).ok_or_else(|| {
                syn::Error::new(name.span(), "validated target field disappeared")
            })?;
            Ok(quote!(#name: <#ty as ::core::clone::Clone>::clone(#binding)))
        })
        .collect::<syn::Result<Vec<_>>>()?;
    Ok(quote!(State::#name { #(#fields),* }))
}

fn initial_expression(node: &StateNode) -> syn::Result<TokenStream> {
    let mut current = node;
    while current.is_compound() {
        current = current
            .children
            .iter()
            .find(|child| child.initial)
            .ok_or_else(|| {
                syn::Error::new(
                    current.variant.name.span(),
                    "validated compound initial state disappeared",
                )
            })?;
    }
    let name = &current.variant.name;
    Ok(quote!(State::#name))
}
