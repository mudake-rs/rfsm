use std::collections::{HashMap, HashSet};

use quote::ToTokens;
use syn::ext::IdentExt;
use syn::{Ident, Type};

use crate::model::{
    BindingPattern, Callable, CallableRole, FieldDef, FlatState, MachineDef, Row, RowEvent,
    RowOutcome, RowSource, StateNode, TargetState, VariantDef, flatten_states,
};

pub struct CallableDef {
    pub name: Ident,
    pub role: CallableRole,
    pub is_async: bool,
    pub arguments: Vec<(Ident, Type)>,
}

pub struct Validated {
    pub callables: Vec<CallableDef>,
    pub transitions: Vec<Ident>,
    pub rejections: Vec<Ident>,
    pub is_async: bool,
}

pub fn validate(definition: &MachineDef) -> syn::Result<Validated> {
    let mut errors = Vec::new();
    let flat_states = flatten_states(&definition.states);

    validate_declarations(definition, &flat_states, &mut errors);

    let states: HashMap<String, &FlatState<'_>> = flat_states
        .iter()
        .map(|state| (state.node.variant.name.to_string(), state))
        .collect();
    let events: HashMap<String, &VariantDef> = definition
        .events
        .iter()
        .map(|event| (event.name.to_string(), event))
        .collect();

    let mut callables = Vec::new();
    let mut transitions = Vec::new();
    let mut transition_names = HashSet::new();
    let mut rejections = Vec::new();
    let mut rejection_names = HashSet::new();

    for row in &definition.rows {
        let bindings = validate_row(row, &states, &events, &mut errors);

        match &row.outcome {
            RowOutcome::Transition { transition, .. } => {
                if transition_names.insert(transition.to_string()) {
                    transitions.push(transition.clone());
                } else {
                    errors.push(syn::Error::new(
                        transition.span(),
                        format!("duplicate transition label `{transition}`"),
                    ));
                }
            }
            RowOutcome::Reject(rejection) => {
                if rejection_names.insert(rejection.unraw().to_string()) {
                    rejections.push(rejection.clone());
                }
            }
        }

        if let Some(guard) = &row.guard {
            note_callable(
                guard,
                CallableRole::Guard,
                &bindings,
                &mut callables,
                &mut errors,
            );
        }
        if let Some(effect) = &row.effect {
            note_callable(
                effect,
                CallableRole::Effect,
                &bindings,
                &mut callables,
                &mut errors,
            );
        }
    }

    if !flat_states.is_empty() && !definition.events.is_empty() {
        validate_coverage(definition, &flat_states, &mut errors);
    }

    if definition.context.is_none() {
        if let Some(callable) = callables.first() {
            errors.push(syn::Error::new(
                callable.name.span(),
                "`context` is required when transition callbacks are used",
            ));
        }
    }
    if definition.effect.is_none() {
        if let Some(callable) = callables
            .iter()
            .find(|callable| callable.role == CallableRole::Effect)
        {
            errors.push(syn::Error::new(
                callable.name.span(),
                format!(
                    "`effect` is required because effect callback `{}` is used",
                    callable.name
                ),
            ));
        }
    }

    if errors.is_empty() {
        let is_async = callables.iter().any(|callable| callable.is_async);
        Ok(Validated {
            callables,
            transitions,
            rejections,
            is_async,
        })
    } else {
        Err(combine(errors))
    }
}

fn validate_declarations<'a>(
    definition: &'a MachineDef,
    flat_states: &[FlatState<'a>],
    errors: &mut Vec<syn::Error>,
) {
    if definition.states.is_empty() {
        errors.push(syn::Error::new(
            definition.name.span(),
            "at least one state is required",
        ));
    }
    if definition.events.is_empty() {
        errors.push(syn::Error::new(
            definition.name.span(),
            "at least one event is required",
        ));
    }

    let mut state_names = HashSet::new();
    for state in flat_states {
        let name = &state.node.variant.name;
        if !state_names.insert(name.to_string()) {
            errors.push(syn::Error::new(
                name.span(),
                format!("duplicate state `{name}`"),
            ));
        }
        validate_fields(&state.node.variant, "state", errors);
    }

    let mut event_names = HashSet::new();
    for event in &definition.events {
        if !event_names.insert(event.name.to_string()) {
            errors.push(syn::Error::new(
                event.name.span(),
                format!("duplicate event `{}`", event.name),
            ));
        }
        validate_fields(event, "event", errors);
    }

    validate_initial_group(&definition.states, "machine root", errors);
}

