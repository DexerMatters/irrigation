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

#[cfg(test)]
mod sim;

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

#[cfg(test)]
mod tests {
    use super::ScopeGraph;
    use irrigation::Runtime;
    use irrigation_syntax::{
        AstMeta, AstTree, LexerComponent, ParserComponent, SourceFile, Text, TextComponent,
        TextDocument, Tokens,
    };

    pub(super) fn board(text: &str) -> Runtime {
        let runtime = Runtime::new();
        let grammar = super::grammar::grammar();
        runtime
            .install_general_producer(TextComponent)
            .expect("text component");
        runtime
            .install_general_producer(LexerComponent::new(grammar.lexer().clone()))
            .expect("lexer component");
        runtime
            .install_general_producer(
                ParserComponent::with_tree::<super::syntax::ast::Syntax>(grammar)
                    .expect("the grammar maps onto the declared tree"),
            )
            .expect("parser component");
        runtime.install(super::pipeline::Stlc).expect("install");
        runtime
            .edit(|edit| {
                edit.view::<SourceFile>()
                    .set_document(TextDocument::new(text))
            })
            .expect("seed");
        let _ = runtime
            .read_outputs::<super::pipeline::Stlc>()
            .expect("demand");
        runtime
    }

    pub(super) fn edit(runtime: &Runtime, text: &str) {
        let current = {
            let snapshot = runtime.read().expect("snapshot");
            snapshot.view::<SourceFile>().document()
        };
        let next = current.replace_with(text).unwrap_or(current);
        runtime
            .edit(|edit| edit.view::<SourceFile>().set_document(next))
            .expect("edit");
        let _ = runtime
            .read_outputs::<super::pipeline::Stlc>()
            .expect("demand");
    }

    fn executed(runtime: &Runtime, name: &str) -> irrigation::Revision {
        let suffix = format!("::{name}");
        runtime
            .component_facts()
            .into_iter()
            .find(|component| component.name.ends_with(&suffix))
            .map_or_else(
                || panic!("no component for {name}"),
                |component| component.executed_at,
            )
    }

    fn result(runtime: &Runtime) -> String {
        runtime
            .read()
            .expect("snapshot")
            .view::<super::Evaluation>()
            .result()
            .0
    }

    /// The listing, one instruction per element, in program order.
    fn listing(runtime: &Runtime) -> Vec<String> {
        runtime
            .read()
            .expect("snapshot")
            .view::<super::Machine>()
            .program()
            .to_vec()
            .into_iter()
            .map(|(_, line)| line.0)
            .collect()
    }

    /// The words the listing prints under the simulator.
    fn printed(runtime: &Runtime) -> Vec<i32> {
        super::sim::run(&listing(runtime)).printed
    }

    fn kind(runtime: &Runtime) -> String {
        runtime
            .read()
            .expect("snapshot")
            .view::<super::Evaluation>()
            .kind()
            .0
    }

    fn targets(runtime: &Runtime) -> Vec<(irrigation::TreeNodeId, irrigation::TreeNodeId)> {
        runtime
            .read()
            .expect("snapshot")
            .view::<ScopeGraph>()
            .targets()
            .into_iter()
            .map(|(term, binding)| (term, binding.raw()))
            .collect()
    }

    fn name_errors(runtime: &Runtime) -> Vec<String> {
        runtime
            .read()
            .expect("snapshot")
            .view::<ScopeGraph>()
            .name_errors()
            .into_iter()
            .map(|(_, line)| line.0)
            .collect()
    }

    fn type_errors(runtime: &Runtime) -> Vec<String> {
        runtime
            .read()
            .expect("snapshot")
            .view::<ScopeGraph>()
            .type_errors()
            .into_iter()
            .map(|(_, line)| line.0)
            .collect()
    }

    fn type_values(runtime: &Runtime) -> Vec<super::types::Type> {
        runtime
            .read()
            .expect("snapshot")
            .view::<ScopeGraph>()
            .types()
            .into_iter()
            .map(|(_, ty)| ty)
            .collect()
    }

