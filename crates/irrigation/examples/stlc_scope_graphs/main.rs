//! A simply typed lambda calculus, resolved with scope graphs, on the
//! Observatory board.
//!
//! ```sh
//! cargo run --example stlc_scope_graphs -p irrigation --features observatory
//! ```
//!
//! Four components do the whole analysis, and each one emits as it walks:
//!
//! ```text
//! resolve(term, scope, chain)      -> scope graph: scopes, term/binding nodes, P/D/R/child
//! check(term)                      -> scope graph: type scopes + T edges, types/errors
//! evaluate(term, env, depth)       -> Evaluation: one row per invocation, plus the summary
//! lower(term, chain, at)           -> Machine: one MIPS row per instruction, in order
//! ```
//!
//! The typed tree is a `#[tree_view]` declaration in [`syntax`], and the grammar
//! says which production builds which variant; the parser publishes it from the
//! same records as the raw AST, with the same node identities. So `resolve` walks
//! a `TermRef`, not a tree index: there is no classifier and no lowering stage.
//! Entity identities are not written by this file — an emission is identified by
//! the expression that performs it, and a shared type scope is interned by the
//! type. The only analysis data the stages hand each other is `targets` (a
//! reference's binding) and `types` (a term's type).
//!
//! Only the source text is editable. The lexer and parser are components on the
//! board, and the raw AST is what the parser published.

mod eval;
mod grammar;
mod graph;
mod machine;
mod report;
mod syntax;
mod types;

use std::sync::{Arc, Mutex};

use irrigation::{Runtime, TreeNodeId, incremental_data};
use irrigation_observe::{Observatory, ObserveLevel, observed_runtime};
use irrigation_syntax::{
    AstMeta, LexerComponent, ParserComponent, SourceFile, TextComponent, TextDocument,
};

use crate::eval::{Env, Outcome, Value};
use crate::graph::{BindingKind, ScopeEdge, ScopeNode, TermKind};
use crate::machine::Machine;
use crate::report::Line;
use crate::syntax::TermRef;
use crate::types::Type;

/// The scope graph plus the term-keyed analysis maps.
///
/// `resolve` owns `graph`, `terms`, `targets`, and `name_errors`; `check` owns
/// `types` and `type_errors`. Every entity is claimed by the emission expression
/// that wrote it, so the two writers never meet on a unit.
#[incremental_data]
pub struct ScopeGraph {
    /// Scopes, terms, and types; `P`, `D`, `R`, `child`, and `T` edges.
    pub graph: irrigation::Graph<ScopeNode, ScopeEdge>,
    /// Term (parser identity) -> the graph node the resolver emitted for it.
    pub terms: Map<TreeNodeId, u64>,
    /// Reference term -> the binding term it resolved to.
    pub targets: Map<TreeNodeId, TermRef>,
    /// Unresolved and shadowing diagnostics.
    pub name_errors: Map<TreeNodeId, Line>,
    /// Each checked term's type.
    pub types: Map<TreeNodeId, Type>,
    /// Ill-typed terms and failed annotations.
    pub type_errors: Map<TreeNodeId, Line>,
}

/// The evaluator's View.
#[incremental_data]
pub struct Evaluation {
    /// The value's text; empty means no value.
    pub result: Line,
    /// `Int`, `Bool`, `function`, or `stuck`.
    pub kind: Line,
    /// Beta and `let` steps performed.
    pub steps: u64,
    /// Why evaluation did not produce a value, if it did not.
    pub diagnostics: List<Line>,
    /// One row per `evaluate` invocation, keyed by that invocation's identity.
    ///
    /// A row is per invocation rather than per term because a lambda body can run
    /// under several environments.
    pub outcomes: Map<u64, Outcome>,
}

/// One binder in scope. `binder` is the lambda/`let` term that declares the name.
///
/// A frame carries the binder's *occurrence*, not its spelling: the name is read
/// from the binder's token at the two places that need it. An argument is the
/// invocation's identity, so passing the name would re-create every entity
/// emitted beneath the binder whenever the user renames it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Frame {
    /// The scope the binder introduced.
    pub scope: u64,
    /// The declaring term.
    pub binder: TermRef,
}

/// The four stages and the root that runs them.
mod pipeline {
    use super::{
        BindingKind, Env, Evaluation, Frame, Line, Machine, Outcome, ScopeEdge, ScopeGraph,
        ScopeNode, TermKind, TermRef, Type, Value, eval, machine, syntax,
    };
    use irrigation::component;