fn validate_fields(variant: &VariantDef, kind: &str, errors: &mut Vec<syn::Error>) {
    let mut names = HashSet::new();
    for field in &variant.fields {
        if !names.insert(field.name.to_string()) {
            errors.push(syn::Error::new(
                field.name.span(),
                format!(
                    "duplicate field `{}` in {kind} `{}`",
                    field.name, variant.name
                ),
            ));
        }
    }
}

fn validate_initial_group(nodes: &[StateNode], owner: &str, errors: &mut Vec<syn::Error>) {
    let initial: Vec<&StateNode> = nodes.iter().filter(|node| node.initial).collect();
    match initial.as_slice() {
        [] => {
            let span = nodes
                .first()
                .map_or(proc_macro2::Span::call_site(), |node| {
                    node.variant.name.span()
                });
            errors.push(syn::Error::new(
                span,
                format!("{owner} requires exactly one `*` initial state"),
            ));
        }
        [node] => {
            if let Some(leaf) = resolve_initial_leaf(node) {
                if !leaf.variant.fields.is_empty() {
                    errors.push(syn::Error::new(
                        leaf.variant.name.span(),
                        "an initial leaf cannot require payload fields",
                    ));
                }
            }
        }
        _ => {
            for node in initial.iter().skip(1) {
                errors.push(syn::Error::new(
                    node.variant.name.span(),
                    format!("{owner} has more than one `*` initial state"),
                ));
            }
        }
    }

    for node in nodes.iter().filter(|node| node.is_compound()) {
        validate_initial_group(
            &node.children,
            &format!("compound state `{}`", node.variant.name),
            errors,
        );
    }
}

fn resolve_initial_leaf(mut node: &StateNode) -> Option<&StateNode> {
    while node.is_compound() {
        node = node.children.iter().find(|child| child.initial)?;
    }
    Some(node)
}

fn validate_row<'a>(
    row: &Row,
    states: &HashMap<String, &'a FlatState<'a>>,
    events: &HashMap<String, &'a VariantDef>,
    errors: &mut Vec<syn::Error>,
) -> HashMap<String, &'a FieldDef> {
    let mut bindings = HashMap::new();

    match &row.source {
        RowSource::Any => {}
        RowSource::State(pattern) => match states.get(&pattern.name.to_string()) {
            Some(state) if state.node.is_compound() => {
                if pattern.explicit_fields {
                    errors.push(syn::Error::new(
                        pattern.name.span(),
                        "compound source states cannot bind payload fields",
                    ));
                }
            }
            Some(state) => {
                validate_pattern(pattern, &state.node.variant, "state", &mut bindings, errors)
            }
            None => errors.push(syn::Error::new(
                pattern.name.span(),
                format!("unknown source state `{}`", pattern.name),
            )),
        },
    }

    match &row.event {
        RowEvent::Any => {}
        RowEvent::Event(pattern) => match events.get(&pattern.name.to_string()) {
            Some(event) => validate_pattern(pattern, event, "event", &mut bindings, errors),
            None => errors.push(syn::Error::new(
                pattern.name.span(),
                format!("unknown event `{}`", pattern.name),
            )),
        },
    }

    match &row.outcome {
        RowOutcome::Transition {
            target: Some(target),
            ..
        } => validate_target(target, states, &bindings, errors),
        RowOutcome::Transition { target: None, .. } | RowOutcome::Reject(_) => {}
    }

    bindings
}

