//! The integrated fixture: one corpus drives both the report and the gates.
//!
//! Every script is a list of text edits over a named document. The runner builds
//! the real pipeline, applies the edits, and records *work counters* — not
//! timings — so the gates are machine independent. `cargo test` reads
//! `fixtures/baseline.json`; `FIXTURE_REPORT=1` prints the table and the
//! latency, which is what a human compares across changes.

mod support;

use std::ops::Range;
use std::path::PathBuf;

use irrigation_syntax::TextEdit;

/// One scripted edit.
#[derive(Debug)]
struct Edit {
    range: Range<usize>,
    text: String,
}

/// One fixture script.
#[derive(Debug)]
struct Script {
    name: String,
    doc: String,
    gated: bool,
    edits: Vec<Edit>,
}

/// The counters a fixture run records.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counters {
    tokens_relexed: u64,
    bytes_relexed: u64,
    tokens_reparsed: u64,
    nodes_built: u64,
    nodes_added: u64,
    nodes_removed: u64,
    meta_units_written: u64,
    token_units_read: u64,
    executions: u64,
    adopted: u64,
    invalidations: u64,
    invalidation_ns: u64,
    content_commits: u64,
    latency_us: u64,
}

impl Counters {
    fn record(&mut self, row: &support::Row) {
        self.tokens_relexed = self.tokens_relexed.max(row.tokens_relexed);
        self.bytes_relexed = self.bytes_relexed.max(row.bytes_relexed);
        self.tokens_reparsed = self.tokens_reparsed.max(row.tokens_reparsed);
        self.nodes_built = self.nodes_built.max(row.nodes_built);
        self.nodes_added = self.nodes_added.max(row.nodes_added);
        self.nodes_removed = self.nodes_removed.max(row.nodes_removed);
        self.meta_units_written = self.meta_units_written.max(row.meta_units_written);
        self.token_units_read = self.token_units_read.max(row.token_units_read);
        self.executions = self.executions.max(row.executions);
        self.adopted = self.adopted.max(row.adopted);
        self.invalidations = self.invalidations.max(row.invalidations);
        self.invalidation_ns = self.invalidation_ns.max(row.invalidation_ns);
        self.content_commits = self.content_commits.max(row.content_commits);
        self.latency_us = self.latency_us.max(row.micros as u64);
    }
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn load_doc(name: &str) -> String {
    let path = fixture_root().join("docs").join(format!("{name}.json"));
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn load_scripts() -> Vec<Script> {
    let filter: Option<Vec<String>> = std::env::var("FIXTURE_SCRIPTS")
        .ok()
        .map(|list| list.split(',').map(str::trim).map(str::to_owned).collect());
    let dir = fixture_root().join("scripts");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter(|path| match &filter {
            None => true,
            Some(names) => path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| names.iter().any(|name| name == stem)),
        })
        .map(|path| {
            let text = std::fs::read_to_string(&path).expect("script");
            let value: serde_json::Value = serde_json::from_str(&text).expect("script json");
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("script name")
                .to_owned();
            let doc = value["doc"].as_str().expect("script doc").to_owned();
            let gated = value["gated"].as_bool().unwrap_or(true);
            let edits = value["edits"]
                .as_array()
                .expect("script edits")
                .iter()
                .map(|edit| {
                    let range = edit["range"].as_array().expect("range");
                    let start = range[0].as_u64().expect("start") as usize;
                    let end = range[1].as_u64().expect("end") as usize;
                    Edit {
                        range: start..end,
                        text: edit["text"].as_str().expect("text").to_owned(),
                    }
                })
                .collect();
            Script {
                name,
                doc,
                gated,
                edits,
            }
        })
        .collect()
}

/// Runs one script and returns its counters plus the final document.
fn run(script: &Script) -> (Counters, String) {
    let text = load_doc(&script.doc);
    let mut session = support::Session::new(&text);
    let mut counters = Counters::default();
    for edit in &script.edits {
        assert!(
            edit.range.end <= session.document.text().len(),
            "{}: edit {:?} is outside the document",
            script.name,
            edit.range
        );
        let row = session.apply(TextEdit::new(edit.range.clone(), edit.text.clone()));
        assert!(
            row.failure.is_none(),
            "{}: pipeline failed: {:?}",
            script.name,
            row.failure
        );
        // Every script checks the tree against the tokens it walked. This is
        // the check a separate consistency gate used to name; it lives here so
        // it costs no second pipeline run.
        assert!(
            session.ast_covers_tokens(),
            "{}: the AST must cover the significant tokens",
            script.name
        );
        counters.record(&row);
    }
    (counters, session.document.text().to_owned())
}

#[test]
fn fixture_work_is_proportional_to_the_edit() {
    let baseline = load_baseline();
    let mut failures = Vec::new();
    for script in load_scripts().into_iter().filter(|script| script.gated) {
        let (counters, _) = run(&script);
        let bound = baseline
            .get(&script.name)
            .unwrap_or_else(|| panic!("{}: no baseline entry", script.name));
        if std::env::var_os("FIXTURE_REPORT").is_some() {
            // What the module doc promises: the table a human compares across changes.
            // Only the pass/fail verdict used to exist, so a change that improved
            // reactivity inside the bounds was invisible.
            println!(
                "{}: tokens_relexed={} tokens_reparsed={} nodes_added={} \
                 token_units_read={} meta_units_written={} invalidations={} content_commits={}",
                script.name,
                counters.tokens_relexed,
                counters.tokens_reparsed,
                counters.nodes_added,
                counters.token_units_read,
                counters.meta_units_written,
                counters.invalidations,
                counters.content_commits
            );
        }
        if counters.tokens_relexed > bound.tokens_relexed
            || counters.tokens_reparsed > bound.tokens_reparsed
            || counters.nodes_added > bound.nodes_added
            || counters.token_units_read > bound.token_units_read
            || counters.meta_units_written > bound.meta_units_written
            || counters.invalidations > bound.invalidations
            || counters.content_commits > bound.content_commits
        {
            failures.push(format!("{}: {counters:?} exceeds {bound:?}", script.name));
        }
    }
    assert!(
        failures.is_empty(),
        "fixture regressions:\n{}",
        failures.join("\n")
    );
}

/// The incremental AST equals a from-scratch parse of the same text.
///
/// Work counters say an edit was *local*; they say nothing about whether the
/// tree that locality preserved is the tree the text describes. The signature
/// carries node kinds, child order and token text, so it is independent of node
/// and token identity: an incremental tree and a fresh parse of the same
/// document must produce the same signature at every revision.
fn assert_ast_matches_from_scratch(session: &support::Session, what: &str) {
    let fresh = support::Session::new(session.document.text());
    assert_eq!(
        session.text_signature(),
        fresh.text_signature(),
        "{what}: the incremental AST disagrees with a from-scratch parse"
    );
}

/// The from-scratch oracle, at every revision of a small document.
///
/// The oracle parses the whole text again in a fresh pipeline, so it costs one
/// full parse per revision. That is why it runs on `small.json`: the corpus in
/// [`load_scripts`] lives on a document whose full parse dominates a debug
/// build, and a gate that costs minutes is not run. The corpus gate below keeps
/// its own job (the pipeline must not fail, the AST must cover the tokens, and
/// the work must stay proportional), and this one owns equivalence.
#[test]
fn the_incremental_ast_equals_a_from_scratch_parse_at_every_revision() {
    let text = load_doc("small");
    let mut session = support::Session::new(&text);
    assert_ast_matches_from_scratch(&session, "initial");

    // A value, a key, a member, and a nested array: each edit changes the tree,
    // so an equality that ignored the tree would not survive all four.
    let edits: [(Range<usize>, &str); 4] = [
        (find(&text, "\"value\":0"), "\"value\":41"),
        (find(&text, "\"name\":\"item 1\""), "\"name\":\"renamed\""),
        (find(&text, "\"beta\"]"), "\"beta\",\"gamma\"]"),
        (find(&text, "\"tags\""), "\"tags\""),
    ];
    for (step, (range, replacement)) in edits.into_iter().enumerate() {
        let start = range.start;
        let len = range.end - range.start;
        let row = session.replace(start..start + len, replacement);
        assert!(row.failure.is_none(), "step {step}: {:?}", row.failure);
        assert_ast_matches_from_scratch(&session, &format!("edit {step}"));
    }

    // And back to the original text, so the round trip is exact too.
    session.replace(0..session.document.text().len(), &text);
    assert_ast_matches_from_scratch(&session, "round trip");
}

/// The byte range of the first occurrence of `needle`.
fn find(haystack: &str, needle: &str) -> Range<usize> {
    let start = haystack
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} is not in the fixture document"));
    start..start + needle.len()
}

