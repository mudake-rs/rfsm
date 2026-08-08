use proc_macro2::Span;
use syn::{Ident, Type};

#[derive(Clone)]
pub struct FieldDef {
    pub name: Ident,
    pub ty: Type,
}

#[derive(Clone)]
pub struct VariantDef {
    pub name: Ident,
    pub fields: Vec<FieldDef>,
}

#[derive(Clone)]
pub struct StateNode {
    pub variant: VariantDef,
    pub initial: bool,
    pub children: Vec<StateNode>,
}

impl StateNode {
    pub fn is_compound(&self) -> bool {
        !self.children.is_empty()
    }
}

#[derive(Clone)]
pub struct BindingPattern {
    pub name: Ident,
    pub fields: Vec<Ident>,
    pub rest: bool,
    pub explicit_fields: bool,
}

#[derive(Clone)]
pub enum RowSource {
    Any,
    State(BindingPattern),
}

#[derive(Clone)]
pub enum RowEvent {
    Any,
    Event(BindingPattern),
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum CallableRole {
    Guard,
    Effect,
}

#[derive(Clone)]
pub struct Callable {
    pub is_async: bool,
    pub name: Ident,
    pub arguments: Vec<Ident>,
}

#[derive(Clone)]
pub struct TargetField {
    pub name: Ident,
    pub binding: Ident,
}

#[derive(Clone)]
pub struct TargetState {
    pub name: Ident,
    pub fields: Vec<TargetField>,
    pub explicit_fields: bool,
}

#[derive(Clone)]
pub enum RowOutcome {
    Transition {
        transition: Ident,
        target: Option<TargetState>,
    },
    Reject(Ident),
}

#[derive(Clone)]
pub struct Row {
    pub source: RowSource,
    pub event: RowEvent,
    pub guard: Option<Callable>,
    pub effect: Option<Callable>,
    pub outcome: RowOutcome,
    pub span: Span,
}

pub struct MachineDef {
    pub name: Ident,
    pub context: Option<Type>,
    pub effect: Option<Type>,
    pub states: Vec<StateNode>,
    pub events: Vec<VariantDef>,
    pub rows: Vec<Row>,
}

#[derive(Clone)]
pub struct FlatState<'a> {
    pub node: &'a StateNode,
    pub parent: Option<&'a Ident>,
}

pub fn flatten_states(nodes: &[StateNode]) -> Vec<FlatState<'_>> {
    fn visit<'a>(
        nodes: &'a [StateNode],
        parent: Option<&'a Ident>,
        output: &mut Vec<FlatState<'a>>,
    ) {
        for node in nodes {
            output.push(FlatState { node, parent });
            visit(&node.children, Some(&node.variant.name), output);
        }
    }

    let mut output = Vec::new();
    visit(nodes, None, &mut output);
    output
}
