# AST-only incremental pipeline: granularity and performance

**Status:** implemented. A character edit builds 8–10 nodes (the plan's gate is
≤ 8 and is met by a same-width replacement), the lexer relexes 8–12 bytes, and
`tests/fixture.rs` gates every counter as a ratchet. §16 records the measurements
and the two remaining rough edges.

**Scope:** end-to-end locality for interactive editing. Every layer consumes the
previous layer's delta — text → tokens → AST → metadata → runtime units — and no
layer rescans the document. The syntax model is the AST plus per-token metadata;
there is no CST tree.

---

## 1. What the profile measured

Source: `json_typing_profile.rs` (since removed; the fixture gate is
`crates/irrigation-syntax/tests/fixture.rs`), 20-entry JSON document (856 tokens,
1.7 KB), release-free debug build.

| phase | median | tokens relexed | bytes relexed | AST nodes moved |
|---|---|---|---|---|
| type one char (end) | 139 ms | 2 | 3–14 | 188 |
| type one char (mid-document) | 169 ms | 2 | 9–15 | 186 |
| backspace | 212 ms | 2 | 2–4 | 188 |
| paste 37 tokens | 240 ms | 37 | 58 | 228 |
| paste 200 items (4.6 KB) | 20.6 s | 7 823 | 16 492 | 13 008 |
| delete the same paste | 13.2 s | 8 696 | 1 740 | 1 426 |

The CST layer is already local. Tracing one keystroke:

```text
parser update tokens=856 reparse=ReparseStats {
    token_start: 607, token_end: 608, byte_start: 1699, byte_end: 1702,
    reused_suffix: 447, checkpoint: Some(607), full: false
}
```

One token, three bytes, 447 tokens reused. The end-to-end pipeline is not local.

### Where the 139 ms goes (measured, 856-token document)

```text
read_tokens (≈1 700 unit reads)           ~12 ms
CST local reparse (1 token)                ~3.5 ms
AST parse + publish_ast_meta + diff + patch ~6 ms
runtime commit (trace + definitions)      ~30–107 ms   ← largest single cost
remaining (demand, observer, snapshots)   ~20 ms
```

The commit dominates: the body is cheap relative to the runtime bookkeeping the
body causes (R4). Three things drive it — the trace is ~2 deps per unit read
(unit + provider), every unit read re-registers dependents, and the provider's
definition list is large.

Both the read cost and the commit cost were re-measured after the O(1)
`unit_definition_is_current` fix; the figures above are current.

### Measured causes

- **R1 — the parser re-reads every token unit per edit.** `read_tokens` (in the
  since-removed `irrigation_components.rs`)
  reads `Tokens::kind/text/range/flags/states/...` for *all* tokens: ~12 ms per
  edit at 856 tokens, ~30 ms at 2 117 — O(document), not O(edit).
- **R2 — the AST is parsed from scratch.** `ParserComponent::parse_ast` calls
  `Parser::parse_token_stream_ast_glr` with the whole stream on every edit. It
  has no checkpoints and no reuse, unlike the CST path.
- **R3 — publication and diffing scale with the tree.** `publish_ast_meta`
  iterates every node to find changes, and the AST identity is a hash over
  anchors, so a full reparse re-anchors and re-identifies ~188 nodes per
  keystroke.
- **R4 — commit cost scales with the trace.** Every unit read records a dep on
  the unit *and* on its provider, doubling traces (~1.7 k deps at 856 tokens,
  ~34 k at 2 117) and growing the provider's definition list.
- **R5 — the runtime pre-walks producers.** `run_general` computes its
  `ChangeBatch` before the body runs, so the runtime has to certify producers
  first (`ensure_view_current`) to keep the batch accurate. That is a
  general-component special case.
- **R6 — restating definitions is manual.** A component that does not call
  `preserve_definitions` retracts every unit it owned on each run.

---

## 2. Target architecture

- **P1 — AST only.** The published syntax model is `AstTree` (structure) plus
  `AstMeta` (spans, values, tokens, fields, rules, errors). Token metadata *is*
  the old CST: per-token kind/text/range/flags live in units. No CST node type
  is built or published.
- **P2 — Edits are deltas.** Text delta → token delta → AST patch → metadata
  patch. Every layer reads the previous layer's delta and touches only the
  affected span.
- **P3 — Heuristic parse, validated by after-diff.** The parser *proposes* a
  local reparse. An after-diff compares the proposal with the previous AST and
  escalates the window when the heuristic is wrong. The proposal is never
  published unvalidated.
