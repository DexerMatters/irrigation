//! A simply typed lambda calculus, resolved with scope graphs, on the
//! Observatory board.
//!
//! ```sh
//! cargo run --example stlc_scope_graphs -p irrigation --features observatory
//! ```
//!
//! The pipeline is the same shape as `observatory_json`: a source document, the
//! framework's text/lexer/parser components, then ordinary components over the
//! published Views.
//!
//! The analysis Views:
//!
//! ```text
//! elaborate     -> Terms
//! resolve_name  -> ScopeGraph: binding nodes, targets, name errors
//! check_term    -> ScopeGraph.types / type_errors, one entry per term
//! evaluate      -> Evaluation, from per-term evaluator invocations
//! ```
//!
//! `Terms` and `ScopeGraph` are singleton Views. Their handles are zero-sized
//! and the read accessors are ambient, so no component takes `&Terms` or
//! `&ScopeGraph`; only semantic arguments (`term`, evaluator environment/depth)
//! appear in signatures. The root returns the View handles for registration.
//!
//! * `elaborate` lowers the parser tree into a term table, splitting shape,
//!   names, annotations, and literals into separate maps.
//! * `resolve_name` builds the binding graph in one structural stage, then
//!   resolves each term in its own invocation. It reads only root/nodes/names,
//!   so a type or literal edit cannot invalidate it.
//! * `check_term(term)` derives a variable's type from its binding node and
//!   writes `types[term]`/`type_errors[term]`. There is no `TypeEnv`, no type
//!   node, and no shared typing table.
//! * `eval_term(term, env, depth)` is the per-term evaluator; `evaluate` writes
//!   the one `Evaluation` summary.
//!
//! Only the source text is editable. The lexer and parser are components on the
//! board, and the raw AST is what the parser published.

mod eval;
mod grammar;
mod graph;
mod report;
mod terms;
mod types;

use std::sync::{Arc, Mutex};

use irrigation::{Runtime, incremental_data};
use irrigation_observe::{Observatory, ObserveLevel, observed_runtime};
use irrigation_syntax::{
    AstMeta, LexerComponent, ParserComponent, SourceFile, TextComponent, TextDocument,
};

/// The elaboration stage's View: the semantic term table.
///
/// Shape, names, annotations, and literal payloads are separate fields so an
/// edit to one does not wake a stage that reads none of it.
#[incremental_data]
pub struct Terms {
    /// Term shapes and child identities.
    pub nodes: Map<u64, terms::Term>,
    /// Binder and variable spellings.
    pub names: Map<u64, String>,
    /// Lambda parameter annotations.
    pub annotations: Map<u64, String>,
    /// Integer and boolean payloads.
    pub literals: Map<u64, terms::Literal>,
    /// The program's term identity.
    pub root: Option<u64>,
    /// Nodes the lowering could not understand.
    pub diagnostics: List<report::Line>,
}

/// The scope graph plus the term-keyed typing maps.
///
/// `resolve_name` owns the graph, targets, and name diagnostics. `check_term`
/// owns one `types[term]` and one `type_errors[term]` entry per term; the maps
/// are per-key entity ports, so per-term invocations write disjoint units.
#[incremental_data]
pub struct ScopeGraph {
    /// Scopes, terms, and bindings; `P`, `D`, `R`, and `child` edges.
    pub graph: irrigation::Graph<graph::ScopeNode, graph::ScopeEdge>,
    /// Reference terms that resolved unambiguously to a binding.
    pub targets: Map<u64, u64>,
    /// Unresolved, ambiguous, and shadowing diagnostics.
    pub name_errors: Map<u64, report::Line>,
    /// Each checked term's type.
    pub types: Map<u64, types::Type>,
    /// Ill-typed terms and failed annotations.
    pub type_errors: Map<u64, report::Line>,
}

/// The evaluator stage's View.
#[incremental_data]
pub struct Evaluation {
    /// The value's text; empty means no value.
    pub result: report::Line,
    /// `Int`, `Bool`, `function`, or `stuck`.
    pub kind: report::Line,
    /// Beta and `let` steps performed.
    pub steps: u64,
    /// Why evaluation did not produce a value, if it did not.
    pub diagnostics: List<report::Line>,
}

/// The component chain: resolve, check, evaluate.
mod pipeline {
    use super::{Evaluation, ScopeGraph, Terms};
    use super::{eval, graph, report, terms, types};
    use irrigation::component;