    fn whole_type(runtime: &Runtime) -> Option<String> {
        let snapshot = runtime.read().expect("snapshot");
        let root = snapshot
            .view::<super::syntax::ast::Syntax>()
            .root()
            .and_then(|document| document.term())?;
        snapshot
            .view::<ScopeGraph>()
            .types_get(&root.raw())
            .map(|ty| ty.render())
    }

    fn int_type_scopes(runtime: &Runtime) -> usize {
        let snapshot = runtime.read().expect("snapshot");
        let graph = snapshot.view::<ScopeGraph>();
        graph
            .graph_nodes()
            .into_iter()
            .filter(|id| {
                matches!(
                    graph.graph_node(*id),
                    Some(super::graph::ScopeNode::Type { label }) if label == "Int"
                )
            })
            .count()
    }

    #[test]
    fn the_seed_evaluates_to_one() {
        let runtime = board(super::grammar::SEED);
        assert_eq!(whole_type(&runtime).as_deref(), Some("Int"));
        assert_eq!(result(&runtime), "1");
        assert_eq!(kind(&runtime), "Int");
    }

    #[test]
    fn a_later_declaration_does_not_capture_an_earlier_body() {
        let runtime = board(super::grammar::SEED);
        assert_eq!(result(&runtime), "1");
    }

