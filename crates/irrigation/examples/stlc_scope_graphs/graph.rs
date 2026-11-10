//! The scope graph: one node kind for scopes, terms, and types.
//!
//! A scope graph is `(S, L, →, ↦)`: scopes, labelled edges, and declarations.
//! This subset has lexical `P` (parent), `D` (declaration), `R` (reference),
//! `child`, and `T` (type) edges. A binder is a **term node with a binding
//! payload**; there is no second binding node for the same term. A type scope is
//! created once per distinct rendered type, so equal types share one node.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use irrigation::GraphRef;

use crate::terms::Term;

const TERM_TAG: u64 = 1 << 62;
const TYPE_TAG: u64 = 3 << 62;
const MASK: u64 = (1 << 62) - 1;
const EDGE_TAG_SHIFT: u64 = 61;
const EDGE_HASH_MASK: u64 = (1 << EDGE_TAG_SHIFT) - 1;
const TYPE_EDGE_TAG: u64 = 4;

/// What introduced a binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BindingKind {
    /// A lambda parameter.
    Lambda,
    /// A `let` binder.
    Let,
}

/// The payload of a term node that declares something.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    /// The lambda/let term that introduced it.
    pub binder: u64,
    /// The let's bound term; `None` for a lambda.
    pub value: Option<u64>,
    /// What introduced the binding.
    pub kind: BindingKind,
}

/// The node identity of a lexical scope.
#[must_use]
pub fn scope_node(index: u64) -> u64 {
    index & MASK
}

/// The node identity of a term.
#[must_use]
pub fn term_node(term: u64) -> u64 {
    TERM_TAG | (term & MASK)
}

/// The node identity of a canonical type scope.
#[must_use]
pub fn type_node(label: &str) -> u64 {
    TYPE_TAG | (hash_bytes(0xcbf2_9ce4_8422_2325, label.as_bytes()) & MASK)
}

/// One graph node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeNode {
    /// A lexical scope.
    Scope {
        /// A short label.
        label: String,
    },
    /// One semantic term. Binders carry their declaration payload here.
    Term {
        /// The original term identity (before the node tag).
        term: u64,
        /// A short label.
        label: String,
        /// The declaration payload, for lambda/let terms.
        binding: Option<Binding>,
    },
    /// A canonical type scope.
    Type {
        /// The rendered type.
        label: String,
    },
}

impl fmt::Display for ScopeNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scope { label } | Self::Term { label, .. } | Self::Type { label } => {
                f.write_str(label)
            }
        }
    }
}

/// One graph edge payload. Its `Debug` form is the label the graph draws.
#[derive(Clone, PartialEq, Eq)]
pub enum ScopeEdge {
    /// The lexical parent edge.
    Parent,
    /// A declaration edge: a scope declares a term/binding.
    Declares {
        /// The declaring term node.
        binding: u64,
        /// The declared name.
        name: String,
    },
    /// A reference edge: a scope contains a variable term.
    Occurs {
        /// The variable term.
        node: u64,
        /// The referenced name.
        name: String,
    },
    /// A term-to-subterm edge.
    Child,
    /// A term-to-type-scope edge.
    TypeOf {
        /// The typed term.
        term: u64,
    },
}

impl ScopeEdge {
    fn tag(&self) -> u64 {
        match self {
            Self::Parent => 0,
            Self::Declares { .. } => 1,
            Self::Occurs { .. } => 2,
            Self::Child => 3,
            Self::TypeOf { .. } => TYPE_EDGE_TAG,
        }
    }
}

impl fmt::Debug for ScopeEdge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parent => f.write_str("P"),
            Self::Declares { name, .. } => write!(f, "D {name}"),
            Self::Occurs { name, .. } => write!(f, "R {name}"),
            Self::Child => f.write_str("child"),
            Self::TypeOf { .. } => f.write_str("T"),
        }
    }
}

/// One edge with its endpoints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    /// Source node.
    pub from: u64,
    /// Destination node.
    pub to: u64,
    /// The edge payload.
    pub edge: ScopeEdge,
}

