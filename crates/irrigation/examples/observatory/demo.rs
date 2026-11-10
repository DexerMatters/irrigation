//! The demo program the Observatory observes.
//!
//! A tiny assignment language: a source document, the framework's text, lexer,
//! and parser components, and two ordinary components over the published AST.
//! The lexer and parser are on the board like every other stage — the board has
//! no idea what they are — and only the source document is editable.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use irrigation::view;
use irrigation_syntax::{AstMeta, AstNodeRef, AstTree, Tokens};

/// The symbol table, as a whole-value list.
#[view(derived)]
pub struct Symbols(List<Symbol>);

/// One bound name.
#[derive(Clone, PartialEq, Eq)]
pub struct Symbol {
    /// The bound name.
    pub name: String,
}

impl std::fmt::Debug for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

/// Unresolved references, as a whole-value list.
#[view(derived)]
pub struct Diagnostics(List<String>);

/// A second source, of a different kind: numbers the host owns.
#[view(source)]
pub struct Weights(List<u64>);

/// The sum reduction, so the weights have a downstream View.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sum;

impl irrigation::Reduction for Sum {
    type Item = u64;
    type Aggregate = u64;

    fn leaf(item: &Self::Item) -> Self::Aggregate {
        *item
    }

    fn combine(left: &Self::Aggregate, right: &Self::Aggregate) -> Self::Aggregate {
        left + right
    }

    fn identity() -> Self::Aggregate {
        0
    }
}

/// Weighted total, as a reduced View.
#[view(derived)]
pub struct Total(Reduced<Sum>);

/// Reads the published Views and walks the parser's tree.
///
/// Everything here is a tracked read: the helper functions are called inside a
/// component, so the units they touch are exactly the ones the component owns.
struct Source<'a> {
    rules: &'a HashMap<u64, Arc<str>>,
    payloads: &'a HashMap<u64, Arc<str>>,
}

impl<'a> Source<'a> {
    fn rule(&self, node: &AstNodeRef) -> Option<&str> {
        node.rule()
            .and_then(|rule| self.rules.get(&rule))
            .map(std::convert::AsRef::as_ref)
    }

    fn text(&self, node: &AstNodeRef) -> Option<&str> {
        node.token_id()
            .and_then(|token| self.payloads.get(&token))
            .map(std::convert::AsRef::as_ref)
    }

    /// A wrapper rule with one meaningful child is transparent. The parser
    /// keeps `Statement` and `Expression` so the grammar reads well; a walker
    /// should see what they wrap.
    fn unwrap(&self, node: &AstNodeRef) -> AstNodeRef {
        let mut current = node.clone();
        for _ in 0..8 {
            if !matches!(self.rule(&current), Some("Statement" | "Expression")) {
                return current;
            }
            let Some(child) = current
                .children()
                .into_iter()
                .find(|child| child.token_id().is_none())
            else {
                return current;
            };
            current = child;
        }
        current
    }

    fn child_tokens(node: &AstNodeRef) -> Vec<AstNodeRef> {
        node.children()
            .into_iter()
            .filter(|child| child.token_id().is_some())
            .collect()
    }

    fn statements(&self, root: &AstNodeRef) -> Vec<AstNodeRef> {
        root.children()
            .into_iter()
            .filter(|child| child.token_id().is_none())
            .map(|child| self.unwrap(&child))
            .collect()
    }

    /// The expression children of a node, with the `Values` wrapper and the
    /// punctuation tokens removed.
    fn expressions(&self, node: &AstNodeRef) -> Vec<AstNodeRef> {
        let mut out = Vec::new();
        for child in node.children() {
            if child.token_id().is_some() {
                continue;
            }
            let child = self.unwrap(&child);
            if matches!(
                self.rule(&child),
                Some("Values" | "Print" | "Assign" | "Program")
            ) {
                out.extend(self.expressions(&child));
                continue;
            }
            out.push(child);
        }
        out
    }

    fn assign_name(&self, node: &AstNodeRef) -> Option<String> {
        Self::child_tokens(node)
            .into_iter()
            .find(|token| self.text(token) != Some(":="))
            .and_then(|token| self.text(&token).map(std::string::ToString::to_string))
    }