    #[test]
    fn shadowing_is_reported_with_its_distance() {
        let runtime = board(super::grammar::SEED);
        let diagnostics = name_errors(&runtime);
        assert!(
            diagnostics
                .iter()
                .any(|line| line.contains("shadows") && line.contains("`x`")),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn the_graph_holds_term_bindings_and_type_scopes() {
        let runtime = board(super::grammar::SEED);
        let snapshot = runtime.read().expect("snapshot");
        let graph = snapshot.view::<ScopeGraph>();
        assert!(
            graph.graph_nodes().into_iter().any(|id| matches!(
                graph.graph_node(id),
                Some(super::graph::ScopeNode::Term {
                    binding: Some(_),
                    ..
                })
            )),
            "a binder must be a term node with a binding payload"
        );
        assert!(graph.graph_edges().into_iter().any(|id| matches!(
            graph.graph_edge(id).map(|edge| edge.value),
            Some(super::graph::ScopeEdge::Declares { .. })
        )));
        assert!(graph.graph_edges().into_iter().any(|id| matches!(
            graph.graph_edge(id).map(|edge| edge.value),
            Some(super::graph::ScopeEdge::TypeOf)
        )));
        assert!(
            !graph.types().is_empty(),
            "the checker must write term type entries"
        );
        assert_eq!(graph.type_errors().len(), 0);
    }

    #[test]
    fn equal_types_share_one_type_scope_and_keep_term_entries() {
        let runtime = board(super::grammar::SEED);
        let values = type_values(&runtime);
        assert!(
            values
                .iter()
                .filter(|ty| **ty == super::types::Type::Int)
                .count()
                >= 2,
            "every Int-typed term keeps its own map entry: {values:?}"
        );
        assert_eq!(
            int_type_scopes(&runtime),
            1,
            "equal types share one canonical type scope"
        );
    }

    #[test]
    fn a_type_annotation_edit_rechecks_the_lambda() {
        let runtime = board(super::grammar::SEED);
        assert_eq!(whole_type(&runtime).as_deref(), Some("Int"));
        let typing_before = executed(&runtime, "check");

        // Change the annotation *and* the argument, so the edited program is
        // still well typed and the checker's new answer is observable.
        edit(
            &runtime,
            "let x = 1 in\nlet f = \\y:Bool. x in\nlet x = true in\nf true",
        );

        assert_eq!(whole_type(&runtime).as_deref(), Some("Int"));
        assert!(type_errors(&runtime).is_empty());
        assert!(executed(&runtime, "check") > typing_before);
        assert_eq!(result(&runtime), "1");
    }

    #[test]
    fn a_literal_edit_evaluates_the_new_literal() {
        let runtime = board("let x = 1 in x");
        let evaluation_before = executed(&runtime, "evaluate");

        edit(&runtime, "let x = 2 in x");

        assert_eq!(result(&runtime), "2");
        assert_eq!(kind(&runtime), "Int");
        assert!(executed(&runtime, "evaluate") > evaluation_before);
    }

    #[test]
    fn a_consistent_rename_keeps_the_program_resolved() {
        let runtime = board("let x = 1 in x");
        let names_before = executed(&runtime, "resolve");

        edit(&runtime, "let y = 1 in y");

        assert!(executed(&runtime, "resolve") > names_before);
        assert_eq!(result(&runtime), "1");
        assert!(
            name_errors(&runtime)
                .iter()
                .all(|line| !line.contains("unresolved")),
            "a consistent rename leaves nothing unresolved: {:?}",
            name_errors(&runtime)
        );
        assert_eq!(
            targets(&runtime).len(),
            1,
            "the renamed reference still resolves"
        );
    }

    #[test]
    fn a_structural_edit_retracts_stale_type_entries() {
        let runtime = board("let x = true in x");
        assert!(type_values(&runtime).contains(&super::types::Type::Bool));
        edit(&runtime, "let x = 1 in x");
        assert!(!type_values(&runtime).contains(&super::types::Type::Bool));
        assert_eq!(result(&runtime), "1");
    }

    #[test]
    fn an_ill_typed_program_reports_errors_and_keeps_type_entries() {
        let runtime = board("\\x:Int. x x");
        assert!(
            !type_errors(&runtime).is_empty(),
            "the checker must report the mismatch"
        );
        assert!(
            !type_values(&runtime).is_empty(),
            "an ill-typed term still gets a type entry"
        );
        let snapshot = runtime.read().expect("snapshot");
        assert!(snapshot.view::<AstTree>().root().is_some());
    }

    #[test]
    fn a_retargeted_reference_drops_its_old_target() {
        let runtime = board("let x = 1 in x");
        assert_eq!(targets(&runtime).len(), 1);
        edit(&runtime, "let x = 1 in y");
        assert!(
            targets(&runtime).is_empty(),
            "the old target must be retracted, not left stale"
        );
        assert!(
            name_errors(&runtime)
                .iter()
                .any(|line| line.contains("unresolved `y`"))
        );
    }

    #[test]
    fn an_unresolved_name_has_no_target_and_a_name_error() {
        let runtime = board("\\x:Int. y");
        let diagnostics = name_errors(&runtime);
        assert!(
            diagnostics
                .iter()
                .any(|line| line.contains("unresolved `y`")),
            "{diagnostics:?}"
        );
        assert!(
            targets(&runtime).is_empty(),
            "an unresolved reference has no target"
        );
    }

    #[test]
    fn an_ill_typed_if_is_stuck() {
        let runtime = board("if 1 then 2 else 3");
        assert_eq!(result(&runtime), "");
        assert_eq!(kind(&runtime), "stuck");
        assert!(!type_errors(&runtime).is_empty());
    }

    #[test]
    fn a_parse_error_is_reported_and_the_last_tree_stays() {
        let runtime = board("let x = 1 in x");
        edit(&runtime, "let x =");
        let snapshot = runtime.read().expect("snapshot");
        assert!(
            !snapshot.view::<AstMeta>().diagnostics().is_empty(),
            "the parser must report the broken edit"
        );
        assert!(snapshot.view::<AstTree>().root().is_some());
    }

    #[test]
    fn a_repeated_read_executes_nothing() {
        let runtime = board("let x = 1 in x");
        let before = runtime.stats().executions;
        let _ = runtime
            .read_outputs::<super::pipeline::Stlc>()
            .expect("warm read");
        assert_eq!(
            runtime.stats().executions,
            before,
            "a demand at the same revision reuses every certified node"
        );
    }

    /// Node identities, in order.
    fn scope_nodes(runtime: &Runtime) -> Vec<u64> {
        let snapshot = runtime.read().expect("snapshot");
        snapshot.view::<ScopeGraph>().graph_nodes()
    }

    /// Edge identities, in order.
    fn scope_edges(runtime: &Runtime) -> Vec<u64> {
        let snapshot = runtime.read().expect("snapshot");
        snapshot.view::<ScopeGraph>().graph_edges()
    }

    /// One `(identity, payload)` pair per node.
    fn node_payloads(runtime: &Runtime, ids: &[u64]) -> Vec<(u64, super::ScopeNode)> {
        let snapshot = runtime.read().expect("snapshot");
        let view = snapshot.view::<ScopeGraph>();
        ids.iter()
            .filter_map(|id| view.graph_node(*id).map(|node| (*id, node)))
            .collect()
    }

    /// One `(identity, endpoints, payload)` tuple per edge.
    fn edge_payloads(runtime: &Runtime, ids: &[u64]) -> Vec<(u64, u64, u64, super::ScopeEdge)> {
        let snapshot = runtime.read().expect("snapshot");
        let view = snapshot.view::<ScopeGraph>();
        ids.iter()
            .filter_map(|id| {
                view.graph_edge(*id)
                    .map(|edge| (*id, edge.from, edge.to, edge.value))
            })
            .collect()
    }

    /// The evaluator's rows, keyed by the invocation that wrote them.
    fn outcome_rows(runtime: &Runtime) -> Vec<(u64, super::Outcome)> {
        let snapshot = runtime.read().expect("snapshot");
        snapshot
            .view::<super::Evaluation>()
            .outcomes()
            .into_iter()
            .collect()
    }

    #[test]
    fn a_literal_edit_changes_no_scope_graph_entity() {
        let runtime = board(super::grammar::SEED);
        let nodes = scope_nodes(&runtime);
        let edges = scope_edges(&runtime);
        let node_values = node_payloads(&runtime, &nodes);
        let edge_values = edge_payloads(&runtime, &edges);
        let rows = outcome_rows(&runtime);

        edit(
            &runtime,
            "let x = 1 in\nlet f = \\y:Int. x in\nlet x = true in\nf 3",
        );

        assert_eq!(
            scope_nodes(&runtime),
            nodes,
            "a re-spelling must not re-identify the scope graph"
        );
        assert_eq!(scope_edges(&runtime), edges, "nor its edges");
        assert_eq!(
            node_payloads(&runtime, &nodes),
            node_values,
            "the graph's payloads are unchanged too"
        );
        assert_eq!(
            edge_payloads(&runtime, &edges),
            edge_values,
            "the edge payloads are unchanged"
        );
        // `f` ignores its argument, so the program still evaluates to the outer
        // `x`; only the literal's own evaluation changed.
        assert_eq!(result(&runtime), "1");
        assert_eq!(kind(&runtime), "Int");

        // The literal's own row is the same evaluation in the same environment:
        // it keeps its key and changes only the value it reports.
        let edited = outcome_rows(&runtime);
        let literal_before = rows
            .iter()
            .find(|(_, outcome)| outcome.value == Some(super::Value::Int(2)))
            .map(|(id, _)| *id);
        let literal_after = edited
            .iter()
            .find(|(_, outcome)| outcome.value == Some(super::Value::Int(3)))
            .map(|(id, _)| *id);
        assert_eq!(
            literal_before, literal_after,
            "the literal's row keeps its identity"
        );
        assert!(
            edited.len() >= rows.len().saturating_sub(2),
            "only rows whose environment changed may move: {} -> {}",
            rows.len(),
            edited.len()
        );
    }

    #[test]
    fn a_rename_changes_no_scope_graph_entity() {
        let runtime = board("let x = 1 in x");
        let nodes = scope_nodes(&runtime);
        let edges = scope_edges(&runtime);

        edit(&runtime, "let y = 1 in y");

        assert_eq!(
            scope_nodes(&runtime),
            nodes,
            "a rename must not re-identify the scope graph"
        );
        assert_eq!(scope_edges(&runtime), edges);
        assert_eq!(
            targets(&runtime).len(),
            1,
            "the renamed reference still resolves"
        );
        assert!(
            name_errors(&runtime).is_empty(),
            "{:?}",
            name_errors(&runtime)
        );
        let payloads = node_payloads(&runtime, &nodes);
        assert!(
            payloads.iter().any(|(_, node)| matches!(
                node,
                super::ScopeNode::Term { label, .. } if label == "let y"
            )),
            "the binder's payload spells the new name: {payloads:?}"
        );
        assert_eq!(result(&runtime), "1");
    }

    #[test]
    fn a_structural_edit_keeps_the_untouched_subtree() {
        let runtime = board(super::grammar::SEED);
        let nodes = scope_nodes(&runtime);
        let payloads = node_payloads(&runtime, &nodes);
        let label = |wanted: &str| {
            payloads
                .iter()
                .find(|(_, node)| match node {
                    super::ScopeNode::Scope { label } | super::ScopeNode::Term { label, .. } => {
                        label == wanted
                    }
                    super::ScopeNode::Type { .. } => false,
                })
                .map(|(id, _)| *id)
        };
        let module = label("module").expect("the module scope");
        let types: Vec<u64> = payloads
            .iter()
            .filter(|(_, node)| matches!(node, super::ScopeNode::Type { .. }))
            .map(|(id, _)| *id)
            .collect();
        let literals: Vec<u64> = payloads
            .iter()
            .filter(|(_, node)| {
                matches!(
                    node,
                    super::ScopeNode::Term {
                        kind: super::TermKind::Int,
                        ..
                    }
                )
            })
            .map(|(id, _)| *id)
            .collect();

        edit(
            &runtime,
            "let x = 1 in\nlet f = \\y:Int. x in\nlet x = true in\nf 2 3",
        );

        let after = scope_nodes(&runtime);
        assert!(after.contains(&module), "the module scope survives");
        for id in &types {
            assert!(after.contains(id), "the shared type scope survives");
        }
        let kept: Vec<u64> = literals
            .iter()
            .copied()
            .filter(|id| after.contains(id))
            .collect();
        assert_eq!(
            kept.len(),
            2,
            "both literals keep their occurrence; only the App spine re-keys"
        );
        assert!(
            after.len() > nodes.len(),
            "the appended application adds nodes"
        );
    }

    #[test]
    fn the_editor_reply_reports_the_whole_type_and_value() {
        let runtime = board(super::grammar::SEED);
        let reply = super::summary(&runtime);
        assert!(reply.contains("Int"), "{reply}");
        assert!(reply.contains("1 : Int"), "{reply}");
    }

    #[test]
    fn the_pipeline_views_are_all_on_the_board() {
        let runtime = board(super::grammar::SEED);
        let snapshot = runtime.read().expect("snapshot");
        assert!(snapshot.contains::<SourceFile>());
        assert!(snapshot.contains::<Text>());
        assert!(snapshot.contains::<Tokens>());
        assert!(snapshot.contains::<AstTree>());
        assert!(snapshot.contains::<AstMeta>());
        assert!(snapshot.contains::<ScopeGraph>());
        assert!(snapshot.contains::<super::Evaluation>());
        assert!(snapshot.contains::<super::Machine>());
    }

    #[test]
    fn the_pipeline_has_four_stage_components() {
        let runtime = board(super::grammar::SEED);
        let names: Vec<&str> = runtime
            .component_facts()
            .into_iter()
            .map(|component| component.name)
            .collect();
        for stage in ["resolve", "check", "evaluate", "lower"] {
            assert!(
                names
                    .iter()
                    .any(|name| name.ends_with(&format!("::{stage}"))),
                "the {stage} stage must be a component: {names:?}"
            );
        }
        assert!(
            names.iter().all(|name| !name.contains("elaborate")
                && !name.contains("build")
                && !name.contains("bind")),
            "no lowering, build, or bind stage may exist: {names:?}"
        );
    }

    #[test]
    fn the_program_starts_at_main() {
        let runtime = board("1");
        let listing = listing(&runtime);
        assert_eq!(
            listing[..7],
            [
                ".data",
                "heap: .space 65536",
                ".text",
                ".globl main",
                "main:",
                "li $s0, 0",
                "la $t3, heap",
            ]
        );
        assert_eq!(
            listing[listing.len() - 5..],
            [
                "move $a0, $v0",
                "li $v0, 1",
                "syscall",
                "li $v0, 10",
                "syscall"
            ]
        );
        assert_eq!(
            listing.iter().filter(|line| *line == "main:").count(),
            1,
            "one entry point"
        );
    }

    #[test]
    fn one_row_per_instruction() {
        let runtime = board(super::grammar::SEED);
        let snapshot = runtime.read().expect("snapshot");
        let keys: Vec<u32> = snapshot
            .view::<super::Machine>()
            .program()
            .to_vec()
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(keys, (0..keys.len() as u32).collect::<Vec<_>>());
    }

    #[test]
    fn a_literal_lowers_to_one_immediate() {
        let listing = listing(&board("7"));
        assert_eq!(
            listing.iter().filter(|line| *line == "li $v0, 7").count(),
            1
        );
    }

    #[test]
    fn an_application_calls_through_a_closure() {
        let listing = listing(&board("(\\x:Int. x) 7"));
        let at = |needle: &str| {
            listing
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("no `{needle}` in {listing:?}"))
        };
        assert!(
            at("lam_") < at("addiu $sp, $sp, -4"),
            "the callee is built first"
        );
        assert!(
            at("sw $a0, 0($t0)") < at("addiu $sp, $sp, -4"),
            "the frame is pushed at entry"
        );
        assert!(
            at("addiu $sp, $sp, -4") < at("li $v0, 7"),
            "the callee is saved before the argument"
        );
        assert!(
            at("li $v0, 7") < at("jalr $t1"),
            "the argument is ready at the call"
        );
    }

    #[test]
    fn a_let_binds_with_a_frame() {
        let listing = listing(&board("let x = 1 in x"));
        assert!(
            listing.iter().any(|line| line == "sw $s0, 4($t0)"),
            "{listing:?}"
        );
        assert!(
            listing.iter().any(|line| line == "lw $s0, 4($s0)"),
            "the frame is popped"
        );
        assert!(
            listing.iter().any(|line| line.starts_with("walk_")),
            "the reference walks its depth: {listing:?}"
        );
    }

    #[test]
    fn an_if_branches() {
        let listing = listing(&board("if true then 1 else 2"));
        assert!(
            listing
                .iter()
                .any(|line| line.starts_with("beq $v0, $zero, else_")),
            "{listing:?}"
        );
        assert!(listing.iter().any(|line| line == "li $v0, 1"));
        assert!(listing.iter().any(|line| line == "li $v0, 2"));
    }

    #[test]
    fn the_program_prints_what_the_interpreter_says() {
        // The gate: the emitted code and the evaluator are two independent
        // readings of the same tree, and they agree on every program.
        for program in [
            "1",
            "let x = 1 in x",
            "(\\x:Int. x) 7",
            "if false then 1 else 2",
            "let x = 1 in let x = true in if x then 2 else 3",
            "let f = (\\y:Int. y) in f 5",
            super::grammar::SEED,
        ] {
            let runtime = board(program);
            let expected: i32 = result(&runtime)
                .parse()
                .unwrap_or_else(|_| panic!("`{program}` did not evaluate to a number"));
            assert_eq!(
                printed(&runtime),
                vec![expected],
                "`{program}` printed something else"
            );
        }
    }

    #[test]
    fn a_rename_changes_no_row() {
        let runtime = board("let x = 1 in x");
        let before = listing(&runtime);
        edit(&runtime, "let y = 1 in y");
        assert_eq!(listing(&runtime), before);
    }

    #[test]
    fn a_literal_edit_changes_one_row() {
        let runtime = board("let x = 1 in x");
        let before = listing(&runtime);
        edit(&runtime, "let x = 2 in x");
        let after = listing(&runtime);
        assert_eq!(before.len(), after.len());
        let changed: Vec<(usize, &String, &String)> = before
            .iter()
            .zip(&after)
            .enumerate()
            .filter(|(_, (left, right))| left != right)
            .map(|(index, (left, right))| (index, left, right))
            .collect();
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(
            (changed[0].1.as_str(), changed[0].2.as_str()),
            ("li $v0, 1", "li $v0, 2")
        );
    }

    #[test]
    fn an_annotation_edit_changes_no_row() {
        let runtime = board("\\y:Int. y");
        let before = listing(&runtime);
        edit(&runtime, "\\y:Bool. y");
        assert_eq!(
            listing(&runtime),
            before,
            "annotations are not lowered, so every row keeps its value"
        );
    }
}

#[cfg(test)]
mod locality {
    use super::tests::{board, edit};
    use super::*;

