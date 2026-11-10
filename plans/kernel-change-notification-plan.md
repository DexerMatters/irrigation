# Kernel change notification: rerun a consumer only when an observed unit changed

**Status:** implemented.

A general component is re-certified without running its body when every unit it
observed still holds the version it recorded. Producers are certified before a
body is evaluated, and preserved definitions are certified when they are
committed, so "unchanged observation" and "stale observation" are distinguishable.

---

## 1. Observed defect

A component that read one derived unit reran whenever any unit of the same View
changed:

```text
edit "b"  →  ShowA (reads values[a]) runs, ChangeBatch is empty
```

Three independent causes.

### 1.1 General components reported "changed" unconditionally

Producer demand used a placeholder equality:

```rust
let eq = |_: &Value, _: &Value| false;
```

so every general-component commit looked like a value change. `commit` then
extended the invalidation seeds with **all dependents of the producer**, and
`invalidate` marked the whole closure `suspect_at = revision`.

### 1.2 Reuse was decided by a suspect stamp, not by observed versions

`eval` / `read_unit` reused only when

```rust
record.verified_at == revision || record.suspect_at <= record.verified_at
```

while every `Dep` already records the versions it observed:

```rust
pub struct Dep { pub node: NodeId, pub version: u64, pub routing: u64 }
```

Subscriptions already used the correct pattern (`versions != last_versions`).

### 1.3 Traces changed shape between runs

`read_unit` demanded a producer when the unit looked stale, and `eval` recorded
the producer as a dependency of the **caller**. The dependency appeared only on
runs that had to refresh, so a trace alternated between `[unit, provider]` and
`[unit]`. `commit` then retired the provider as unreachable — retracting every
unit it owned in the middle of a revision.

---

## 2. Invariant

For `DependencyUpdates::Incremental`:

> A component runs only if, since its last committed run, an observed unit's
> `changed_at` moved, its `routing_at` moved, or the component's retained
> definitions must be restaged. Reading an unchanged unit never schedules the
> reader.

`DependencyUpdates::Reset` keeps running whenever it is invalidated.

---

## 3. Implementation

### 3.1 Real output equality (`inner.rs`)

`ErasedGeneral::output_eq` (backed by `GeneralComponent::Output: PartialEq`) is
used at every demand site: `unit_node_init`, `ensure_unit`,
`demand_root_projection`, `demand_root`. A producer whose output did not change
no longer invalidates its dependents, and `ChangeBatch` is non-empty exactly when
an observed value moved.

### 3.2 Producers are certified before a body runs (`ensure_view_current`)

`run_general` certifies the producers of every View it observed, depth-first,
**before** computing the `ChangeBatch`. The walk runs inside a scratch frame, so
its demands never appear in any trace. This is what makes the batch describe the
values the body is about to observe rather than the ones it observed last time.

`reuse_current_general` performs the same walk when a component's versions match
but its units are not yet certified, then re-checks; if everything survived, the
component is re-certified instead of executed.

### 3.3 Reuse decision

```rust
fn trace_versions_match(state, node) -> bool   // same node identity, changed_at, routing_at
fn trace_is_certified(state, node) -> bool     // every observed unit final for this revision
```

`eval` re-certifies (setting `verified_at`, clearing `suspect_at`, bumping
`no_change_runs`) when both hold and the policy is `Incremental`.

### 3.4 Preserved definitions are certified

A producer that preserves a unit (`preserve_definitions`) leaves its value
authoritative. `install_definitions` certifies such units at commit, and
`unit_definition_is_current` lets a reader accept one that was certified lazily.
Without this a preserved unit looked permanently stale.

### 3.5 Stable traces

Every successful unit read records its View's producer as a dependency, so a
trace keeps the same shape on every run. Provider deps carry no unit; the reuse
check treats them as routing observations only.

### 3.6 Read-time producer demand (`bad_producer`)

`read_unit` demands a unit's producer when the producer is not certified for the
current revision. Without it a unit materialized from stale content could be
re-certified lazily and return the pipeline default.

### 3.7 Statistics

`Stats::no_change_runs` counts re-certifications and is exposed on `RuntimeStats`.
`invalidations_without_change` from the original draft was dropped: with exact
commit seeds (`output_eq` + "dependents only on a real change") there is no
second invalidation path to measure, and the counter would always read zero.

---

## 4. Edge cases

| Case | Behaviour |
|---|---|
| first run | never elided (`general_state` is `None`) |
| `Reset` policy | never elided; covered by `reset_component_reruns_when_invalidated` |
| routing handover | provider deps record `routing_at`; a move forces the body |
| definition conflict | conflicts run through `resolve_conflicts`, not the reuse path |
| retired branch | `retire_unreachable` still invalidates dependents |
| scope removal | `old_retained` filtering in the batch is unchanged |
| failure at the current revision | surfaced before reuse is considered |
| cancellation | unchanged; no commit, no certification |
| cycle | detected by `eval` before the reuse path |
| no reads | runs once, then re-certified forever |
| nested producer commit | producers are certified first, so the batch and the body agree |

---

## 5. Tests

`crates/irrigation/tests/change_notification.rs` (6 tests):

```text
unchanged_output_does_not_invalidate_consumers   reader of an unchanged unit does not run
changed_unit_still_notifies_its_reader           the changed unit's reader runs once
unchanged_component_is_reused_without_running    replay executes nothing
reset_component_reruns_when_invalidated          Reset policy still reruns
subscription_fires_only_for_observed_units       View subscription ignores sibling changes
change_batch_reports_the_moved_units             the batch names exactly the moved unit
```

`crates/irrigation-syntax/tests/json_ast_delta.rs`:

```text
value_reader_is_not_scheduled_for_unrelated_edits   AST value reader survives two unrelated edits
```

---

## 6. Verification

```text
cargo test --workspace    # 0 failures
cargo fmt --check         # runtime, irrigation, syntax
```

Observed effect on the parser pipeline: an edit re-parses locally, and a reader
of an untouched AST value unit is re-certified (`no_change_runs`) instead of
re-executed.

---

## 7. Non-goals

- No change to Tree/Map/List/Set field semantics.
- No parser or lexer publication change.
- No scheduler rewrite; the ordering guarantee is scoped to producer
  certification inside `run_general` and the reuse check.