/// Every script's tree covers the tokens it walked.
///
/// The assertion itself lives in [`run`], so it holds for the report table and
/// the proportionality gate too; this test exists to run the corpus when a
/// change is about AST consistency rather than about counters.
#[test]
fn fixture_keeps_the_ast_consistent() {
    for script in load_scripts().into_iter().filter(|script| script.gated) {
        let _ = run(&script);
    }
}

#[test]
#[ignore = "report mode: run with --ignored --nocapture to print the table"]
fn fixture_report() {
    println!(
        "{:<16} {:>6} {:>6} {:>7} {:>8} {:>6} {:>6} {:>8} {:>10} {:>10}",
        "script", "relex", "bytes", "reparse", "built", "added", "read", "meta", "execs", "us"
    );
    for script in load_scripts() {
        let (counters, _) = run(&script);
        println!(
            "{:<16} {:>6} {:>6} {:>7} {:>8} {:>6} {:>6} {:>8} {:>10} {:>10}",
            script.name,
            counters.tokens_relexed,
            counters.bytes_relexed,
            counters.tokens_reparsed,
            counters.nodes_built,
            counters.nodes_added,
            counters.token_units_read,
            counters.meta_units_written,
            counters.executions,
            counters.latency_us
        );
    }
}

/// The recorded upper bounds of one script.
#[derive(Clone, Copy, Debug)]
struct Bound {
    tokens_relexed: u64,
    tokens_reparsed: u64,
    nodes_added: u64,
    meta_units_written: u64,
    token_units_read: u64,
    invalidations: u64,
    content_commits: u64,
}

fn load_baseline() -> std::collections::HashMap<String, Bound> {
    let path = fixture_root().join("baseline.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let value: serde_json::Value = serde_json::from_str(&text).expect("baseline json");
    let mut bounds = std::collections::HashMap::new();
    for (name, entry) in value["scripts"].as_object().expect("scripts") {
        let read = |key: &str| {
            entry[key]
                .as_u64()
                .unwrap_or_else(|| panic!("{name}.{key}"))
        };
        bounds.insert(
            name.clone(),
            Bound {
                tokens_relexed: read("tokens_relexed"),
                tokens_reparsed: read("tokens_reparsed"),
                nodes_added: read("nodes_added"),
                meta_units_written: read("meta_units_written"),
                token_units_read: read("token_units_read"),
                invalidations: read("invalidations"),
                content_commits: read("content_commits"),
            },
        );
    }
    bounds
}