    /// Resolves names and emits the name half of the scope graph.
    ///
    /// A binder's body is resolved under a chain carried as an argument: the graph
    /// is the witness, and reading a declaration back out of it would wait on the
    /// enclosing invocation's uncommitted write. Every emission is its own
    /// expression in this body: an entity's identity is the expression that
    /// emitted it, so a helper that emitted two entities would give them one name.
    #[component(callable)]
    pub fn resolve(term: TermRef, scope: u64, chain: Vec<Frame>) -> Option<u64> {
        match term.kind() {
            None => None,
            Some(TermKind::Var) => {
                let name = term.name().unwrap_or_default();
                let node = ScopeGraph.graph().emit_node(ScopeNode::Term {
                    kind: TermKind::Var,
                    binding: None,
                    label: name.clone(),
                });
                ScopeGraph.define_terms(term.raw(), node);
                ScopeGraph
                    .graph()
                    .emit_edge(scope, node, ScopeEdge::Occurs { name: name.clone() });
                match chain
                    .iter()
                    .rev()
                    .find(|frame| frame.binder.name().as_deref() == Some(name.as_str()))
                    .map(|frame| frame.binder.clone())
                {
                    Some(binder) => ScopeGraph.define_targets(term.raw(), binder),
                    None => ScopeGraph
                        .define_name_errors(term.raw(), Line(format!("unresolved `{name}`"))),
                }
                Some(node)
            }
            Some(kind) => {
                let name = term.name().unwrap_or_default();
                let binder = binding_of(kind);
                let node = ScopeGraph.graph().emit_node(ScopeNode::Term {
                    kind,
                    binding: binder,
                    label: label_of(kind, &name),
                });
                ScopeGraph.define_terms(term.raw(), node);
                if binder.is_some() {
                    let inner = ScopeGraph.graph().emit_node(ScopeNode::Scope {
                        label: String::from(if kind == TermKind::Lam {
                            "lambda"
                        } else {
                            "let"
                        }),
                    });
                    ScopeGraph
                        .graph()
                        .emit_edge(inner, scope, ScopeEdge::Parent);
                    if chain
                        .iter()
                        .any(|frame| frame.binder.name().as_deref() == Some(name.as_str()))
                    {
                        ScopeGraph.define_name_errors(
                            term.raw(),
                            Line(format!("`{name}` shadows an outer binding")),
                        );
                    }
                    ScopeGraph
                        .graph()
                        .emit_edge(inner, node, ScopeEdge::Declares { name });
                    let next = chain
                        .iter()
                        .cloned()
                        .chain([Frame {
                            scope: inner,
                            binder: term.clone(),
                        }])
                        .collect();
                    if let Some(body) = term.body() {
                        if let Some(resolved) = resolve(body, inner, next) {
                            ScopeGraph.graph().emit_edge(
                                node,
                                resolved,
                                ScopeEdge::Child {
                                    slot: u8::from(kind == TermKind::Let),
                                },
                            );
                        }
                    }
                    if let Some(value) = term.value() {
                        if let Some(resolved) = resolve(value, scope, chain) {
                            ScopeGraph.graph().emit_edge(
                                node,
                                resolved,
                                ScopeEdge::Child { slot: 0 },
                            );
                        }
                    }
                } else if kind == TermKind::App {
                    if let Some(func) = term.func() {
                        if let Some(resolved) = resolve(func, scope, chain.clone()) {
                            ScopeGraph.graph().emit_edge(
                                node,
                                resolved,
                                ScopeEdge::Child { slot: 0 },
                            );
                        }
                    }
                    if let Some(arg) = term.arg() {
                        if let Some(resolved) = resolve(arg, scope, chain) {
                            ScopeGraph.graph().emit_edge(
                                node,
                                resolved,
                                ScopeEdge::Child { slot: 1 },
                            );
                        }
                    }
                } else if kind == TermKind::If {
                    if let Some(cond) = term.cond() {
                        if let Some(resolved) = resolve(cond, scope, chain.clone()) {
                            ScopeGraph.graph().emit_edge(
                                node,
                                resolved,
                                ScopeEdge::Child { slot: 0 },
                            );
                        }
                    }
                    if let Some(yes) = term.yes() {
                        if let Some(resolved) = resolve(yes, scope, chain.clone()) {
                            ScopeGraph.graph().emit_edge(
                                node,
                                resolved,
                                ScopeEdge::Child { slot: 1 },
                            );
                        }
                    }
                    if let Some(no) = term.no() {
                        if let Some(resolved) = resolve(no, scope, chain) {
                            ScopeGraph.graph().emit_edge(
                                node,
                                resolved,
                                ScopeEdge::Child { slot: 2 },
                            );
                        }
                    }
                }
                Some(node)
            }
        }
    }