/// A graph fragment the resolver is about to publish.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    /// Nodes by identity.
    pub nodes: BTreeMap<u64, ScopeNode>,
    /// Edges by identity.
    pub edges: BTreeMap<u64, Edge>,
}

/// Builds the scope/term graph from the term table.
#[must_use]
pub fn build<F, N>(root: Option<u64>, node: F, name: N) -> Draft
where
    F: Fn(u64) -> Option<Term>,
    N: Fn(u64) -> Option<String>,
{
    let mut draft = Draft::default();
    let Some(root) = root else {
        return draft;
    };
    add_node(
        &mut draft,
        scope_node(0),
        ScopeNode::Scope {
            label: String::from("module"),
        },
    );
    let mut next = 1u64;
    walk(&mut draft, &mut next, root, None, 0, &node, &name);
    draft
}

fn walk<F, N>(
    draft: &mut Draft,
    next: &mut u64,
    term: u64,
    parent: Option<u64>,
    scope: u64,
    node: &F,
    name: &N,
) where
    F: Fn(u64) -> Option<Term>,
    N: Fn(u64) -> Option<String>,
{
    let Some(shape) = node(term) else {
        return;
    };
    let spelling = name(term).unwrap_or_default();
    let binding = match shape {
        Term::Lam { .. } => Some(Binding {
            binder: term,
            value: None,
            kind: BindingKind::Lambda,
        }),
        Term::Let { binding, .. } => Some(Binding {
            binder: term,
            value: Some(binding),
            kind: BindingKind::Let,
        }),
        _ => None,
    };
    let label = match shape {
        Term::Var => spelling.clone(),
        Term::Lam { .. } => format!("λ{spelling}"),
        Term::Let { .. } => format!("let {spelling}"),
        Term::App { .. } => String::from("app"),
        Term::If { .. } => String::from("if"),
        Term::Int => String::from("int"),
        Term::Bool => String::from("bool"),
    };
    let this = term_node(term);
    add_node(
        draft,
        this,
        ScopeNode::Term {
            term,
            label,
            binding,
        },
    );
    if let Some(parent) = parent {
        add_edge(draft, term_node(parent), this, ScopeEdge::Child);
    }
    match shape {
        Term::Var => {
            add_edge(
                draft,
                scope_node(scope),
                this,
                ScopeEdge::Occurs {
                    node: term,
                    name: spelling,
                },
            );
        }
        Term::Lam { body } => {
            let inner = *next;
            *next = next.saturating_add(1);
            add_node(
                draft,
                scope_node(inner),
                ScopeNode::Scope {
                    label: format!("λ{spelling}"),
                },
            );
            add_edge(
                draft,
                scope_node(inner),
                scope_node(scope),
                ScopeEdge::Parent,
            );
            add_edge(
                draft,
                scope_node(inner),
                this,
                ScopeEdge::Declares {
                    binding: this,
                    name: spelling,
                },
            );
            walk(draft, next, body, Some(term), inner, node, name);
        }
        Term::Let { binding, body } => {
            walk(draft, next, binding, Some(term), scope, node, name);
            let inner = *next;
            *next = next.saturating_add(1);
            add_node(
                draft,
                scope_node(inner),
                ScopeNode::Scope {
                    label: format!("let {spelling}"),
                },
            );
            add_edge(
                draft,
                scope_node(inner),
                scope_node(scope),
                ScopeEdge::Parent,
            );
            add_edge(
                draft,
                scope_node(inner),
                this,
                ScopeEdge::Declares {
                    binding: this,
                    name: spelling,
                },
            );
            walk(draft, next, body, Some(term), inner, node, name);
        }
        Term::App { func, arg } => {
            walk(draft, next, func, Some(term), scope, node, name);
            walk(draft, next, arg, Some(term), scope, node, name);
        }
        Term::If { cond, yes, no } => {
            walk(draft, next, cond, Some(term), scope, node, name);
            walk(draft, next, yes, Some(term), scope, node, name);
            walk(draft, next, no, Some(term), scope, node, name);
        }
        Term::Int | Term::Bool => {}
    }
}

