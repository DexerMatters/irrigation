//! Lowers the parser's raw AST into the semantic term table.
//!
//! The parser is generic: it publishes rule nodes and token leaves. This stage
//! assigns every semantic term a stable identity (the AST node it came from) and
//! splits its observations into separate maps, so a downstream stage can read
//! exactly what it needs:
//!
//! * [`TermTable::nodes`] — shape and child identities;
//! * [`TermTable::names`] — binder and variable spellings;
//! * [`TermTable::annotations`] — lambda parameter types;
//! * [`TermTable::literals`] — integer and boolean payloads.
//!
//! A type edit therefore changes `annotations` alone, and a literal edit
//! changes `literals` alone.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use irrigation_syntax::{AstMeta, AstNodeRef, AstTree, Tokens};

/// The root term's stable identity.
const ROOT: u64 = 0x5354_4c43_0000_0001;

/// A stable identity for a child: the parent identity and the child's slot.
///
/// The parser's node identities follow token extents, so a token whose text
/// changes can take a new identity. The term table wants the *position* to keep
/// its identity, exactly as the JSON sink keys a path: then an edit to a literal
/// rewrites one unit of one term instead of replacing the term.
fn child_id(parent: u64, slot: u8) -> u64 {
    let mut hash = parent ^ 0xcbf2_9ce4_8422_2325;
    hash ^= u64::from(slot);
    hash = hash.wrapping_mul(0x100_0000_01b3);
    hash ^= hash >> 33;
    hash
}

/// The shape of one term. Child fields are term identities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Term {
    /// A variable reference.
    Var,
    /// An abstraction; the body is in the new scope.
    Lam {
        /// The body term.
        body: u64,
    },
    /// An application.
    App {
        /// The applied term.
        func: u64,
        /// The argument.
        arg: u64,
    },
    /// A `let`; the binding is evaluated in the enclosing scope.
    Let {
        /// The bound term.
        binding: u64,
        /// The body, in the new scope.
        body: u64,
    },
    /// An integer literal.
    Int,
    /// A boolean literal.
    Bool,
    /// A conditional.
    If {
        /// The condition.
        cond: u64,
        /// The value when true.
        yes: u64,
        /// The value otherwise.
        no: u64,
    },
}

/// A literal payload, kept apart from the shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Literal {
    /// An integer.
    Int(i64),
    /// A boolean.
    Bool(bool),
}

/// The semantic terms of one program.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TermTable {
    /// Terms by identity.
    pub nodes: BTreeMap<u64, Term>,
    /// Variable and binder names by term identity.
    pub names: BTreeMap<u64, String>,
    /// Lambda annotations by term identity.
    pub annotations: BTreeMap<u64, String>,
    /// Literal payloads by term identity.
    pub literals: BTreeMap<u64, Literal>,
    /// The program's term identity.
    pub root: Option<u64>,
    /// Nodes that could not be lowered.
    pub diagnostics: Vec<String>,
}

/// Reads the three published Views and lowers the tree.
///
/// Runs inside the elaborating component; every read below is a tracked unit.
#[must_use]
pub fn lower_now() -> TermTable {
    let root = AstTree.root();
    let rules: HashMap<u64, Arc<str>> = AstMeta.rules().to_vec().into_iter().collect();
    let payloads: HashMap<u64, Arc<str>> = Tokens
        .payloads()
        .to_vec()
        .into_iter()
        .map(|(id, payload)| (id.0, Arc::clone(&payload.text)))
        .collect();
    lower(root.as_ref(), &rules, &payloads)
}

/// Lowers a tree whose root, rule names, and token texts are already read.
#[must_use]
pub fn lower(
    root: Option<&AstNodeRef>,
    rules: &HashMap<u64, Arc<str>>,
    payloads: &HashMap<u64, Arc<str>>,
) -> TermTable {
    let mut lowerer = Lowerer {
        rules,
        payloads,
        out: TermTable::default(),
    };
    if let Some(root) = root {
        lowerer.out.root = lowerer.term(root, ROOT);
    }
    lowerer.out
}

struct Lowerer<'a> {
    rules: &'a HashMap<u64, Arc<str>>,
    payloads: &'a HashMap<u64, Arc<str>>,
    out: TermTable,
}