fn validate_pattern<'a>(
    pattern: &BindingPattern,
    variant: &'a VariantDef,
    kind: &str,
    bindings: &mut HashMap<String, &'a FieldDef>,
    errors: &mut Vec<syn::Error>,
) {
    if !pattern.explicit_fields {
        return;
    }

    let fields: HashMap<String, &FieldDef> = variant
        .fields
        .iter()
        .map(|field| (field.name.to_string(), field))
        .collect();
    let mut seen = HashSet::new();

    for binding in &pattern.fields {
        let name = binding.to_string();
        if !seen.insert(name.clone()) {
            errors.push(syn::Error::new(
                binding.span(),
                format!("duplicate pattern field `{binding}`"),
            ));
            continue;
        }

        let Some(field) = fields.get(&name) else {
            errors.push(syn::Error::new(
                binding.span(),
                format!("unknown field `{binding}` on {kind} `{}`", variant.name),
            ));
            continue;
        };

        if let Some(previous) = bindings.insert(name.clone(), field) {
            errors.push(syn::Error::new(
                binding.span(),
                format!(
                    "binding `{binding}` is ambiguous between `{}` and `{}`",
                    previous.name, field.name
                ),
            ));
        }
    }

    if !pattern.rest && pattern.fields.len() != variant.fields.len() {
        errors.push(syn::Error::new(
            pattern.name.span(),
            format!(
                "pattern for {kind} `{}` must bind every field or end with `..`",
                variant.name
            ),
        ));
    }
}

fn validate_target<'a>(
    target: &TargetState,
    states: &HashMap<String, &'a FlatState<'a>>,
    bindings: &HashMap<String, &'a FieldDef>,
    errors: &mut Vec<syn::Error>,
) {
    let Some(state) = states.get(&target.name.to_string()) else {
        errors.push(syn::Error::new(
            target.name.span(),
            format!("unknown target state `{}`", target.name),
        ));
        return;
    };

    if state.node.is_compound() {
        if target.explicit_fields {
            errors.push(syn::Error::new(
                target.name.span(),
                "compound target states cannot carry payload fields",
            ));
        }
        return;
    }

    let fields: HashMap<String, &FieldDef> = state
        .node
        .variant
        .fields
        .iter()
        .map(|field| (field.name.to_string(), field))
        .collect();

    if fields.is_empty() {
        if target.explicit_fields && !target.fields.is_empty() {
            errors.push(syn::Error::new(
                target.name.span(),
                format!("target state `{}` has no payload fields", target.name),
            ));
        }
        return;
    }

    if !target.explicit_fields {
        errors.push(syn::Error::new(
            target.name.span(),
            format!("target state `{}` requires payload fields", target.name),
        ));
        return;
    }

    let mut seen = HashSet::new();
    for target_field in &target.fields {
        if !seen.insert(target_field.name.to_string()) {
            errors.push(syn::Error::new(
                target_field.name.span(),
                format!("duplicate target field `{}`", target_field.name),
            ));
        }
        if !fields.contains_key(&target_field.name.to_string()) {
            errors.push(syn::Error::new(
                target_field.name.span(),
                format!(
                    "unknown field `{}` on target state `{}`",
                    target_field.name, target.name
                ),
            ));
        }
        if !bindings.contains_key(&target_field.binding.to_string()) {
            errors.push(syn::Error::new(
                target_field.binding.span(),
                format!(
                    "target field `{}` references unknown binding `{}`",
                    target_field.name, target_field.binding
                ),
            ));
        }
    }

    if seen.len() != fields.len() {
        errors.push(syn::Error::new(
            target.name.span(),
            format!(
                "target state `{}` requires every payload field",
                target.name
            ),
        ));
    }
}

