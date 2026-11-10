//! Workload matrix: no-op transactions, map lookups, tree leaf edits, and
//! cross-family workloads (map to keyed list, source list to map, join target,
//! graph node map).
//!
//! Run with `cargo run --release --example workloads -p irrigation`.
//!
//! Every row reports the executions caused by the transaction plus the
//! observer read, which is the plan's "no unnecessary body execution" measure.

use std::time::Instant;

use irrigation::{Runtime, irrigation, tree_view, view};

#[view(source)]
pub struct Table {
    entries: Map<u32, u64>,
}

#[irrigation]
mod map_workload {
    use super::Table;

    #[component]
    pub fn lookup(table: &Table) -> Option<u64> {
        let entries = table.entries();
        entries.get(&1)
    }
}

#[tree_view(root = Document, view = Syntax)]
pub mod syntax {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Document {
        Module { items: Vec<Expr> },
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Expr {
        Literal(Literal),
        Name(String),
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Literal {
        Int(i64),
    }
}

#[irrigation]
mod tree_workload {
    use super::syntax;

    #[component]
    pub fn count(expr: syntax::ExprRef) -> i64 {
        let kind = expr.kind();
        if kind == Some(syntax::ExprKind::Literal) {
            let literal = expr.literal();
            match literal {
                Some(literal) => literal.int().unwrap_or(0),
                None => 0,
            }
        } else {
            1
        }
    }

    #[component]
    pub fn total(syntax: &syntax::Syntax) -> i64 {
        let root = syntax.root();
        match root {
            Some(document) => {
                let items = document.items();
                items.iter().map(|item| count(item.clone())).sum::<i64>()
            }
            None => 0,
        }
    }
}

fn document(size: usize) -> syntax::Document {
    syntax::Document::Module {
        items: (0..size)
            .map(|index| syntax::Expr::Literal(syntax::Literal::Int(index as i64)))
            .collect(),
    }
}

// --- Cross-family workloads ------------------------------------------------

#[view(source)]
pub struct Elements {
    values: List<u32>,
}

#[view(derived)]
pub struct MappedList(List<u64>);

#[view(derived)]
pub struct MappedMap(Map<u32, u64>);

#[view(source)]
pub struct LeftNumbers {
    entries: Map<u32, u64>,
}

#[view(source)]
pub struct RightNumbers {
    entries: Map<u32, u64>,
}

#[view(derived)]
pub struct JoinedNumbers(Map<u32, u64>);

#[view(source)]
pub struct GraphMode {
    size: u64,
    bump: bool,
}

#[view(derived)]
pub struct WorkGraph(Graph<u64, u64>);

#[view(derived)]
pub struct WorkGraphMap(Map<u64, u64>);

#[irrigation]
mod cross_workloads {
    use super::{Elements, MappedList, MappedMap, Table};

    #[component]
    pub fn map_to_list(table: &Table) -> Option<u64> {
        irrigation::ensure_view(<MappedList as irrigation::ViewSchema>::DESCRIPTOR);
        let target = MappedList.field0();
        table
            .entries()
            .to_list(&target, |key, value| Some((u64::from(*key), *value)));
        target.get_key(1)
    }

    #[component]
    pub fn list_to_map(elements: &Elements) -> Option<u64> {
        irrigation::ensure_view(<MappedMap as irrigation::ViewSchema>::DESCRIPTOR);
        let target = MappedMap.field0();
        elements
            .values()
            .to_map(&target, |id, value| Some((id as u32, u64::from(*value))));
        target.get(&1)
    }
}

struct NumberJoin;

impl irrigation::JoinSpec for NumberJoin {
    type Key = u32;
    type Left = u64;
    type Right = u64;
    type Joined = u64;

    fn left() -> (irrigation::ViewId, irrigation::FieldId) {
        (
            <LeftNumbers as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
        )
    }

    fn right() -> (irrigation::ViewId, irrigation::FieldId) {
        (
            <RightNumbers as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
        )
    }

    fn combine(left: &u64, right: &u64) -> u64 {
        left * right
    }

    fn target() -> Option<(irrigation::ViewId, irrigation::FieldId)> {
        Some((
            <JoinedNumbers as irrigation::ViewSchema>::DESCRIPTOR.id,
            irrigation::FieldId(0),
        ))
    }
}

fn define_work_graph(nodes: Vec<(u64, u64)>, edges: Vec<(u64, u64, u64, u64)>) {
    WorkGraph.define_graph(&nodes, &edges);
}

#[irrigation]
mod graph_workload {
    use super::{GraphMode, WorkGraph, WorkGraphMap, define_work_graph, graph_edges, graph_nodes};

