use syn::parse::{Parse, ParseStream};
use syn::{Ident, LitBool, Token, Type, braced, bracketed, parenthesized};

use crate::model::{
    BindingPattern, Callable, FieldDef, MachineDef, Row, RowEvent, RowOutcome, RowSource,
    StateNode, TargetField, TargetState, VariantDef,
};

impl Parse for MachineDef {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut name = None;
        let mut serde = None;
        let mut context = None;
        let mut effect = None;
        let mut states = None;
        let mut events = None;
        let mut rows = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![:]>()?;

            match key.to_string().as_str() {
                "name" => set_once(&mut name, input.parse()?, &key)?,
                "serde" => set_once(&mut serde, input.parse::<LitBool>()?, &key)?,
                "context" => set_once(&mut context, input.parse::<Type>()?, &key)?,
                "effect" => set_once(&mut effect, input.parse::<Type>()?, &key)?,
                "rejection" => {
                    return Err(syn::Error::new(
                        key.span(),
                        "`rejection` is generated from `reject Reason` rows and must not be declared",
                    ));
                }
                "states" => {
                    let content;
                    braced!(content in input);
                    set_once(&mut states, parse_states(&content)?, &key)?;
                }
                "events" => {
                    let content;
                    braced!(content in input);
                    set_once(&mut events, parse_variants(&content)?, &key)?;
                }
                "transitions" => {
                    let content;
                    braced!(content in input);
                    set_once(&mut rows, parse_rows(&content)?, &key)?;
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown key `{other}`; expected name, serde, context, effect, states, events, or transitions"
                        ),
                    ));
                }
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(Self {
            name: name.ok_or_else(|| input.error("missing required key `name`"))?,
            serde: serde.is_some_and(|value| value.value),
            context,
            effect,
            states: states.ok_or_else(|| input.error("missing required key `states`"))?,
            events: events.ok_or_else(|| input.error("missing required key `events`"))?,
            rows: rows.ok_or_else(|| input.error("missing required key `transitions`"))?,
        })
    }
}

fn set_once<T>(slot: &mut Option<T>, value: T, key: &Ident) -> syn::Result<()> {
    if slot.replace(value).is_some() {
        return Err(syn::Error::new(
            key.span(),
            format!("duplicate key `{key}`"),
        ));
    }
    Ok(())
}

fn parse_states(input: ParseStream<'_>) -> syn::Result<Vec<StateNode>> {
    let mut states = Vec::new();

    while !input.is_empty() {
        let initial = input.peek(Token![*]);
        if initial {
            input.parse::<Token![*]>()?;
        }

        let name: Ident = input.parse()?;
        let mut fields = Vec::new();
        let mut children = Vec::new();

        if input.peek(syn::token::Brace) {
            let content;
            braced!(content in input);
            if content.is_empty() {
                return Err(syn::Error::new(
                    name.span(),
                    format!(
                        "state `{name}` uses empty braces; remove them for a leaf or declare fields or child states"
                    ),
                ));
            }
            if looks_like_fields(&content) {
                fields = parse_fields(&content)?;
            } else {
                children = parse_states(&content)?;
            }
        }

        states.push(StateNode {
            variant: VariantDef { name, fields },
            initial,
            children,
        });

        parse_list_separator(input, "state")?;
    }

    Ok(states)
}

fn looks_like_fields(input: ParseStream<'_>) -> bool {
    if input.is_empty() || input.peek(Token![*]) || !input.peek(Ident) {
        return false;
    }

    let fork = input.fork();
    let Ok(_) = fork.parse::<Ident>() else {
        return false;
    };
    fork.peek(Token![:])
}

fn parse_variants(input: ParseStream<'_>) -> syn::Result<Vec<VariantDef>> {
    let mut variants = Vec::new();

    while !input.is_empty() {
        let name: Ident = input.parse()?;
        let fields = if input.peek(syn::token::Brace) {
            let content;
            braced!(content in input);
            parse_fields(&content)?
        } else {
            Vec::new()
        };
        variants.push(VariantDef { name, fields });
        parse_list_separator(input, "variant")?;
    }

    Ok(variants)
}

fn parse_fields(input: ParseStream<'_>) -> syn::Result<Vec<FieldDef>> {
    let mut fields = Vec::new();

    while !input.is_empty() {
        let name: Ident = input.parse()?;
        input.parse::<Token![:]>()?;
        fields.push(FieldDef {
            name,
            ty: input.parse()?,
        });
        parse_list_separator(input, "field")?;
    }

    Ok(fields)
}