    fn root(runtime: &Runtime) -> u64 {
        runtime
            .read()
            .expect("snapshot")
            .view::<syntax::ast::Syntax>()
            .root()
            .map(|document| document.raw())
            .expect("the seed has a typed root")
    }

    fn changed(runtime: &Runtime) -> u64 {
        runtime
            .view_facts()
            .iter()
            .map(|facts| u64::from(facts.changed_units))
            .sum()
    }

    fn counters(runtime: &Runtime) -> String {
        let snapshot = runtime.read().expect("snapshot");
        let stats = snapshot.view::<irrigation_syntax::PipelineStats>();
        let mut rows: Vec<String> = stats
            .counters()
            .into_iter()
            .filter(|(id, _)| {
                matches!(
                    id,
                    irrigation_syntax::StatId::BytesReparsed
                        | irrigation_syntax::StatId::TokensReparsed
                        | irrigation_syntax::StatId::NodesBuilt
                        | irrigation_syntax::StatId::NodesAdded
                        | irrigation_syntax::StatId::NodesRemoved
                        | irrigation_syntax::StatId::AdoptedSuffix
                        | irrigation_syntax::StatId::Escalations
                )
            })
            .map(|(id, value)| format!("{id:?}={value}"))
            .collect();
        rows.sort();
        rows.join(" ")
    }