    #[component]
    pub fn root(mode: &GraphMode) -> Option<u64> {
        irrigation::ensure_view(<WorkGraph as irrigation::ViewSchema>::DESCRIPTOR);
        irrigation::ensure_view(<WorkGraphMap as irrigation::ViewSchema>::DESCRIPTOR);
        define_work_graph(
            graph_nodes(mode.size(), mode.bump()),
            graph_edges(mode.size()),
        );
        let target = WorkGraphMap.field0();
        WorkGraph.field0().node_map(&target);
        target.get(&1)
    }
}

fn graph_nodes(size: u64, bump: bool) -> Vec<(u64, u64)> {
    (0..size)
        .map(|id| (id, if id == 1 && bump { 11 } else { id }))
        .collect()
}

fn graph_edges(size: u64) -> Vec<(u64, u64, u64, u64)> {
    (0..size.saturating_sub(1))
        .map(|id| (id, id, id + 1, id * 10))
        .collect()
}

fn seed_table(runtime: &Runtime, size: usize) {
    runtime
        .edit(|edit| {
            let mut table = edit.view::<Table>();
            let mut entries = table.entries();
            for index in 0..size {
                entries.insert(index as u32, index as u64);
            }
        })
        .expect("seed table");
}

fn cross_family_rows() {
    for size in [100usize, 1_000] {
        // map -> keyed list, one member per key.
        let runtime = Runtime::new();
        runtime
            .install(cross_workloads::MapToList)
            .expect("install");
        seed_table(&runtime, size);
        let _ = runtime
            .read_outputs::<cross_workloads::MapToList>()
            .expect("warm");
        let before = runtime.stats();
        let started = Instant::now();
        runtime
            .edit(|edit| {
                let mut table = edit.view::<Table>();
                let mut entries = table.entries();
                entries.insert(1, 11);
            })
            .expect("edit");
        let _ = runtime
            .read_outputs::<cross_workloads::MapToList>()
            .expect("read");
        let elapsed = started.elapsed();
        let after = runtime.stats();
        println!(
            "map-to-list\t{size}\t{}\t{}\t{}",
            after.executions - before.executions,
            after.revisions - before.revisions,
            elapsed.as_micros()
        );

        // source list -> map, one entry per element identity.
        let runtime = Runtime::new();
        runtime
            .install(cross_workloads::ListToMap)
            .expect("install");
        runtime
            .edit(|edit| {
                let mut elements = edit.view::<Elements>();
                let mut values = elements.values();
                for index in 0..size {
                    values.push(index as u32);
                }
            })
            .expect("seed list");
        let _ = runtime
            .read_outputs::<cross_workloads::ListToMap>()
            .expect("warm");
        let before = runtime.stats();
        let started = Instant::now();
        runtime
            .edit(|edit| {
                let mut elements = edit.view::<Elements>();
                let mut values = elements.values();
                values.set(1, 11);
            })
            .expect("edit");
        let _ = runtime
            .read_outputs::<cross_workloads::ListToMap>()
            .expect("read");
        let elapsed = started.elapsed();
        let after = runtime.stats();
        println!(
            "list-to-map\t{size}\t{}\t{}\t{}",
            after.executions - before.executions,
            after.revisions - before.revisions,
            elapsed.as_micros()
        );

        // Equi-join publishing a per-key target View.
        let runtime = Runtime::new();
        runtime.register_view(<LeftNumbers as irrigation::ViewSchema>::DESCRIPTOR);
        runtime.register_view(<RightNumbers as irrigation::ViewSchema>::DESCRIPTOR);
        runtime.register_view(<JoinedNumbers as irrigation::ViewSchema>::DESCRIPTOR);
        runtime
            .install_general(irrigation::EquiJoin::<NumberJoin>::new())
            .expect("install join");
        runtime
            .edit(|edit| {
                {
                    let mut left = edit.view::<LeftNumbers>();
                    let mut entries = left.entries();
                    for index in 0..size {
                        entries.insert(index as u32, index as u64);
                    }
                }
                let mut right = edit.view::<RightNumbers>();
                let mut entries = right.entries();
                for index in 0..size {
                    entries.insert(index as u32, 2);
                }
            })
            .expect("seed join");
        let _ = runtime
            .read_outputs_general::<irrigation::EquiJoin<NumberJoin>>()
            .expect("warm join");
        let before = runtime.stats();
        let started = Instant::now();
        runtime
            .edit(|edit| {
                let mut left = edit.view::<LeftNumbers>();
                let mut entries = left.entries();
                entries.insert(1, 11);
            })
            .expect("edit");
        let _ = runtime
            .read_outputs_general::<irrigation::EquiJoin<NumberJoin>>()
            .expect("read join");
        let elapsed = started.elapsed();
        let after = runtime.stats();
        println!(
            "join-view\t{size}\t{}\t{}\t{}",
            after.executions - before.executions,
            after.revisions - before.revisions,
            elapsed.as_micros()
        );

        // Derived graph -> node map.
        let runtime = Runtime::new();
        runtime
            .install(graph_workload::Root)
            .expect("install graph");
        runtime
            .edit(|edit| {
                let mut mode = edit.view::<GraphMode>();
                mode.set_size(size as u64);
            })
            .expect("seed graph");
        let _ = runtime
            .read_outputs::<graph_workload::Root>()
            .expect("warm graph");
        let before = runtime.stats();
        let started = Instant::now();
        runtime
            .edit(|edit| edit.view::<GraphMode>().set_bump(true))
            .expect("edit");
        let _ = runtime
            .read_outputs::<graph_workload::Root>()
            .expect("read graph");
        let elapsed = started.elapsed();
        let after = runtime.stats();
        println!(
            "graph-node-map\t{size}\t{}\t{}\t{}",
            after.executions - before.executions,
            after.revisions - before.revisions,
            elapsed.as_micros()
        );

        // Tree -> flatten map, one entry per node.
        let runtime = Runtime::new();
        runtime
            .install(flatten_workload::Root)
            .expect("install tree flatten");
        runtime
            .edit(|edit| {
                let mut tree = edit.view::<syntax::Syntax>();
                tree.set_root(&document(size));
            })
            .expect("seed tree");
        let _ = runtime
            .read_outputs::<flatten_workload::Root>()
            .expect("warm tree");
        let before = runtime.stats();
        let started = Instant::now();
        runtime
            .edit(|edit| {
                let mut tree = edit.view::<syntax::Syntax>();
                let root = tree.root();
                let items = root.items();
                let leaf = items[size / 2].clone();
                tree.replace_expr(leaf, &syntax::Expr::Name(String::from("-1")));
            })
            .expect("edit tree");
        let _ = runtime
            .read_outputs::<flatten_workload::Root>()
            .expect("read tree");
        let elapsed = started.elapsed();
        let after = runtime.stats();
        println!(
            "tree-flatten\t{size}\t{}\t{}\t{}",
            after.executions - before.executions,
            after.revisions - before.revisions,
            elapsed.as_micros()
        );
    }
}

#[view(derived)]
pub struct SyntaxFlatten(Map<u64, u32>);

#[expect(
    clippy::unnecessary_wraps,
    reason = "`flatten_map` takes `fn(&Node) -> Option<u32>`; this grammar maps every node"
)]
fn flatten_tag(node: &syntax::SyntaxNode) -> Option<u32> {
    match node {
        syntax::SyntaxNode::Document(_) => Some(0),
        syntax::SyntaxNode::Expr(_) => Some(1),
        syntax::SyntaxNode::Literal(_) => Some(2),
    }
}