impl Lowerer<'_> {
    fn rule<'a>(&'a self, node: &AstNodeRef) -> Option<&'a str> {
        node.rule()
            .and_then(|rule| self.rules.get(&rule))
            .map(AsRef::as_ref)
    }

    fn token_text(&self, node: &AstNodeRef) -> Option<String> {
        node.token_id()
            .and_then(|token| self.payloads.get(&token))
            .map(std::string::ToString::to_string)
    }

    fn child(node: &AstNodeRef, index: usize) -> Option<AstNodeRef> {
        node.children().into_iter().nth(index)
    }

    /// Lowers one term at its stable position identity.
    fn term(&mut self, node: &AstNodeRef, id: u64) -> Option<u64> {
        if node.category().is_some() {
            self.out
                .diagnostics
                .push(format!("recovered syntax at node #{:x}", node.raw()));
            return None;
        }
        let Some(rule) = self.rule(node) else {
            self.out
                .diagnostics
                .push(format!("token #{:x} is not a term", node.raw()));
            return None;
        };
        match rule {
            // Transparent wrappers produced by the start/alternative rules: the
            // wrapped term keeps the identity the parent assigned to the slot.
            "Program" | "Term" | "Atom" => {
                let child = Self::child(node, 0)?;
                self.term(&child, id)
            }
            "Parens" => {
                let child = Self::child(node, 1)?;
                self.term(&child, id)
            }
            "Lam" => {
                let param = self.token_text(&Self::child(node, 1)?).unwrap_or_default();
                let annotation = Self::child(node, 3)
                    .map_or_else(|| String::from("?"), |node| self.type_text(&node));
                let body = Self::child(node, 5).and_then(|body| self.term(&body, child_id(id, 0)));
                let body = body?;
                self.out.nodes.insert(id, Term::Lam { body });
                self.out.names.insert(id, param);
                self.out.annotations.insert(id, annotation);
                Some(id)
            }
            "Let" => {
                let bound = self.token_text(&Self::child(node, 1)?).unwrap_or_default();
                let binding =
                    Self::child(node, 3).and_then(|binding| self.term(&binding, child_id(id, 0)));
                let body = Self::child(node, 5).and_then(|body| self.term(&body, child_id(id, 1)));
                let (Some(binding), Some(body)) = (binding, body) else {
                    return None;
                };
                self.out.nodes.insert(id, Term::Let { binding, body });
                self.out.names.insert(id, bound);
                Some(id)
            }
            "If" => {
                let cond = Self::child(node, 1).and_then(|cond| self.term(&cond, child_id(id, 0)));
                let yes = Self::child(node, 3).and_then(|yes| self.term(&yes, child_id(id, 1)));
                let no = Self::child(node, 5).and_then(|no| self.term(&no, child_id(id, 2)));
                let (Some(cond), Some(yes), Some(no)) = (cond, yes, no) else {
                    return None;
                };
                self.out.nodes.insert(id, Term::If { cond, yes, no });
                Some(id)
            }
            "App" => {
                let func = Self::child(node, 0).and_then(|func| self.term(&func, child_id(id, 0)));
                let arg = Self::child(node, 1).and_then(|arg| self.term(&arg, child_id(id, 1)));
                let (Some(func), Some(arg)) = (func, arg) else {
                    return None;
                };
                self.out.nodes.insert(id, Term::App { func, arg });
                Some(id)
            }
            "Variable" => {
                let name = self.token_text(&Self::child(node, 0)?).unwrap_or_default();
                self.out.nodes.insert(id, Term::Var);
                self.out.names.insert(id, name);
                Some(id)
            }
            "Integer" => {
                let text = self.token_text(&Self::child(node, 0)?).unwrap_or_default();
                match text.parse::<i64>() {
                    Ok(value) => {
                        self.out.nodes.insert(id, Term::Int);
                        self.out.literals.insert(id, Literal::Int(value));
                        Some(id)
                    }
                    Err(_) => {
                        self.out.diagnostics.push(format!(
                            "integer `{text}` at node #{:x} is out of range",
                            node.raw()
                        ));
                        None
                    }
                }
            }
            "Boolean" => {
                let text = self.token_text(&Self::child(node, 0)?).unwrap_or_default();
                self.out.nodes.insert(id, Term::Bool);
                self.out.literals.insert(id, Literal::Bool(text == "true"));
                Some(id)
            }
            other => {
                self.out
                    .diagnostics
                    .push(format!("unexpected `{other}` at node #{:x}", node.raw()));
                None
            }
        }
    }

    fn type_text(&mut self, node: &AstNodeRef) -> String {
        match self.rule(node) {
            Some("Type") => Self::child(node, 0)
                .map_or_else(|| String::from("?"), |child| self.type_text(&child)),
            Some("TypeArrow") => {
                let domain = Self::child(node, 0)
                    .map_or_else(|| String::from("?"), |child| self.type_text(&child));
                let codomain = Self::child(node, 2)
                    .map_or_else(|| String::from("?"), |child| self.type_text(&child));
                format!("{domain} -> {codomain}")
            }
            Some("TypeAtom") => {
                let children = node.children();
                let Some(first) = children.first() else {
                    return String::from("?");
                };
                match self.token_text(first).as_deref() {
                    Some("Int") => String::from("Int"),
                    Some("Bool") => String::from("Bool"),
                    Some("(") => children.get(1).map_or_else(
                        || String::from("?"),
                        |inner| format!("({})", self.type_text(inner)),
                    ),
                    _ => {
                        self.out
                            .diagnostics
                            .push(format!("unknown type annotation at node #{:x}", node.raw()));
                        String::from("?")
                    }
                }
            }
            _ => {
                self.out
                    .diagnostics
                    .push(format!("unexpected type node #{:x}", node.raw()));
                String::from("?")
            }
        }
    }
}