    /// Resolves names: lowers the parser tree into Terms, builds the scope/
    /// term graph, publishes bindings/parents/children/references, and writes
    /// the targets/name diagnostics. This is the only resolver component.
    #[component(callable)]
    pub fn resolve_name() -> (Terms, ScopeGraph) {
        let lowered = terms::lower_now();
        publish_terms(&lowered);
        let draft = graph::build(
            lowered.root,
            |id| lowered.nodes.get(&id).copied(),
            |id| lowered.names.get(&id).cloned(),
        );
        graph::publish(ScopeGraph.graph(), &draft, "name_edge");
        let analysis = graph::resolve(&draft);
        publish_analysis(analysis);
        (Terms, ScopeGraph)
    }

    /// Checks every term, writing type maps, canonical type scopes, and `T`
    /// edges into the same scope graph.
    #[component(callable)]
    pub fn check() {
        types::check_all();
    }

    /// Evaluates the root term datum, writing the one Evaluation summary.
    #[component(callable)]
    pub fn evaluate() -> Evaluation {
        eval::evaluate_now()
    }

    /// The root composes the three stages.
    #[component(root)]
    pub fn stlc() -> (Terms, ScopeGraph, Evaluation) {
        let (terms, scope) = resolve_name();
        check();
        let evaluation = evaluate();
        (terms, scope, evaluation)
    }

    fn publish_analysis(analysis: graph::NameAnalysis) {
        for (term, target) in analysis.targets {
            ScopeGraph.define_targets(term, target);
        }
        for (term, message) in analysis.errors {
            ScopeGraph.define_name_errors(term, report::Line(message));
        }
    }

    fn publish_terms(lowered: &terms::TermTable) {
        for (id, node) in &lowered.nodes {
            Terms.define_nodes(*id, *node);
        }
        for (id, name) in &lowered.names {
            Terms.define_names(*id, name.clone());
        }
        for (id, annotation) in &lowered.annotations {
            Terms.define_annotations(*id, annotation.clone());
        }
        for (id, literal) in &lowered.literals {
            Terms.define_literals(*id, *literal);
        }
        Terms.define_root(lowered.root);
        Terms.define_diagnostics(
            lowered
                .diagnostics
                .iter()
                .cloned()
                .map(report::Line)
                .collect::<Vec<_>>(),
        );
    }
}