    fn breakdown(runtime: &Runtime) -> String {
        let mut rows: Vec<String> = runtime
            .view_facts()
            .into_iter()
            .filter(|facts| facts.changed_units > 0)
            .map(|facts| {
                let fields: Vec<String> = facts
                    .fields
                    .iter()
                    .filter(|field| field.changed_units > 0)
                    .map(|field| format!("{}:{}", field.name, field.changed_units))
                    .collect();
                format!(
                    "{}={}({})",
                    facts.name.rsplit("::").next().unwrap_or(facts.name),
                    facts.changed_units,
                    fields.join(",")
                )
            })
            .collect();
        rows.sort();
        rows.join(" ")
    }

    fn view_changed(runtime: &Runtime, suffix: &str) -> u32 {
        runtime
            .view_facts()
            .into_iter()
            .find(|facts| facts.name.ends_with(suffix))
            .map_or(0, |facts| facts.changed_units)
    }

    fn let_spine(runtime: &Runtime) -> Vec<u64> {
        let snapshot = runtime.read().expect("snapshot");
        let tree = snapshot.view::<syntax::ast::Syntax>();
        let mut term = tree
            .root()
            .and_then(|document| document.term())
            .expect("the seed has a term");
        let mut ids = vec![term.raw()];
        while let Some(body) = term.body() {
            if body.kind() != Some(syntax::TermKind::Let) {
                break;
            }
            ids.push(body.raw());
            term = body;
        }
        ids
    }