    fn name(&self, node: &AstNodeRef) -> Option<String> {
        Self::child_tokens(node)
            .into_iter()
            .find_map(|token| self.text(&token).map(std::string::ToString::to_string))
    }
}

fn rules() -> HashMap<u64, Arc<str>> {
    AstMeta.rules().to_vec().into_iter().collect()
}

fn payloads() -> HashMap<u64, Arc<str>> {
    Tokens
        .payloads()
        .to_vec()
        .into_iter()
        .map(|(id, payload)| -> (u64, Arc<str>) { (id.0, Arc::clone(&payload.text)) })
        .collect()
}

/// Every bound name in source order.
fn bound_names() -> Vec<String> {
    let Some(root) = AstTree.root() else {
        return Vec::new();
    };
    let rules = rules();
    let payloads = payloads();
    let source = Source {
        rules: &rules,
        payloads: &payloads,
    };
    source
        .statements(&root)
        .iter()
        .filter(|statement| source.rule(statement) == Some("Assign"))
        .filter_map(|statement| source.assign_name(statement))
        .collect()
}

/// Every unresolved reference, in source order.
fn unresolved_names() -> Vec<String> {
    let Some(root) = AstTree.root() else {
        return Vec::new();
    };
    let rules = rules();
    let payloads = payloads();
    let source = Source {
        rules: &rules,
        payloads: &payloads,
    };
    let defined: HashSet<String> = source
        .statements(&root)
        .iter()
        .filter(|statement| source.rule(statement) == Some("Assign"))
        .filter_map(|statement| source.assign_name(statement))
        .collect();
    let mut messages = Vec::new();
    for statement in source.statements(&root) {
        match source.rule(&statement) {
            Some("Assign") => {
                if let Some(value) = source.expressions(&statement).first() {
                    unresolved_in(&source, value, &defined, &mut messages);
                }
            }
            Some("Print") => {
                for value in source.expressions(&statement) {
                    unresolved_in(&source, &value, &defined, &mut messages);
                }
            }
            _ => {}
        }
    }
    messages
}

fn unresolved_in(
    source: &Source<'_>,
    node: &AstNodeRef,
    defined: &HashSet<String>,
    out: &mut Vec<String>,
) {
    let node = source.unwrap(node);
    match source.rule(&node) {
        Some("Name") => {
            let Some(name) = source.name(&node) else {
                return;
            };
            if !defined.contains(&name) {
                out.push(format!("unresolved name `{name}`"));
            }
        }
        Some("Call") => {
            for argument in source.expressions(&node) {
                unresolved_in(source, &argument, defined, out);
            }
        }
        Some("Number") => {}
        // A wrapper this walker does not know: descend into it rather than
        // pretend there was nothing inside.
        _ => {
            for child in source.expressions(&node) {
                unresolved_in(source, &child, defined, out);
            }
        }
    }
}

#[irrigation::irrigation]
pub mod resolve {
    /// Symbols: one component, one View. An owned parameter makes this an
    /// invoked component, so the runtime keeps its identity on the board.
    #[component]
    pub fn symbols(version: u8) -> usize {
        let _ = version;
        let names = super::bound_names();
        super::Symbols::from(
            names
                .iter()
                .map(|name| super::Symbol { name: name.clone() })
                .collect::<Vec<_>>(),
        );
        names.len()
    }

    /// Diagnostics: a second component reading the same source.
    #[component]
    pub fn diagnostics(strict: bool) -> usize {
        let messages: Vec<String> = super::unresolved_names()
            .into_iter()
            .filter(|message| strict || message.len() > 4)
            .collect();
        let count = messages.len();
        super::Diagnostics::from(messages);
        count
    }

    /// The weights total: a third component, reading the other source.
    #[component]
    pub fn total(scale: u64) -> u64 {
        let values = super::Weights.field0().to_vec();
        super::Total::from(values.clone());
        values.iter().sum::<u64>() * scale
    }

    /// The root composes them; each call carries a constant, which is enough
    /// for the runtime to keep the component boundaries. Every stage reads the
    /// Views it needs, so the root's demand reaches the whole pipeline.
    #[component]
    pub fn resolve_program(source: &irrigation_syntax::SourceFile) -> u64 {
        let _ = source;
        let a = symbols(1);
        let b = diagnostics(true);
        let c = total(1);
        (a as u64) + (b as u64) + c
    }
}