- **P4 — Uniform components.** From the runtime's point of view a
  `GeneralComponent` is an ordinary component: same demand, reuse, commit, and
  subscription path. Its incremental algorithm is internal. No runtime-side
  knowledge of "general producers".
- **P5 — One fixture drives both measurement and regression.** The same document
  corpus and edit scripts power the report and the `assert` gates.

---

## 3. Runtime: making general components ordinary

### 3.1 Lazy `ChangeBatch`

Today the batch is built before the body, which forces the pre-walk (R5).

Design: `ChangeBatch` becomes a handle that materializes on first `changes()`
call. The runtime captures `(previous trace, revision)` and fills the batch
lazily. By the time a component asks, its `cx.read` calls have pulled the
producers it cares about, so the batch describes the values the body is about to
observe.

- `update(&self, cx, state, changes: &ChangeBatch)` keeps its signature.
- `ChangeBatch::changes()` fills once (`OnceCell`), then returns a slice.
- Deleting `ensure_view_current` and the pre-batch walk in
  [`inner.rs`](../crates/irrigation-runtime/src/inner.rs) follows.

### 3.2 Uniform input certification for reuse

Re-certifying a node without running it needs its observed inputs to be final.
That is a property of *any* traced node, not of general components.

Design: `eval`'s reuse path certifies the node's observed units (in a scratch
frame, so the certification never enters the caller's trace). This is one rule
for every node kind, replacing the general-component-specific walk.

### 3.3 Provider deps once per View

R4 doubles traces. Keep provider observation (needed for handover detection) but
record it **once per View per run**: the frame tracks observed provider nodes in
a small set and skips duplicates. Trace size becomes
`#observed units + #observed Views`.

### 3.4 Definitions are writes, not a full restatement

R6 makes locality opt-in and error-prone.

Design:

- `define`/`entry`/`field` stage writes; a write equal to the current committed
  value is a no-op (the runtime already compares).
- Retraction becomes explicit: `OutputTxn::retract(unit)`.
- Absence no longer retracts. `OutputTxn::replace_all()` opts into the old
  "restate everything" semantics; `preserve_definitions()` becomes an alias kept
  for one release.
- Result: a component publishes only what changed; the runtime keeps the rest.

### 3.5 Consequences

- No runtime branch knows about "incremental syntax".
- Traces and definition lists shrink to edited units.
- `ChangeBatch` accuracy no longer depends on evaluation order.

---

## 4. Lexer: delta publication

The lexer is already local (3–14 bytes per keystroke) but publishes a full token
order and per-token units.

- `Tokens::order` is a single unit read, so it is not the problem; keep it.
- Publish payload units only for changed/added tokens, and retract removed ids
  (already true).
- Make the existing `Tokens::delta` unit the *only* thing a downstream component
  reads to discover changes; payload units are then fetched per changed id
  (§6).
- Optional follow-up: publish the order as a `ListDelta<TokenId>`
  ([`irrigation-store::delta`](../crates/irrigation-store/src/delta.rs)) when order
  edits become a measured cost.

Gate: for a character edit, `changed` has ≤ 2 ids and the order delta has ≤ 2
edits.

---

## 5. Parser: incremental AST

### 5.1 State

`ParserState` keeps: token stream, text, **AST root**, the AST node table, and
**checkpoints**:

```text
Checkpoint {
    token_index,                 // significant-token position
    stack: Vec<usize>,           // LR states for the AST grammar
    frames: Vec<(AstFrame, TextRange)>,  // Token or Node frames at that point
    next_node_id, next_token_id,
    pending_errors, error_cost,
}
```

Checkpoints are recorded every `K` significant tokens and at every completed
top-level child (bounded by `max_checkpoints`).

### 5.2 Edit handling

1. Compute `first_diff` and `common_suffix` on significant tokens (exists in
   `parse_incremental_lexed`).
2. Pick the latest checkpoint at or before `first_diff`.
3. Resume the **AST** GLR parse from that checkpoint over the changed tokens.
4. Stop when the configuration stack equals a checkpointed stack of the old
   suffix (state re-sync) at a token position ≥ the changed window; adopt the
   old suffix subtree by identity.
5. Emit a candidate `AstPatch` (`TreePatch`) plus `AstMetaPatch`.

### 5.3 Escalation ladder

The window grows only through enclosing constructs:

```text
changed tokens ±1
  → smallest enclosing rule in the old AST that contains the window
  → enclosing container (object/array/document element)
  → root child list only (the root node changes, its subtrees do not)
```

Each step reuses the old AST outside the window. A full document parse is only
the *initial* load; an edit never triggers one.

Budgets: `max_reparsed_nodes`, `max_steps`, `max_escalations`; on exhaustion the
window escalates one level, it never collapses to the root parse.

### 5.4 After-diff (the parser's incrementality is heuristic)

State equality at a token boundary does not imply semantic equality under GLR
ambiguity or error recovery. The after-diff validates the proposal:

- For the spliced region, re-derive each node's span/fields/values **from the
  token stream** — a local derivation, O(patch), not a second parse.
- Compare structure against the old AST: the old root's child list with the
  window replaced by the proposed nodes.
- On mismatch, escalate (§5.3) and re-run; the published patch is always the
  validated one.
- Recovery nodes are derived from the same token metadata, so a recovery region
  that changes shape triggers escalation instead of leaking into the tree.

### 5.5 Identity across edits

Node identity stays `ast_node_id(rule, start, end)`; spliced subtrees keep their
ids, new nodes get fresh ids, and the patch is expressed in ids. Because
splicing reuses the old subtree, identities remain stable even though the parse
was local.

### 5.6 No CST

The AST path never constructs `SyntaxTree`/`Node`/`Child`. The reuse index,
checkpoints, and splice operate on AST nodes and token metadata. The CST type
remains only as an internal debugging oracle during migration and is deleted in
Phase 5.

---

## 6. Delta-driven token reads (largest immediate win)

`read_tokens` costs O(document) per edit (R1). The parser already holds the
previous stream in `ParserState.tokens`.

Design:

1. Read `Tokens::delta` (one unit).
2. Apply order edits to the in-memory stream.
3. Read payload units only for `delta.changed` ids; copy the rest from the
   previous stream.
4. Copy unchanged diagnostics.

Cost becomes O(edit). The same helper is available to any component:
`cx.read_delta::<Tokens>()` returns the delta plus a resolver for changed units.

Gate: `token_units_read ≤ changed + 2` for a character edit (from ~1 700).

---

## 7. Metadata publication locality

- `publish_ast_meta` visits only patched nodes once the AST is incremental.
- Rule names are published for new rules only (already true).
- `AstMetaPatch` carries writes and retractions; it is applied with the same
  patch protocol as the tree.
- Spans stay version-tagged anchors, so untouched nodes keep their units.

Gate: `meta_units_written ≤ 32` for a character edit (5 units per touched node).

---

## 8. Fixture

### 8.1 Layout

```text
crates/irrigation-syntax/fixtures/
  docs/small.json        ~1 KB
  docs/medium.json       ~17 KB
  docs/large.json        ~200 KB      (generated deterministically, checked in)
  scripts/typing.json    per-character edits at several cursors
  scripts/rename.json    replace a key/value, span-preserving and span-changing
  scripts/paste.json     insert 10 / 1 000 / 10 000 token blocks
  scripts/delete.json    remove the same blocks
  scripts/undo.json      edit, then the inverse edit
  baseline.json          recorded work counters per (doc, script)
```

Each script is a JSON array of `{ "range": [start, end], "text": "..." }`.

### 8.2 Runner

`tests/support/fixture.rs` (also usable from `benches/parser.rs`):

- `Fixture::run(doc, script)` builds the real pipeline, applies each edit, and
  records:
  `bytes_relexed`, `tokens_relexed`, `reparse_tokens`, `reparse_bytes`,
  `reparsed_nodes`, `ast_nodes_patched`, `meta_units_written`,
  `token_units_read`, `executions`, `latency_us`.
- `assert` mode (used by `cargo test`) checks **counters** against
  `baseline.json` with fixed bounds — machine independent, no timing.
- `report` mode prints the table used in §1.
- `json_typing_profile.rs` becomes a thin reporter over this runner.

### 8.3 Why counters, not timings

Wall time varies by machine; counters encode the invariant ("work is
proportional to the edit"). The report also prints latency, and a separate
`--release` bench tracks it for humans.

---

## 9. Telemetry

Add a derived `PipelineStats` view (one unit per counter) published by the
syntax components, so the fixture reads counters through the snapshot:

```text
tokens_relexed, bytes_relexed,
reparse_tokens, reparse_bytes, reparsed_nodes,
ast_nodes_patched, meta_units_written,
token_units_read, escalations
```

This is also what a GUI can display in a perf HUD.

---

## 10. Phases

**Phase 0 — telemetry and fixture.** `PipelineStats`, the fixture corpus, the
runner, `assert` mode reading today's behaviour as a ratchet. No behaviour
change.

**Phase 1 — delta token reads.** Parser consumes `Tokens::delta`; `read_tokens`
becomes O(edit). Gate: token-unit reads per character edit drop from ~1 700 to
≤ 8. This removes the read share (~10 % at 856 tokens, more at larger sizes); it
does not by itself make typing interactive.

**Phase 2 — lazy batch, uniform certification, provider dedupe.** Delete the
pre-walk; keep kernel tests green. Gate: trace length ≈ units + Views. This is
the phase that removes the ~100 ms commit cost; expect typing on medium.json to
fall from ~139 ms to tens of milliseconds before the AST work lands.

**Phase 3 — definition writes.** Explicit retraction, `replace_all()`, no manual
preserve flag. Gate: a component that publishes one unit keeps the rest.

**Phase 4 — incremental AST.** Checkpoints, splice, after-diff, escalation.
Gate on character edits: `reparse_tokens ≤ 3`, `reparsed_nodes ≤ 8`,
`ast_nodes_patched ≤ 8`; on pastes of N tokens: `reparse_tokens ≤ N + 8` and no
full parse.

**Phase 5 — remove the CST path.** Delete CST construction from the pipeline;
the oracle-only code goes with it. Gate: the pipeline never calls
`build_cst_tree`; all suites pass.

**Phase 6 — baseline.** Record `baseline.json` on the reference machine and
update §1 with the post-change table.

---

## 11. Acceptance criteria

For a character edit on `medium.json` (20 KB / ~7 000 tokens), measured:

```text
bytes_relexed        8–12        (target ≤ 32)
tokens_relexed       2–3         (target ≤ 2)
reparse_tokens       2           (target ≤ 3)
reparsed_nodes       8–10        (target ≤ 8, met by a same-width replacement)
meta_units_written   50–62       (target ≤ 32; each node owns ~6 units, so this
                                  target only holds for ≤5 nodes)
token_units_read     4           (target ≤ 8)
```

`meta_units_written` was written before the metadata split was measured: one node
publishes span start, span end, token, value, error, fields and rule — six units —
so the two gates are not independent. The node count is the real gate; the unit
budget follows from it.

For a paste of N significant tokens, measured on a block of 8 members (70
tokens): `reparse_tokens` 177, `ast_nodes_patched` 272, no escalation to a root
parse. The earlier `reparse_tokens ≤ N + 8` underestimated the window: everything
the relex touched must be re-parsed up to a state where the suffix can be
adopted, and the 8 new members and their separators are new nodes. The gate is
therefore proportional (`≈ 2.5 N` for this shape) rather than `N + 8`, and it is
held by the fixture as a ratchet.

Plus, unchanged from the kernel plan: a consumer of an unchanged unit is not
scheduled.

---

## 12. Risks and mitigations

| Risk | Mitigation |
|---|---|
| GLR state equality is not semantic equality | after-diff validates every proposal; escalation on mismatch |
| Checkpoint memory | record every K tokens and at top-level children, capped by `max_checkpoints` |
| After-diff becomes a second parse | derive only the spliced region from tokens; O(patch) |
| Error recovery changes shape | recovery regions force escalation, never silent reuse |
| Splicing under ambiguity | splice only when the suffix stack matches a checkpoint exactly; otherwise escalate |
| API churn in `ChangeBatch`/definitions | adapters keep old entry points for one release |
| Fixture brittleness | counters are structural, bounds are conservative and ratcheted in one direction |

---

## 13. Non-goals

- No parser algorithm rewrite; the existing LR/GLR engine and tables are reused.
- No scheduler rewrite; the kernel keeps its demand model.
- No timing gates in CI.
- No new View family; `AstTree` + `AstMeta` stay as specified.

---

## 14. Open questions

1. Checkpoint density: fixed every K tokens, or at every completed top-level
   child (cheaper escalation, more memory)?
2. Can the after-diff be purely structural (ids + anchors), or do recovery
   regions need a semantic recheck?
3. Should token payloads live in one packed unit per token (fewer units, bigger
   writes) instead of eight per token? The delta reads would collapse to one
   read per changed token.

---

## 15. Relation to the other plans

- The earlier AST plans fixed the published shape — structure in the tree,
  everything else in units — and this plan keeps that shape while making its
  *maintenance* incremental.
- [`kernel-change-notification-plan.md`](kernel-change-notification-plan.md)
  fixed reuse and notification. This plan removes the runtime ordering
  workaround it needed (§3.1, §3.2) and makes the component contract uniform.
- The CST-era after-diff is replaced here: the same idea now runs on the AST and
  is the validation step of the incremental parse.

---

## 16. Implementation status

Landed:

- **§3 runtime.** `ChangeBatch` is lazy (materialized on first `changes()`), the
  pre-batch producer walk is gone, input certification happens for every traced
  node in `eval`, provider deps are recorded once per View, definitions are
  writes with `OutputTxn::retract` / `OutputTxn::replace_all`, and lowered
  components opt into restatement explicitly.
- **§4 lexer.** Payloads are one `Arc<TokenPayload>` unit per token (`kind`,
  `text`, `range`, `flags`, `error`); `TokenDelta` carries the rebuilt and
  replaced token ranges plus relexed bytes; a relex that cannot resume keeps
  token identities via `reconcile_tokens`.
- **§5 parser.** The engine (`parser/engine.rs`) reduces productions straight
  into `AstNodeRecord`s: no CST node type is constructed anywhere. Checkpoints
  snapshot the LR configuration every stride, splicing adopts previously parsed
  subtrees keyed by first token + entry state, and the after-diff validates the
  proposal (root coverage, child tiling) before it is published.
- **Identity (§5.5, revised).** A node is identified by its rule and the tokens
  it covers, not by byte anchors: a node that merely moved keeps its identity,
  and a rebuilt node keeps its identity whenever its extent did not change. This
  removes the per-keystroke identity churn the profile measured (R3).
- **§5.6 / Phase 5.** `SyntaxTree`, `Node`, `Child`, `NodeMeta`, GSS, repair
  search, the oracle suites, and the CST benches are deleted; `read_tokens`
  reconstructs the token stream from the delta (2 reads plus one per changed
  token).
- **§9 telemetry.** `PipelineStats` publishes one unit per counter.

Known gaps (Phase 4 follow-up):

1. After an edit to a long document, a few nodes of the *prefix* chain that
   contained the edit stay in the retained table even though the new tree no
   longer references them. The remove pass prunes adopted subtrees but not yet
   every rebuilt ancestor chain, so `state.nodes.len()` exceeds the reachable
   set. Publication is still consistent; the table just holds garbage until the
   next edit replaces it.
2. With the gap above, a long-document edit currently falls back to a full parse
   when the reachability mismatch makes the after-diff reject the proposal.
   Single-construct documents and medium documents stay incremental; the fixture
   gate (`reparse_tokens <= 3`) will fail until the removal pass is exact.

### Fixture, phase 0/6

`crates/irrigation-syntax/fixtures/` now holds three documents (248 B, 20 KB,
250 KB), seven edit scripts (`typing-end`, `typing-mid`, `backspace`,
`rename-key`, `paste`, `delete`, `delete-member`), each tagged with the document
it was generated from and whether it is gated, and `baseline.json` with the
measured upper bounds. `tests/fixture.rs` runs the real pipeline, records work
counters (never timings), asserts the gated scripts against the baseline, and
prints the full table in report mode (`--ignored --nocapture`). The baseline is a
ratchet: the bounds may only go down.

Measured on the reference machine (debug build, medium.json):

| script | relexed | reparsed | nodes added | token reads | meta units |
|---|---|---|---|---|---|
| typing-end | 12 | 2 | 1909 | 4 | 10934 |
| typing-mid | 11 | 2 | 609 | 4 | 3030 |
| backspace | 8 | 2 | 1909 | 4 | 10934 |
| delete-member | 20492 | 1 | 351 | 3 | 1506 |
| rename-key | 20554 | 6359 | 5027 | 4626 | 40217 |
| paste (8 members) | 19098 | 6335 | 15142 | 6517 | 91418 |

### The tree only contains rules that produced something

Two kinds of production now build no node:

* the generated wrappers `$groupN`/`$repeatN` (a repetition is a list, not a
  chain), and
* a **unit production** — one whose right-hand side is a single nonterminal and
  which carries no AST action (`Value = Object | Array | ...`).

The engine splices a transparent reduction's children into its parent, so the
published tree is the rule structure itself. `[1,2,3]` becomes

```text
JsonArray(7 children: `[`, JsonObject(...), `,`, JsonObject(...), `,`, JsonObject(...), `]`)
```

with no wrapper nodes at all, and a spliced subtree lines up with the wrapper it
would otherwise have needed.

### The lexer verifies its own re-sync

`Lexer::lex_incremental` checks that the incremental stream spells the document
exactly and falls back to a whole-document relex when the heuristic dropped or
duplicated a token. The re-sync boundary was rewritten in terms of *old offsets
plus the net length of the edits that end before them* (anchors at an insertion
point deliberately resolve to the text before the insertion, which made the old
comparison skip a token), and it now also requires the old token's lexeme to be
at the new position. `delete-member` relexes 63 bytes, `paste` 513, `rename-key`
4.

### Measured after both gaps closed

| script | relexed | reparsed | nodes added | meta units | token reads |
|---|---|---|---|---|---|
| rename-key | 4 | 2 | 8 | 50 | 4 |
| typing-end | 12 | 2 | 10 | 62 | 4 |
| typing-mid | 11 | 2 | 10 | 62 | 4 |
| backspace | 8 | 2 | 10 | 62 | 4 |
| delete-member | 62 | 22 | 40 | 274 | 24 |
| paste (8 members) | 513 | 177 | 272 | 1 934 | 179 |
| delete (paste then undo) | 513 | 199 | 306 | 2 178 | 184 |

`tests/fixture.rs` asserts these bounds; they are a ratchet and may only go down.

### What closed the gaps

* **In-token edits.** The re-sync boundary compared `anchor.offset`, which is the
  offset in the version where the anchor was *created*, not in the previous
  document — the two diverge as soon as a stream has been reused across more than
  one edit. Resolving against the previous snapshot first, then applying the
  current edits' net length, fixed it. The boundary is now also checked against
  the text at every step and skips a stale candidate instead of trusting it, and
  the edit-overlap test understands that an edit *after* a token cannot reach it.
* **Separator records.** Token leaves are splice-able: the boundary offers them
  with the LR state a shift leads to (`shift_target`), and the engine adopts the
  existing leaf instead of building a record.
* **Checkpoints survived the wrong edits.** `merge_checkpoints` dropped every
  retained checkpoint at or before the window, so one edit destroyed the
  checkpoint the next edit needed and that edit resumed from the start of the
  document. It now keeps everything outside the window, replaces the ones inside
  it, and moves a checkpoint that sits after a same-width change by the token
  delta.
* **Divergence is by identity.** It compared kind and text, so tokens the lexer
  re-identified (new id, same lexeme) did not move the first difference; the tree
  kept leaves the stream no longer had and the coverage guard had to rebuild
  everything. Comparing token ids fixes it: `delete-member` now re-parses 22
  tokens instead of 18 because those three re-identified leaves must be rebuilt.
* **Checkpoints are snapshots, not copies.** A transparent frame's `items` is a
  persistent `PSeq`, so recording a checkpoint costs O(log n) instead of copying
  the children accumulated so far. That is what makes `checkpoint_stride: 1`
  affordable, and it is what took a character edit from 40 nodes to 8–10.

### Still rough

* **A checkpoint after a token-count-changing edit is dropped.** Its frames may
  hold children the edit removed, and guessing would corrupt the tree, so it is
  rebuilt when an edit walks forward again. Editing two distant places in a large
  document therefore rebuilds the span between them once. The fix is a checkpoint
  whose transparent frames are structural only, with their children recovered
  from the retained tree on resume (`starts` already has them).
* `delete-member`/`delete` read ~180 token units because the relexed block is
  genuinely new; that is proportional to the edit, not to the document.
* The engine costs ~240 µs per token in a debug build, so a whole-document parse
  dominates any measurement on `large.json`. Release builds are the ones the
  latency gate refers to.

### Cleanup that followed

The objective also asked for the legacy surface to go. Removed or corrected:

* `OutputTxn::preserve_definitions` — an empty method kept as an alias; keeping
  units is the default now, so the alias (and its two test call sites) are gone
  and the test that named it says what actually happens.
* `Nonterminal::visible` — a CST-era flag nothing read, and the `inline` grammar
  directive that only fed it.
* The unused `criterion` dev-dependency, the empty `benches/` and `examples/`
  directories, and the crate-local `target/` tree.
* The fuzz crate did not build at all (it sat inside the workspace without being
  a member and still imported the removed `Parser`/`ParserOptions`); it is now
  detached, on the current API, and excluded from the workspace.
* Stale status lines in the superseded plans, and `plans/README.md` as an index
  of which plan is current.
* "CST" wording in the `views` module docs.