    #[test]
    fn typing_the_seed_one_character_at_a_time_stays_local() {
        let runtime = board("");
        let mut text = String::new();
        let mut worst = 0u64;
        let mut worst_at = String::new();
        for ch in grammar::SEED.chars() {
            text.push(ch);
            edit(&runtime, &text);
            let total = changed(&runtime);
            if total > worst {
                worst = total;
                worst_at = format!("{text:?}");
            }
            if std::env::var_os("TYPING_REPORT").is_some() {
                eprintln!(
                    "{} {:?} -> {total} [{}]",
                    text.len(),
                    ch,
                    breakdown(&runtime)
                );
            }
        }
        assert!(
            worst <= 400,
            "one keystroke rebuilt too much: {worst} at {worst_at}"
        );
    }

    #[test]
    fn a_large_program_keeps_an_append_local() {
        let mut text = String::new();
        for index in 0..24 {
            text.push_str(&format!("let x{index} = {index} in\n"));
        }
        text.push('0');
        let runtime = board(&text);
        let seed_root = root(&runtime);
        edit(&runtime, &format!("{text} 3"));
        assert_eq!(root(&runtime), seed_root, "the root is stable");
        assert!(
            changed(&runtime) <= 600,
            "a tail append is local even in a large program: {} [{}]",
            changed(&runtime),
            breakdown(&runtime)
        );
        assert!(
            view_changed(&runtime, "ScopeGraph") <= 160,
            "the graph should follow the edited path, not the document: {}",
            view_changed(&runtime, "ScopeGraph")
        );
    }

