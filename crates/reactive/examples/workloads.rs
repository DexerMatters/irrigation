//! Workload matrix: no-op transactions, map lookups and tree leaf edits.
//!
//! Run with `cargo run --release --example workloads -p reactive`.
//!
//! Every row reports the executions caused by the transaction plus the
//! observer read, which is the plan's "no unnecessary body execution" measure.

use std::time::Instant;

use reactive::{reactive, tree_view, view, Runtime};

#[view(source)]
pub struct Table {
    entries: Map<u32, u64>,
}

#[reactive]
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

#[reactive]
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
    let _ = runtime.read_outputs::<map_workload::Lookup>().expect("warm");
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
    let _ = runtime.read_outputs::<map_workload::Lookup>().expect("read");
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
        let _ = runtime.read_outputs::<tree_workload::Total>().expect("warm");
        let before = runtime.stats();
        let started = Instant::now();
        runtime
            .edit(|edit| {
                let mut tree = edit.view::<syntax::Syntax>();
                let root = tree.root();
                let items = root.items();
                let leaf = items[size / 2].clone();
                tree.replace_expr(leaf, &syntax::Expr::Literal(syntax::Literal::Int(-1)));
            })
            .expect("edit");
        let _ = runtime.read_outputs::<tree_workload::Total>().expect("read");
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
        println!(
            "from-scratch\t{size}\t-\t-\t{}",
            elapsed.as_micros()
        );

        // Whole-function memoization baseline: an edit invalidates the entry,
        // so every transaction recomputes the whole function.
        let mut cache: std::collections::HashMap<u64, i64> = std::collections::HashMap::new();
        let started = Instant::now();
        for edit in 0..10u64 {
            // A new edit changes the fingerprint, missing the cache.
            let key = values.len() as u64 + edit;
            total = *cache.entry(key).or_insert_with(|| values.iter().sum());
        }
        let elapsed = started.elapsed() / 10;
        println!(
            "memoized-miss\t{size}\t-\t-\t{}",
            elapsed.as_micros()
        );
    }
}