fn parse_rows(input: ParseStream<'_>) -> syn::Result<Vec<Row>> {
    let mut rows = Vec::new();

    while !input.is_empty() {
        let span = input.span();
        let label = if input.peek(Ident) && input.peek2(Token![:]) {
            let label = input.parse::<Ident>()?;
            input.parse::<Token![:]>()?;
            Some(label)
        } else {
            None
        };

        let source = if input.peek(Token![_]) {
            input.parse::<Token![_]>()?;
            RowSource::Any
        } else {
            RowSource::State(parse_binding_pattern(input)?)
        };

        input.parse::<Token![+]>()?;

        let event = if input.peek(Token![_]) {
            input.parse::<Token![_]>()?;
            RowEvent::Any
        } else {
            RowEvent::Event(parse_binding_pattern(input)?)
        };

        let guard = if input.peek(syn::token::Bracket) {
            let content;
            bracketed!(content in input);
            let callable = parse_callable(&content)?;
            if !content.is_empty() {
                return Err(content.error("unexpected tokens after guard"));
            }
            Some(callable)
        } else {
            None
        };

        let effect = if input.peek(Token![/]) {
            input.parse::<Token![/]>()?;
            Some(parse_callable(input)?)
        } else {
            None
        };

        input.parse::<Token![=>]>()?;

        let outcome = if input.peek(Ident) {
            let fork = input.fork();
            let word: Ident = fork.parse()?;
            if word == "reject" {
                input.parse::<Ident>()?;
                let rejection: Ident = input.parse()?;
                if let Some(label) = label {
                    return Err(syn::Error::new(
                        label.span(),
                        "rejection rows do not have transition labels",
                    ));
                }
                if effect.is_some() {
                    return Err(input.error("rejection rows cannot emit an effect"));
                }
                RowOutcome::Reject(rejection)
            } else {
                RowOutcome::Transition {
                    transition: label.ok_or_else(|| {
                        input.error("accepted transition rows require a `Name:` label")
                    })?,
                    target: Some(parse_target(input)?),
                }
            }
        } else if input.peek(Token![_]) {
            input.parse::<Token![_]>()?;
            RowOutcome::Transition {
                transition: label
                    .ok_or_else(|| input.error("accepted stay rows require a `Name:` label"))?,
                target: None,
            }
        } else {
            return Err(input.error("expected a target state, `_`, or `reject Reason`"));
        };

        rows.push(Row {
            source,
            event,
            guard,
            effect,
            outcome,
            span,
        });
        parse_list_separator(input, "transition")?;
    }

    Ok(rows)
}

fn parse_binding_pattern(input: ParseStream<'_>) -> syn::Result<BindingPattern> {
    let name: Ident = input.parse()?;
    let mut fields = Vec::new();
    let mut rest = false;
    let explicit_fields = input.peek(syn::token::Brace);

    if explicit_fields {
        let content;
        braced!(content in input);
        while !content.is_empty() {
            if content.peek(Token![..]) {
                content.parse::<Token![..]>()?;
                rest = true;
                if content.peek(Token![,]) {
                    content.parse::<Token![,]>()?;
                }
                if !content.is_empty() {
                    return Err(content.error("`..` must be the final pattern item"));
                }
                break;
            }
            fields.push(content.parse()?);
            parse_list_separator(&content, "pattern field")?;
        }
    }

    Ok(BindingPattern {
        name,
        fields,
        rest,
        explicit_fields,
    })
}

fn parse_callable(input: ParseStream<'_>) -> syn::Result<Callable> {
    let is_async = input.peek(Token![async]);
    if is_async {
        input.parse::<Token![async]>()?;
    }
    let name: Ident = input.parse()?;
    let mut arguments = Vec::new();

    if input.peek(syn::token::Paren) {
        let content;
        parenthesized!(content in input);
        while !content.is_empty() {
            arguments.push(content.parse()?);
            parse_list_separator(&content, "callback argument")?;
        }
    }

    Ok(Callable {
        is_async,
        name,
        arguments,
    })
}

fn parse_target(input: ParseStream<'_>) -> syn::Result<TargetState> {
    let name: Ident = input.parse()?;
    let explicit_fields = input.peek(syn::token::Brace);
    let mut fields = Vec::new();

    if explicit_fields {
        let content;
        braced!(content in input);
        while !content.is_empty() {
            let field: Ident = content.parse()?;
            let binding = if content.peek(Token![:]) {
                content.parse::<Token![:]>()?;
                content.parse()?
            } else {
                field.clone()
            };
            fields.push(TargetField {
                name: field,
                binding,
            });
            parse_list_separator(&content, "target field")?;
        }
    }

    Ok(TargetState {
        name,
        fields,
        explicit_fields,
    })
}

fn parse_list_separator(input: ParseStream<'_>, item: &str) -> syn::Result<()> {
    if input.peek(Token![,]) {
        input.parse::<Token![,]>()?;
        Ok(())
    } else if input.is_empty() {
        Ok(())
    } else {
        Err(input.error(format!("expected `,` after {item}")))
    }
}

#[cfg(test)]
mod tests {
    use syn::parse_str;

    use super::*;

    #[test]
    fn empty_state_braces_are_rejected() {
        let result = parse_str::<MachineDef>(
            r#"
                name: M,
                states: { *Payment {} },
                events: { Go },
                transitions: { Stayed: Payment + Go => Payment }
            "#,
        );

        let error = result.err().unwrap_or_else(|| panic!("expected failure"));
        assert!(
            error
                .to_string()
                .contains("state `Payment` uses empty braces")
        );
    }

    #[test]
    fn explicit_rejection_type_has_a_migration_error() {
        let result = parse_str::<MachineDef>(
            r#"
                name: M,
                rejection: Refusal,
                states: { *A },
                events: { Go },
                transitions: { A + Go => reject Denied }
            "#,
        );

        let error = result.err().unwrap_or_else(|| panic!("expected failure"));
        assert!(error.to_string().contains(
            "`rejection` is generated from `reject Reason` rows and must not be declared"
        ));
    }
}