#[irrigation]
mod flatten_workload {
    use super::{SyntaxFlatten, flatten_tag, syntax};

    #[component]
    pub fn root(syntax: &syntax::Syntax) -> u32 {
        irrigation::ensure_view(<SyntaxFlatten as irrigation::ViewSchema>::DESCRIPTOR);
        let target = SyntaxFlatten.field0();
        match syntax.root() {
            Some(document) => {
                document.flatten_map(&target, flatten_tag);
                0
            }
            None => 0,
        }
    }
}

fn main() {
    println!("workload\tunits\texecutions\trevisions\tmicros");

    // No-op transaction: nothing executes and no revision is published.
    let runtime = Runtime::new();
    runtime.install(map_workload::Lookup).expect("install");
    runtime
        .edit(|edit| {
            let mut table = edit.view::<Table>();
            let mut entries = table.entries();
            entries.insert(1, 10);
        })
        .expect("seed");
    let _ = runtime
        .read_outputs::<map_workload::Lookup>()
        .expect("warm");
    let before = runtime.stats();
    let started = Instant::now();
    for _ in 0..100 {
        runtime
            .edit(|edit| {
                let mut table = edit.view::<Table>();
                let mut entries = table.entries();
                entries.insert(1, 10);
            })
            .expect("no-op");
    }
    let elapsed = started.elapsed();
    let after = runtime.stats();
    println!(
        "no-op\t100\t{}\t{}\t{}",
        after.executions - before.executions,
        after.revisions - before.revisions,
        elapsed.as_micros()
    );

    // Unrelated map key: a lookup of key 1 must not execute consumer work.
    let before = runtime.stats();
    let started = Instant::now();
    runtime
        .edit(|edit| {
            let mut table = edit.view::<Table>();
            let mut entries = table.entries();
            entries.insert(2, 20);
        })
        .expect("edit");
    let _ = runtime
        .read_outputs::<map_workload::Lookup>()
        .expect("read");
    let elapsed = started.elapsed();
    let after = runtime.stats();
    println!(
        "unrelated-key\t1\t{}\t{}\t{}",
        after.executions - before.executions,
        after.revisions - before.revisions,
        elapsed.as_micros()
    );

    // Tree leaf edit at increasing sizes: only the changed path executes.
    for size in [100usize, 1_000, 10_000] {
        let runtime = Runtime::new();
        runtime.install(tree_workload::Total).expect("install");
        runtime
            .edit(|edit| {
                let mut tree = edit.view::<syntax::Syntax>();
                tree.set_root(&document(size));
            })
            .expect("seed");
        let _ = runtime
            .read_outputs::<tree_workload::Total>()
            .expect("warm");
        let before = runtime.stats();
        let started = Instant::now();
        runtime
            .edit(|edit| {
                let mut tree = edit.view::<syntax::Syntax>();
                let root = tree.root();
                let items = root.items();
                let leaf = items[size / 2].clone();
                tree.replace_expr(leaf, &syntax::Expr::Name(String::from("-1")));
            })
            .expect("edit");
        let _ = runtime
            .read_outputs::<tree_workload::Total>()
            .expect("read");
        let elapsed = started.elapsed();
        let after = runtime.stats();
        println!(
            "tree-leaf\t{size}\t{}\t{}\t{}",
            after.executions - before.executions,
            after.revisions - before.revisions,
            elapsed.as_micros()
        );

        // From-scratch baseline: evaluate the same sum without the runtime.
        let values: Vec<i64> = (0..size).map(|index| index as i64).collect();
        let started = Instant::now();
        let mut total = 0i64;
        for _ in 0..10 {
            total = values.iter().sum();
        }
        let elapsed = started.elapsed() / 10;
        assert_eq!(total, (0..size as i64).sum::<i64>());
        println!("from-scratch\t{size}\t-\t-\t{}", elapsed.as_micros());

        // Specialized handwritten incremental algorithm: a running total
        // updated at the edited index. This is the floor the transparent
        // machinery is priced against.
        let mut values: Vec<i64> = (0..size).map(|index| index as i64).collect();
        let middle = size / 2;
        let mut running: i64 = values.iter().sum();
        let started = Instant::now();
        for step in 0..10_000i64 {
            let next = if step % 2 == 0 { -1 } else { -2 };
            running += next - values[middle];
            values[middle] = next;
        }
        let per_edit = started.elapsed().as_secs_f64() * 1e6 / 10_000.0;
        std::hint::black_box(&values);
        assert_eq!(running, (0..size as i64).sum::<i64>() - middle as i64 - 2);
        println!("handwritten\t{size}\t-\t-\t{per_edit:.4}");

        // Whole-function memoization baseline: an edit invalidates the entry,
        // so every transaction recomputes the whole function.
        let mut cache: std::collections::HashMap<u64, i64> = std::collections::HashMap::new();
        let started = Instant::now();
        let mut memo_total = 0i64;
        for edit in 0..10u64 {
            // A new edit changes the fingerprint, missing the cache.
            let key = values.len() as u64 + edit;
            memo_total = *cache.entry(key).or_insert_with(|| values.iter().sum());
        }
        let elapsed = started.elapsed() / 10;
        assert_eq!(memo_total, values.iter().sum::<i64>());
        println!("memoized-miss\t{size}\t-\t-\t{}", elapsed.as_micros());
    }
    cross_family_rows();
}