fn main() -> std::io::Result<()> {
    let (runtime, recorder) = observed_runtime(ObserveLevel::Nodes);
    let grammar = grammar::grammar();
    runtime
        .install_general_component(TextComponent)
        .expect("text component");
    runtime
        .install_general_component(LexerComponent::new(grammar.lexer().clone()))
        .expect("lexer component");
    runtime
        .install_general_component(ParserComponent::new(grammar))
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
/// The whole term's type is its `types[root]` entry.
fn summary(runtime: &Runtime) -> String {
    use std::fmt::Write as _;

    let Ok(snapshot) = runtime.read() else {
        return String::from("the runtime is not available");
    };
    let terms = snapshot.view::<Terms>();
    let graph = snapshot.view::<ScopeGraph>();
    let evaluation = snapshot.view::<Evaluation>();
    let whole = terms.root().and_then(|root| {
        graph
            .types()
            .into_iter()
            .find_map(|(id, ty)| (id == root).then(|| ty.render()))
    });
    let name_errors = graph.name_errors();
    let type_errors = graph.type_errors();
    let parse_errors = snapshot.view::<AstMeta>().diagnostics().len();
    let lower_errors = terms.diagnostics().len();
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
    if lower_errors > 0 {
        let _ = write!(reply, " · {lower_errors} lowering error(s)");
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
    use super::{grammar, graph, pipeline, types};
    use irrigation::Runtime;
    use irrigation_syntax::{
        AstMeta, AstTree, LexerComponent, ParserComponent, SourceFile, Text, TextComponent,
        TextDocument, Tokens,
    };

    fn board(text: &str) -> Runtime {
        let runtime = Runtime::new();
        let grammar = grammar::grammar();
        runtime
            .install_general_component(TextComponent)
            .expect("text component");
        runtime
            .install_general_component(LexerComponent::new(grammar.lexer().clone()))
            .expect("lexer component");
        runtime
            .install_general_component(ParserComponent::new(grammar))
            .expect("parser component");
        runtime.install(pipeline::Stlc).expect("install");
        runtime
            .edit(|edit| {
                edit.view::<SourceFile>()
                    .set_document(TextDocument::new(text))
            })
            .expect("seed");
        let _ = runtime.read_outputs::<pipeline::Stlc>().expect("demand");
        runtime
    }

    fn edit(runtime: &Runtime, text: &str) {
        let current = {
            let snapshot = runtime.read().expect("snapshot");
            snapshot.view::<SourceFile>().document()
        };
        let next = current.replace_with(text).unwrap_or(current);
        runtime
            .edit(|edit| edit.view::<SourceFile>().set_document(next))
            .expect("edit");
        let _ = runtime.read_outputs::<pipeline::Stlc>().expect("demand");
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

    fn kind(runtime: &Runtime) -> String {
        runtime
            .read()
            .expect("snapshot")
            .view::<super::Evaluation>()
            .kind()
            .0
    }

    fn targets(runtime: &Runtime) -> Vec<(u64, u64)> {
        runtime
            .read()
            .expect("snapshot")
            .view::<super::ScopeGraph>()
            .targets()
    }

    fn name_errors(runtime: &Runtime) -> Vec<String> {
        runtime
            .read()
            .expect("snapshot")
            .view::<super::ScopeGraph>()
            .name_errors()
            .into_iter()
            .map(|(_, line)| line.0)
            .collect()
    }

    fn type_errors(runtime: &Runtime) -> Vec<String> {
        runtime
            .read()
            .expect("snapshot")
            .view::<super::ScopeGraph>()
            .type_errors()
            .into_iter()
            .map(|(_, line)| line.0)
            .collect()
    }

    fn type_values(runtime: &Runtime) -> Vec<types::Type> {
        runtime
            .read()
            .expect("snapshot")
            .view::<super::ScopeGraph>()
            .types()
            .into_iter()
            .map(|(_, ty)| ty)
            .collect()
    }

    fn whole_type(runtime: &Runtime) -> Option<String> {
        let snapshot = runtime.read().expect("snapshot");
        let root = snapshot.view::<super::Terms>().root()?;
        snapshot
            .view::<super::ScopeGraph>()
            .types()
            .into_iter()
            .find_map(|(term, ty)| (term == root).then(|| ty.render()))
    }

    #[test]
    fn the_seed_evaluates_to_one() {
        let runtime = board(grammar::SEED);
        assert_eq!(whole_type(&runtime).as_deref(), Some("Int"));
        assert_eq!(result(&runtime), "1");
        assert_eq!(kind(&runtime), "Int");
    }

    #[test]
    fn a_later_declaration_does_not_capture_an_earlier_body() {
        let runtime = board(grammar::SEED);
        assert_eq!(result(&runtime), "1");
    }

    #[test]
    fn shadowing_is_reported_with_its_distance() {
        let runtime = board(grammar::SEED);
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
        let runtime = board(grammar::SEED);
        let snapshot = runtime.read().expect("snapshot");
        let graph = snapshot.view::<super::ScopeGraph>();
        assert!(
            graph.graph_nodes().into_iter().any(|id| matches!(
                graph.graph_node(id),
                Some(graph::ScopeNode::Term {
                    binding: Some(_),
                    ..
                })
            )),
            "a binder must be a term node with a binding payload"
        );
        assert!(graph.graph_edges().into_iter().any(|id| matches!(
            graph.graph_edge(id).map(|edge| edge.value),
            Some(graph::ScopeEdge::Declares { .. })
        )));
        assert!(graph.graph_edges().into_iter().any(|id| matches!(
            graph.graph_edge(id).map(|edge| edge.value),
            Some(graph::ScopeEdge::TypeOf { .. })
        )));
        assert!(
            !graph.types().is_empty(),
            "the checker must write term type entries"
        );
        assert_eq!(graph.type_errors().len(), 0);
    }

    #[test]
    fn equal_types_share_one_type_scope_and_keep_term_entries() {
        let runtime = board(grammar::SEED);
        let values = type_values(&runtime);
        assert!(
            values.iter().filter(|ty| **ty == types::Type::Int).count() >= 2,
            "every Int-typed term keeps its own map entry: {values:?}"
        );
        let snapshot = runtime.read().expect("snapshot");
        let graph = snapshot.view::<super::ScopeGraph>();
        let mut int_scopes = Vec::new();
        for id in graph.graph_nodes() {
            if let Some(graph::ScopeNode::Type { label }) = graph.graph_node(id) {
                if label == "Int" {
                    int_scopes.push(id);
                }
            }
        }
        assert_eq!(
            int_scopes.len(),
            1,
            "equal types must share one canonical type scope"
        );
    }

    #[test]
    fn a_type_annotation_edit_reruns_the_checker_and_keeps_targets_equal() {
        let runtime = board(grammar::SEED);
        let before = runtime.read().expect("snapshot");
        let evaluation_before = before.version::<super::Evaluation>();
        let targets_before = targets(&runtime);
        let names_before = executed(&runtime, "resolve_name");
        let typing_before = executed(&runtime, "check");
        let evaluation_stage_before = executed(&runtime, "evaluate");

        edit(
            &runtime,
            "let x = 1 in\nlet f = \\y:Bool. x in\nlet x = true in\nf 2",
        );
        let after = runtime.read().expect("snapshot");
        assert_eq!(targets(&runtime), targets_before);
        assert_eq!(after.version::<super::Evaluation>(), evaluation_before);
        assert!(after.version::<super::ScopeGraph>() > before.version::<super::ScopeGraph>());
        assert!(executed(&runtime, "check") > typing_before);
        assert_eq!(executed(&runtime, "resolve_name"), names_before);
        assert_eq!(executed(&runtime, "evaluate"), evaluation_stage_before);
    }

    #[test]
    fn a_literal_edit_reruns_the_evaluator_alone() {
        let runtime = board("let x = 1 in x");
        let before = runtime.read().expect("snapshot");
        let graph_before = before.version::<super::ScopeGraph>();
        let names_before = executed(&runtime, "resolve_name");
        let typing_before = executed(&runtime, "check");
        let evaluation_before = executed(&runtime, "evaluate");

        edit(&runtime, "let x = 2 in x");

        let after = runtime.read().expect("snapshot");
        assert_eq!(after.version::<super::ScopeGraph>(), graph_before);
        assert_eq!(result(&runtime), "2");
        assert!(executed(&runtime, "evaluate") > evaluation_before);
        assert_eq!(executed(&runtime, "resolve_name"), names_before);
        assert_eq!(executed(&runtime, "check"), typing_before);
    }

    #[test]
    fn a_consistent_rename_stops_at_the_resolver() {
        let runtime = board("let x = 1 in x");
        let before = runtime.read().expect("snapshot");
        let targets_before = targets(&runtime);
        let names_before = executed(&runtime, "resolve_name");
        let typing_before = executed(&runtime, "check");
        let evaluation_before = executed(&runtime, "evaluate");

        edit(&runtime, "let y = 1 in y");

        let after = runtime.read().expect("snapshot");
        assert!(after.version::<super::ScopeGraph>() > before.version::<super::ScopeGraph>());
        assert_eq!(targets(&runtime), targets_before);
        assert_eq!(
            after.version::<super::Evaluation>(),
            before.version::<super::Evaluation>()
        );
        assert!(executed(&runtime, "resolve_name") > names_before);
        assert_eq!(executed(&runtime, "check"), typing_before);
        assert_eq!(executed(&runtime, "evaluate"), evaluation_before);
    }

    #[test]
    fn a_structural_edit_retracts_stale_type_entries() {
        let runtime = board("let x = true in x");
        assert!(type_values(&runtime).contains(&types::Type::Bool));
        edit(&runtime, "let x = 1 in x");
        assert!(!type_values(&runtime).contains(&types::Type::Bool));
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
        assert!(snapshot.view::<super::Terms>().root().is_some());
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
    fn the_pipeline_views_are_all_on_the_board() {
        let runtime = board(grammar::SEED);
        let snapshot = runtime.read().expect("snapshot");
        assert!(snapshot.contains::<SourceFile>());
        assert!(snapshot.contains::<Text>());
        assert!(snapshot.contains::<Tokens>());
        assert!(snapshot.contains::<AstTree>());
        assert!(snapshot.contains::<AstMeta>());
        assert!(snapshot.contains::<super::Terms>());
        assert!(snapshot.contains::<super::ScopeGraph>());
        assert!(snapshot.contains::<super::Evaluation>());
    }
}