    /// The binding a term introduces, if it binds one.
    fn binding_of(kind: TermKind) -> Option<BindingKind> {
        match kind {
            TermKind::Lam => Some(BindingKind::Lambda),
            TermKind::Let => Some(BindingKind::Let),
            _ => None,
        }
    }

    /// A term's short label.
    fn label_of(kind: TermKind, name: &str) -> String {
        match kind {
            TermKind::Lam => format!("λ{name}"),
            TermKind::Let => format!("let {name}"),
            TermKind::App => String::from("app"),
            TermKind::If => String::from("if"),
            TermKind::Int => String::from("int"),
            TermKind::Bool => String::from("bool"),
            TermKind::Var => String::from("var"),
        }
    }

    /// Checks one term and emits its type scope and `T` edge.
    #[component(callable)]
    pub fn check(term: TermRef) -> Type {
        let ty = match term.kind() {
            Some(TermKind::Var) => match ScopeGraph.targets().get(&term.raw()) {
                Some(binding) => match binding.kind() {
                    Some(TermKind::Lam) => syntax::annotation(&binding),
                    Some(TermKind::Let) => match binding.value() {
                        Some(value) => check(value),
                        None => Type::Error,
                    },
                    _ => Type::Error,
                },
                None => Type::Error,
            },
            Some(TermKind::Lam) => match (syntax::annotation(&term), term.body()) {
                (Type::Error, _) | (_, None) => Type::Error,
                (domain, Some(body)) => match check(body) {
                    Type::Error => Type::Error,
                    codomain => Type::Fun(Box::new(domain), Box::new(codomain)),
                },
            },
            Some(TermKind::Let) => match term.body() {
                Some(body) => {
                    if let Some(value) = term.value() {
                        let _ = check(value);
                    }
                    check(body)
                }
                None => Type::Error,
            },
            Some(TermKind::App) => match (term.func(), term.arg()) {
                (Some(func), Some(arg)) => match (check(func), check(arg)) {
                    (Type::Fun(domain, codomain), argument) if *domain == argument => *codomain,
                    _ => Type::Error,
                },
                _ => Type::Error,
            },
            Some(TermKind::If) => match (term.cond(), term.yes(), term.no()) {
                (Some(cond), Some(yes), Some(no)) => {
                    let condition = check(cond);
                    let yes = check(yes);
                    let no = check(no);
                    if condition == Type::Bool && yes == no {
                        yes
                    } else {
                        Type::Error
                    }
                }
                _ => Type::Error,
            },
            Some(TermKind::Int) => Type::Int,
            Some(TermKind::Bool) => Type::Bool,
            None => Type::Error,
        };
        ScopeGraph.define_types(term.raw(), ty.clone());
        if ty == Type::Error {
            ScopeGraph.define_type_errors(
                term.raw(),
                Line(format!("term #{} is ill-typed", term.raw())),
            );
        }
        let label = ty.render();
        let type_scope = ScopeGraph
            .graph()
            .emit_shared(label.clone(), ScopeNode::Type { label });
        if let Some(node) = ScopeGraph.terms().get(&term.raw()) {
            ScopeGraph
                .graph()
                .emit_edge(node, type_scope, ScopeEdge::TypeOf);
        }
        ty
    }