/// The targets and name diagnostics of one resolution pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NameAnalysis {
    /// Reference term -> declaring term.
    pub targets: BTreeMap<u64, u64>,
    /// Term -> diagnostic.
    pub errors: BTreeMap<u64, String>,
}

/// Resolves every reference and checks binder shadowing against the draft.
#[must_use]
pub fn resolve(draft: &Draft) -> NameAnalysis {
    let mut analysis = NameAnalysis::default();
    for (id, node) in &draft.nodes {
        let ScopeNode::Term {
            term,
            binding,
            label,
        } = node
        else {
            continue;
        };
        if let Some(binding) = binding {
            if let Some((scope, name)) = declaration_of(draft, *id) {
                check_shadow(draft, &mut analysis, scope, &name, binding.binder);
            }
            continue;
        }
        if label.is_empty() {
            continue;
        }
        let Some((scope, name)) = reference_of(draft, *term) else {
            continue;
        };
        match resolve_from(draft, scope, &name) {
            Resolution::Resolved(target) => {
                analysis.targets.insert(*term, target);
            }
            Resolution::Unresolved => {
                analysis.errors.insert(
                    *term,
                    format!("unresolved `{name}` in {}", scope_label(draft, scope)),
                );
            }
            Resolution::Ambiguous(count) => {
                analysis.errors.insert(
                    *term,
                    format!(
                        "ambiguous `{name}` in {}: {count} declarations at the same distance",
                        scope_label(draft, scope)
                    ),
                );
            }
        }
    }
    analysis
}

fn check_shadow(draft: &Draft, analysis: &mut NameAnalysis, scope: u64, name: &str, binder: u64) {
    let Some(parent) = parent_of(draft, scope) else {
        return;
    };
    let mut cursor = Some(parent);
    let mut distance = 1u16;
    while let Some(outer) = cursor {
        if !declarations_in(draft, outer, name).is_empty() {
            analysis.errors.insert(
                binder,
                format!(
                    "`{name}` in {} shadows an outer binding {distance} P-step(s) away",
                    scope_label(draft, scope)
                ),
            );
            return;
        }
        cursor = parent_of(draft, outer);
        distance = distance.saturating_add(1);
    }
}

enum Resolution {
    Resolved(u64),
    Unresolved,
    Ambiguous(usize),
}

fn resolve_from(draft: &Draft, scope: u64, name: &str) -> Resolution {
    for (_, reachable) in reachable(draft, scope) {
        let declarations = declarations_in(draft, reachable, name);
        if declarations.is_empty() {
            continue;
        }
        return match declarations.as_slice() {
            [binding] => Resolution::Resolved(*binding),
            many => Resolution::Ambiguous(many.len()),
        };
    }
    Resolution::Unresolved
}

fn reachable(draft: &Draft, scope: u64) -> Vec<(u16, u64)> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([(0u16, scope)]);
    let mut out = Vec::new();
    while let Some((distance, current)) = queue.pop_front() {
        if !seen.insert(current) {
            continue;
        }
        out.push((distance, current));
        if let Some(parent) = parent_of(draft, current) {
            queue.push_back((distance.saturating_add(1), parent));
        }
    }
    out
}

fn parent_of(draft: &Draft, scope: u64) -> Option<u64> {
    draft.edges.values().find_map(|edge| {
        (edge.from == scope && matches!(edge.edge, ScopeEdge::Parent)).then_some(edge.to)
    })
}

fn declarations_in(draft: &Draft, scope: u64, name: &str) -> Vec<u64> {
    draft
        .edges
        .values()
        .filter_map(|edge| {
            if edge.from != scope {
                return None;
            }
            match &edge.edge {
                ScopeEdge::Declares {
                    binding,
                    name: declared,
                } if declared == name => Some(raw_term(draft, *binding)),
                _ => None,
            }
        })
        .collect()
}