    #[test]
    fn a_structural_append_keeps_the_let_spine() {
        let runtime = board(grammar::SEED);
        let before = let_spine(&runtime);
        edit(&runtime, &format!("{} 3", grammar::SEED));
        assert_eq!(
            let_spine(&runtime),
            before,
            "the Let occurrences are named by their `let`, not by their growing extent"
        );
    }

    #[test]
    fn an_invalid_character_does_not_run_the_stages() {
        let runtime = board(grammar::SEED);
        edit(
            &runtime,
            &grammar::SEED.replacen("let x = 1 in", "let x = 1# in", 1),
        );
        // The stages may be re-certified because their producer wrote another
        // View in the same revision; what matters is that their outputs did not
        // move. The typed tree is kept, so every downstream unit is a cutoff.
        assert_eq!(view_changed(&runtime, "Syntax"), 0);
        assert_eq!(view_changed(&runtime, "ScopeGraph"), 0);
        assert_eq!(view_changed(&runtime, "Evaluation"), 0);
        assert_eq!(view_changed(&runtime, "Machine"), 0);
        assert!(
            changed(&runtime) <= 120,
            "invalid input should be a parser edit, not a pipeline rebuild: {changed} [{breakdown}]",
            changed = changed(&runtime),
            breakdown = breakdown(&runtime)
        );
    }