fn note_callable(
    callable: &Callable,
    role: CallableRole,
    bindings: &HashMap<String, &FieldDef>,
    callables: &mut Vec<CallableDef>,
    errors: &mut Vec<syn::Error>,
) {
    let mut arguments = Vec::new();
    let mut seen_arguments = HashSet::new();
    for argument in &callable.arguments {
        if !seen_arguments.insert(argument.to_string()) {
            errors.push(syn::Error::new(
                argument.span(),
                format!("duplicate callback argument `{argument}`"),
            ));
            continue;
        }
        match bindings.get(&argument.to_string()) {
            Some(field) => arguments.push((argument.clone(), field.ty.clone())),
            None => errors.push(syn::Error::new(
                argument.span(),
                format!("callback argument `{argument}` is not bound by this row"),
            )),
        }
    }

    if let Some(existing) = callables
        .iter()
        .find(|existing| existing.name == callable.name)
    {
        let same_types = existing.arguments.len() == arguments.len()
            && existing
                .arguments
                .iter()
                .zip(&arguments)
                .all(|((_, left), (_, right))| type_key(left) == type_key(right));
        if existing.role != role || existing.is_async != callable.is_async || !same_types {
            errors.push(syn::Error::new(
                callable.name.span(),
                format!(
                    "callback `{}` is used with incompatible roles, async modes, or argument types",
                    callable.name
                ),
            ));
        }
        return;
    }

    callables.push(CallableDef {
        name: callable.name.clone(),
        role,
        is_async: callable.is_async,
        arguments,
    });
}

fn type_key(ty: &Type) -> String {
    ty.to_token_stream().to_string()
}