    /// Evaluates one term under one environment and contributes its row.
    #[component(callable)]
    pub fn evaluate(term: TermRef, env: Env, depth: u32) -> Outcome {
        let outcome = if depth > eval::DEPTH_LIMIT {
            Outcome::stuck(format!("stopped after {} nested terms", eval::DEPTH_LIMIT))
        } else {
            match term.kind() {
                Some(TermKind::Var) => match ScopeGraph.targets().get(&term.raw()) {
                    Some(binding) => match env
                        .iter()
                        .rev()
                        .find(|entry| entry.0 == binding.raw())
                        .map(|entry| entry.1.clone())
                    {
                        Some(value) => Outcome::value(value),
                        None => Outcome::stuck(format!("unbound variable #{}", term.raw())),
                    },
                    None => Outcome::stuck(format!("unresolved variable #{}", term.raw())),
                },
                Some(TermKind::Lam) => match term.body() {
                    Some(body) => Outcome::value(Value::Closure {
                        binder: term.raw(),
                        body,
                        env: env.clone(),
                    }),
                    None => Outcome::stuck(format!("lambda #{} has no body", term.raw())),
                },
                Some(TermKind::App) => match (term.func(), term.arg()) {
                    (Some(func), Some(arg)) => {
                        let function = evaluate(func, env.clone(), depth + 1);
                        let argument = evaluate(arg, env, depth + 1);
                        match function.value.clone() {
                            Some(Value::Closure {
                                binder,
                                body,
                                env: captured,
                            }) => match argument.value.clone() {
                                Some(value) => {
                                    let next =
                                        captured.iter().cloned().chain([(binder, value)]).collect();
                                    evaluate(body, next, depth + 1)
                                        .plus_steps(function.steps + argument.steps + 1)
                                        .with_diagnostics(function.diagnostics.clone())
                                }
                                None => function.merge(argument).stuck_with(format!(
                                    "argument of #{} did not evaluate",
                                    term.raw()
                                )),
                            },
                            _ => function.merge(argument).stuck_with(format!(
                                "application #{} of a non-function",
                                term.raw()
                            )),
                        }
                    }
                    _ => Outcome::stuck(format!("application #{} is incomplete", term.raw())),
                },
                Some(TermKind::Let) => {
                    let bound = match term.value() {
                        Some(value) => evaluate(value, env.clone(), depth + 1),
                        None => Outcome::stuck(format!("let #{} has no value", term.raw())),
                    };
                    match bound.value.clone() {
                        Some(value) => {
                            let next = env.iter().cloned().chain([(term.raw(), value)]).collect();
                            match term.body() {
                                Some(body) => evaluate(body, next, depth + 1)
                                    .plus_steps(bound.steps + 1)
                                    .with_diagnostics(bound.diagnostics.clone()),
                                None => bound,
                            }
                        }
                        None => bound,
                    }
                }
                Some(TermKind::Int) => Outcome::value(Value::Int(term.int().unwrap_or_default())),
                Some(TermKind::Bool) => {
                    Outcome::value(Value::Bool(term.flag().unwrap_or_default()))
                }
                Some(TermKind::If) => match (term.cond(), term.yes(), term.no()) {
                    (Some(cond), Some(yes), Some(no)) => {
                        let condition = evaluate(cond, env.clone(), depth + 1);
                        match condition.value.clone() {
                            Some(Value::Bool(true)) => evaluate(yes, env, depth + 1)
                                .plus_steps(condition.steps)
                                .with_diagnostics(condition.diagnostics.clone()),
                            Some(Value::Bool(false)) => evaluate(no, env, depth + 1)
                                .plus_steps(condition.steps)
                                .with_diagnostics(condition.diagnostics.clone()),
                            _ => condition
                                .stuck_with(format!("if #{}: condition is not Bool", term.raw())),
                        }
                    }
                    _ => Outcome::stuck(format!("if #{} is incomplete", term.raw())),
                },
                None => Outcome::stuck(format!("term #{} has no declared variant", term.raw())),
            }
        };
        Evaluation.define_outcomes(irrigation::emission_id(), outcome.clone());
        outcome
    }

    /// The root composes the four stages and writes the one summary.
    #[component(root)]
    pub fn stlc() -> (ScopeGraph, Evaluation, Machine) {
        let program = Machine.program();
        // The shell is fixed, so it is written once and the entry term is laid
        // out after it; the exit goes after whatever the lowering returned.
        program.define(0, ".data".into(), "shell");
        program.define(1, "heap: .space 65536".into(), "shell");
        program.define(2, ".text".into(), "shell");
        program.define(3, ".globl main".into(), "shell");
        program.define(4, "main:".into(), "shell");
        program.define(5, "li $s0, 0".into(), "shell");
        program.define(6, "la $t3, heap".into(), "shell");
        // A pattern binding observes the value it projects, so `root` is a
        // tracked local: the stage arguments read from it follow a new tree.
        let end = match syntax::root() {
            Some(root) => {
                let module = ScopeGraph.graph().emit_node(ScopeNode::Scope {
                    label: String::from("module"),
                });
                let _ = resolve(root.clone(), module, Vec::new());
                let _ = check(root.clone());
                let outcome = evaluate(root.clone(), Vec::new(), 0);
                Evaluation.define_result(Line(outcome.text()));
                Evaluation.define_kind(Line(outcome.kind().to_owned()));
                Evaluation.define_steps(outcome.steps);
                Evaluation.define_diagnostics(outcome.diagnostics.clone());
                machine::lower(root, Vec::new(), machine::SHELL)
            }
            None => {
                program.define(machine::SHELL, "li $v0, 0".into(), "shell");
                machine::SHELL + 1
            }
        };
        program.define(end, "move $a0, $v0".into(), "shell");
        program.define(end + 1, "li $v0, 1".into(), "shell");
        program.define(end + 2, "syscall".into(), "shell");
        program.define(end + 3, "li $v0, 10".into(), "shell");
        program.define(end + 4, "syscall".into(), "shell");
        (ScopeGraph, Evaluation, Machine)
    }
}