    #[test]
    fn reverting_an_error_does_not_run_the_stages() {
        let runtime = board(grammar::SEED);
        edit(
            &runtime,
            &grammar::SEED.replacen("let x = 1 in", "let x = 1# in", 1),
        );
        edit(&runtime, grammar::SEED);
        assert!(
            changed(&runtime) <= 90,
            "the revert is a parser/token edit: {changed} [{breakdown}]",
            changed = changed(&runtime),
            breakdown = breakdown(&runtime)
        );
    }

    #[test]
    fn measure_structural_edits() {
        let runtime = board(grammar::SEED);
        let seed_root = root(&runtime);
        eprintln!("seed changed={} root={seed_root}", changed(&runtime));

        edit(&runtime, &format!("{} 3", grammar::SEED));
        eprintln!(
            "append changed={} root={} [{}]",
            changed(&runtime),
            root(&runtime),
            breakdown(&runtime)
        );
        assert_eq!(root(&runtime), seed_root, "the root is stable");

        edit(&runtime, grammar::SEED);
        eprintln!(
            "reset changed={} root={} [{}]",
            changed(&runtime),
            root(&runtime),
            breakdown(&runtime)
        );

        edit(
            &runtime,
            &grammar::SEED.replacen("let x = 1 in", "let x = 1# in", 1),
        );
        eprintln!(
            "invalid changed={} root={} [{}] {}",
            changed(&runtime),
            root(&runtime),
            breakdown(&runtime),
            counters(&runtime)
        );
        assert_eq!(
            root(&runtime),
            seed_root,
            "the root is stable across an error"
        );

        edit(&runtime, grammar::SEED);
        eprintln!(
            "revert changed={} root={} [{}] {}",
            changed(&runtime),
            root(&runtime),
            breakdown(&runtime),
            counters(&runtime)
        );
        assert_eq!(
            root(&runtime),
            seed_root,
            "the root is stable after a revert"
        );
    }
}