fn validate_coverage(
    definition: &MachineDef,
    flat_states: &[FlatState<'_>],
    errors: &mut Vec<syn::Error>,
) {
    let leaves: Vec<&FlatState<'_>> = flat_states
        .iter()
        .filter(|state| !state.node.is_compound())
        .collect();
    let parents: HashMap<String, Option<String>> = flat_states
        .iter()
        .map(|state| {
            (
                state.node.variant.name.to_string(),
                state.parent.map(ToString::to_string),
            )
        })
        .collect();
    let mut reachable = vec![false; definition.rows.len()];
    let mut missing = Vec::new();

    for leaf in &leaves {
        for event in &definition.events {
            let mut candidates = Vec::new();
            for (index, row) in definition.rows.iter().enumerate() {
                if !row_covers_event(row, &event.name) {
                    continue;
                }
                if let Some(distance) = row_source_distance(row, &leaf.node.variant.name, &parents)
                {
                    candidates.push((distance, index));
                }
            }
            candidates.sort_unstable();

            if candidates.is_empty() {
                missing.push(format!("({}, {})", leaf.node.variant.name, event.name));
                continue;
            }

            let mut blocked = false;
            for (_, index) in candidates {
                if !blocked {
                    reachable[index] = true;
                }
                if definition.rows[index].guard.is_none() {
                    blocked = true;
                }
            }
        }
    }

    if !missing.is_empty() {
        let shown = missing.len().min(8);
        let mut message = missing[..shown].join(", ");
        if missing.len() > shown {
            message.push_str(&format!(" and {} more", missing.len() - shown));
        }
        errors.push(syn::Error::new(
            definition.name.span(),
            format!(
                "unhandled leaf/event pairs in `{}`: {message}",
                definition.name
            ),
        ));
    }

    for (index, row) in definition.rows.iter().enumerate() {
        if !reachable[index] {
            errors.push(syn::Error::new(
                row.span,
                "unreachable transition row: an earlier unguarded row handles every matching leaf/event pair",
            ));
        }
    }
}

fn row_covers_event(row: &Row, event: &Ident) -> bool {
    match &row.event {
        RowEvent::Any => true,
        RowEvent::Event(pattern) => pattern.name == *event,
    }
}

fn row_source_distance(
    row: &Row,
    leaf: &Ident,
    parents: &HashMap<String, Option<String>>,
) -> Option<usize> {
    let RowSource::State(pattern) = &row.source else {
        return Some(usize::MAX);
    };

    let target = pattern.name.to_string();
    let mut current = Some(leaf.to_string());
    let mut distance = 0;
    while let Some(name) = current {
        if name == target {
            return Some(distance);
        }
        current = parents.get(&name).cloned().flatten();
        distance += 1;
    }
    None
}

fn combine(errors: Vec<syn::Error>) -> syn::Error {
    let mut errors = errors.into_iter();
    let mut combined = errors.next().expect("validation produced an error");
    for error in errors {
        combined.combine(error);
    }
    combined
}

#[cfg(test)]
mod tests {
    use syn::parse_str;

    use super::*;

    #[test]
    fn nested_tree_covers_descendant_events_from_parent_rows() {
        let definition: MachineDef = parse_str(
            r#"
                name: M,
                states: { *Root { *A, B } },
                events: { Go },
                transitions: {
                    Handled: Root + Go => Root,
                }
            "#,
        )
        .unwrap_or_else(|error| panic!("unexpected parse failure: {error}"));

        validate(&definition)
            .unwrap_or_else(|error| panic!("unexpected validation failure: {error}"));
    }

    #[test]
    fn missing_leaf_event_pair_is_reported() {
        let definition: MachineDef = parse_str(
            r#"
                name: M,
                states: { *A, B },
                events: { Go },
                transitions: {
                    Handled: A + Go => B,
                }
            "#,
        )
        .unwrap_or_else(|error| panic!("unexpected parse failure: {error}"));

        let error = validate(&definition)
            .err()
            .unwrap_or_else(|| panic!("expected failure"));
        assert!(error.to_string().contains("(B, Go)"));
    }

    #[test]
    fn every_compound_requires_one_initial_child() {
        let definition: MachineDef = parse_str(
            r#"
                name: M,
                states: { *Root { A, B } },
                events: { Go },
                transitions: { Handled: Root + Go => Root }
            "#,
        )
        .unwrap_or_else(|error| panic!("unexpected parse failure: {error}"));

        let error = validate(&definition)
            .err()
            .unwrap_or_else(|| panic!("expected failure"));
        assert!(
            error
                .to_string()
                .contains("compound state `Root` requires exactly one `*` initial state")
        );
    }

    #[test]
    fn target_payload_must_come_from_row_bindings() {
        let definition: MachineDef = parse_str(
            r#"
                name: M,
                states: { *A, B { value: u8 } },
                events: { Go },
                transitions: {
                    Moved: A + Go => B { value: missing },
                    Stayed: B { value } + Go => B { value },
                }
            "#,
        )
        .unwrap_or_else(|error| panic!("unexpected parse failure: {error}"));

        let error = validate(&definition)
            .err()
            .unwrap_or_else(|| panic!("expected failure"));
        assert!(error.to_string().contains("unknown binding `missing`"));
    }

    #[test]
    fn incompatible_callback_argument_types_are_rejected() {
        let definition: MachineDef = parse_str(
            r#"
                name: M,
                context: (),
                states: { *A },
                events: { Byte { value: u8 }, Word { value: u16 } },
                transitions: {
                    Byte: A + Byte { value } [check(value)] => A,
                    A + Byte { .. } => reject No,
                    Word: A + Word { value } [check(value)] => A,
                    A + Word { .. } => reject No,
                }
            "#,
        )
        .unwrap_or_else(|error| panic!("unexpected parse failure: {error}"));

        let error = validate(&definition)
            .err()
            .unwrap_or_else(|| panic!("expected failure"));
        assert!(
            error
                .to_string()
                .contains("incompatible roles, async modes, or argument types")
        );
    }

    #[test]
    fn row_shadowed_for_every_leaf_event_pair_is_rejected() {
        let definition: MachineDef = parse_str(
            r#"
                name: M,
                states: { *A },
                events: { Go },
                transitions: {
                    First: A + Go => A,
                    A + Go => reject No,
                }
            "#,
        )
        .unwrap_or_else(|error| panic!("unexpected parse failure: {error}"));

        let error = validate(&definition)
            .err()
            .unwrap_or_else(|| panic!("expected failure"));
        assert!(error.to_string().contains("unreachable transition row"));
    }

    #[test]
    fn async_callback_selects_async_generated_api() {
        let definition: MachineDef = parse_str(
            r#"
                name: M,
                context: (),
                states: { *A },
                events: { Go },
                transitions: {
                    Moved: A + Go [async allowed] => A,
                    A + Go => reject No,
                }
            "#,
        )
        .unwrap_or_else(|error| panic!("unexpected parse failure: {error}"));

        let validated = validate(&definition)
            .unwrap_or_else(|error| panic!("unexpected validation failure: {error}"));
        assert!(validated.is_async);
    }

    #[test]
    fn duplicate_state_and_event_names_are_reported() {
        let definition: MachineDef = parse_str(
            r#"
                name: M,
                states: { *A, A },
                events: { Go, Go },
                transitions: { Stayed: A + Go => A }
            "#,
        )
        .unwrap_or_else(|error| panic!("unexpected parse failure: {error}"));

        let error = validate(&definition)
            .err()
            .unwrap_or_else(|| panic!("expected failure"));
        let message = error.into_compile_error().to_string();
        assert!(message.contains("duplicate state `A`"));
        assert!(message.contains("duplicate event `Go`"));
    }

    #[test]
    fn duplicate_transition_labels_are_reported() {
        let definition: MachineDef = parse_str(
            r#"
                name: M,
                states: { *A, B },
                events: { Go },
                transitions: {
                    Same: A + Go => B,
                    Same: B + Go => A,
                }
            "#,
        )
        .unwrap_or_else(|error| panic!("unexpected parse failure: {error}"));

        let error = validate(&definition)
            .err()
            .unwrap_or_else(|| panic!("expected failure"));
        assert!(
            error
                .to_string()
                .contains("duplicate transition label `Same`")
        );
    }

    #[test]
    fn unknown_source_event_and_target_are_reported() {
        let definition: MachineDef = parse_str(
            r#"
                name: M,
                states: { *A },
                events: { Go },
                transitions: {
                    MissingSource: B + Go => A,
                    MissingEvent: A + Stop => A,
                    MissingTarget: A + Go => C,
                }
            "#,
        )
        .unwrap_or_else(|error| panic!("unexpected parse failure: {error}"));

        let error = validate(&definition)
            .err()
            .unwrap_or_else(|| panic!("expected failure"));
        let message = error.into_compile_error().to_string();
        assert!(message.contains("unknown source state `B`"));
        assert!(message.contains("unknown event `Stop`"));
        assert!(message.contains("unknown target state `C`"));
    }

    #[test]
    fn callback_requires_explicit_context() {
        let definition: MachineDef = parse_str(
            r#"
                name: M,
                states: { *A },
                events: { Go },
                transitions: {
                    Moved: A + Go [allowed] => A,
                    A + Go => reject Denied,
                }
            "#,
        )
        .unwrap_or_else(|error| panic!("unexpected parse failure: {error}"));

        let error = validate(&definition)
            .err()
            .unwrap_or_else(|| panic!("expected failure"));
        assert!(
            error
                .to_string()
                .contains("`context` is required when transition callbacks are used")
        );
    }

    #[test]
    fn effect_callback_requires_explicit_effect_type() {
        let definition: MachineDef = parse_str(
            r#"
                name: M,
                context: (),
                states: { *A },
                events: { Go },
                transitions: {
                    Moved: A + Go / emit => A,
                }
            "#,
        )
        .unwrap_or_else(|error| panic!("unexpected parse failure: {error}"));

        let error = validate(&definition)
            .err()
            .unwrap_or_else(|| panic!("expected failure"));
        assert!(
            error
                .to_string()
                .contains("`effect` is required because effect callback `emit` is used")
        );
    }

    #[test]
    fn raw_and_plain_spellings_share_one_rejection_variant() {
        let definition: MachineDef = parse_str(
            r#"
                name: M,
                states: { *A, B },
                events: { Go },
                transitions: {
                    A + Go => reject Denied,
                    B + Go => reject r#Denied,
                }
            "#,
        )
        .unwrap_or_else(|error| panic!("unexpected parse failure: {error}"));

        let validated = validate(&definition)
            .unwrap_or_else(|error| panic!("unexpected validation failure: {error}"));
        assert_eq!(validated.rejections.len(), 1);
        assert_eq!(validated.rejections[0].unraw(), "Denied");
    }
}