fn raw_term(draft: &Draft, node: u64) -> u64 {
    draft
        .nodes
        .get(&node)
        .and_then(|node| match node {
            ScopeNode::Term { term, .. } => Some(*term),
            _ => None,
        })
        .unwrap_or(node & MASK)
}

fn reference_of(draft: &Draft, term: u64) -> Option<(u64, String)> {
    draft.edges.values().find_map(|edge| match &edge.edge {
        ScopeEdge::Occurs { node, name } if *node == term => Some((edge.from, name.clone())),
        _ => None,
    })
}

fn declaration_of(draft: &Draft, term: u64) -> Option<(u64, String)> {
    draft.edges.values().find_map(|edge| match &edge.edge {
        ScopeEdge::Declares { binding, name } if *binding == term => {
            Some((edge.from, name.clone()))
        }
        _ => None,
    })
}

fn scope_label(draft: &Draft, scope: u64) -> String {
    draft.nodes.get(&scope).map_or_else(
        || format!("scope #{scope:x}"),
        |node| match node {
            ScopeNode::Scope { label } => label.clone(),
            _ => format!("scope #{scope:x}"),
        },
    )
}

/// Publishes one draft into the graph View.
pub fn publish(view: GraphRef<ScopeNode, ScopeEdge>, draft: &Draft, label: &'static str) {
    for (id, node) in &draft.nodes {
        view.define_node(*id, node.clone());
    }
    for (id, edge) in &draft.edges {
        view.define_edge_trusted(*id, edge.from, edge.to, edge.edge.clone(), label);
    }
}

/// Adds one canonical type scope.
pub fn publish_type_scope(graph: GraphRef<ScopeNode, ScopeEdge>, ty: &str) {
    graph.define_node(
        type_node(ty),
        ScopeNode::Type {
            label: ty.to_owned(),
        },
    );
}

/// Adds one term-to-type `T` edge.
pub fn publish_type_edge(graph: GraphRef<ScopeNode, ScopeEdge>, term: u64, ty: &str) {
    let node = type_node(ty);
    let from = term_node(term);
    let edge = edge_id(from, node, &ScopeEdge::TypeOf { term });
    graph.define_edge_trusted(edge, from, node, ScopeEdge::TypeOf { term }, "type_edge");
}

fn add_node(draft: &mut Draft, id: u64, node: ScopeNode) {
    draft.nodes.entry(id).or_insert(node);
}

fn add_edge(draft: &mut Draft, from: u64, to: u64, edge: ScopeEdge) -> u64 {
    let id = edge_id(from, to, &edge);
    draft.edges.entry(id).or_insert(Edge { from, to, edge });
    id
}

fn edge_id(from: u64, to: u64, edge: &ScopeEdge) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    hash = hash_bytes_from(hash, &from.to_le_bytes());
    hash = hash_bytes_from(hash, &to.to_le_bytes());
    let hash = hash_bytes_from(hash, format!("{edge:?}").as_bytes());
    (edge.tag() << EDGE_TAG_SHIFT) | (hash & EDGE_HASH_MASK)
}

fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn hash_bytes_from(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::{
        BindingKind, Draft, ScopeEdge, ScopeNode, add_edge, add_node, scope_node, term_node,
    };

    #[test]
    fn identical_edges_collapse() {
        let mut draft = Draft::default();
        add_node(
            &mut draft,
            scope_node(0),
            ScopeNode::Scope {
                label: String::from("module"),
            },
        );
        add_edge(&mut draft, 1, 0, ScopeEdge::Parent);
        add_edge(&mut draft, 1, 0, ScopeEdge::Parent);
        assert_eq!(draft.edges.len(), 1);
    }

    #[test]
    fn a_declaration_targets_a_term_node() {
        let mut draft = Draft::default();
        let binding = term_node(1);
        add_edge(
            &mut draft,
            0,
            binding,
            ScopeEdge::Declares {
                binding,
                name: String::from("x"),
            },
        );
        assert_eq!(draft.edges.len(), 1);
        let kind = BindingKind::Let;
        assert_eq!(kind, BindingKind::Let);
    }
}