fn main() -> std::io::Result<()> {
    let (runtime, recorder) = observed_runtime(ObserveLevel::Nodes);
    let grammar = grammar::grammar();
    runtime
        .install_general_producer(TextComponent)
        .expect("text component");
    runtime
        .install_general_producer(LexerComponent::new(grammar.lexer().clone()))
        .expect("lexer component");
    runtime
        .install_general_producer(
            ParserComponent::with_tree::<syntax::ast::Syntax>(grammar)
                .expect("the grammar maps onto the declared tree"),
        )
        .expect("parser component");
    let app = runtime
        .install_root(pipeline::Stlc)
        .expect("install the pipeline");

    let document = Arc::new(Mutex::new(TextDocument::new(grammar::SEED)));
    runtime
        .edit(|edit| {
            edit.view::<SourceFile>()
                .set_document(document.lock().expect("document").clone());
        })
        .expect("seed the source");
    let _ = app.read().expect("demand the pipeline");

    let observatory = Observatory::builder(runtime.clone(), recorder)
        .bind("127.0.0.1:7801")
        .bind_from_args()
        .static_dir(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../web/observatory/dist"
        ))
        .text_view::<SourceFile>(
            {
                let document = Arc::clone(&document);
                move || document.lock().expect("document").text().to_owned()
            },
            {
                let document = Arc::clone(&document);
                move |text: &str| {
                    // The editor sends its whole buffer; commit one edit that
                    // separates it from the document the pipeline holds, so the
                    // lexer and parser see a keystroke and not a new file.
                    let next = {
                        let current = document.lock().expect("document").clone();
                        current.replace_with(text).unwrap_or(current)
                    };
                    runtime
                        .edit(|edit| edit.view::<SourceFile>().set_document(next.clone()))
                        .map_err(|error| format!("{error}"))?;
                    *document.lock().expect("document") = next;
                    let _ = app.read().expect("demand the pipeline");
                    Ok(summary(&runtime))
                }
            },
        )
        .start()?;

    println!("Observatory listening on {}", observatory.url());
    println!(
        "Edit the source plate: try `\\x:Int. x 1`, `let x = true in if x then 1 else 2`, or break a name."
    );
    observatory.wait();
    Ok(())
}

/// A one-line report for the editor's reply.
///
/// The whole term's type is its `types` entry, keyed by the parser's term
/// identity.
fn summary(runtime: &Runtime) -> String {
    use std::fmt::Write as _;

    let Ok(snapshot) = runtime.read() else {
        return String::from("the runtime is not available");
    };
    let graph = snapshot.view::<ScopeGraph>();
    let evaluation = snapshot.view::<Evaluation>();
    let root = snapshot
        .view::<syntax::ast::Syntax>()
        .root()
        .and_then(|document| document.term());
    let whole = root
        .and_then(|root| graph.types_get(&root.raw()))
        .map(|ty| ty.render());
    let name_errors = graph.name_errors();
    let type_errors = graph.type_errors();
    let parse_errors = snapshot.view::<AstMeta>().diagnostics().len();
    let result = evaluation.result().0;
    let kind = evaluation.kind().0;
    let steps = evaluation.steps();
    let mut reply = format!(
        "{} · {} · {steps} steps",
        whole.unwrap_or_else(|| String::from("untyped")),
        if result.is_empty() {
            String::from("no value")
        } else {
            format!("{result} : {kind}")
        }
    );
    if parse_errors > 0 {
        let _ = write!(reply, " · {parse_errors} parse error(s)");
    }
    if !name_errors.is_empty() {
        let first = name_errors
            .to_vec()
            .first()
            .map(|(_, line)| line.0.clone())
            .unwrap_or_default();
        let _ = write!(
            reply,
            " · {} name diagnostic(s): {first}",
            name_errors.len()
        );
    }
    if !type_errors.is_empty() {
        let first = type_errors
            .to_vec()
            .first()
            .map(|(_, line)| line.0.clone())
            .unwrap_or_default();
        let _ = write!(reply, " · {} type error(s): {first}", type_errors.len());
    }
    reply
}
