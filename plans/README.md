# Plans

One plan per area, and every file here describes work that is still current.
Plans that were superseded were removed from the tree; their history is in git,
and the note under each surviving plan is the only claim that has to stay true.

## Current

| Plan | What it covers |
|---|---|
| [`granular-concurrent-field-stores-plan.md`](granular-concurrent-field-stores-plan.md) | Delta-native field stores, per-unit commit/publication, occurrence handles, port plans, write groups, and transparent granular concurrency. |
| [`concurrent-incremental-system-plan.md`](concurrent-incremental-system-plan.md) | The framework design: views, units, routing, suspension. This is the design the other plans slice. |
| [`component-authoring-api-plan.md`](component-authoring-api-plan.md) | Public-API ergonomics: generated field handles, typed component installation, `GeneralComponent` authoring, and the reducible boilerplate behind them. |
| [`term-keyed-typing-plan.md`](term-keyed-typing-plan.md) | Scopes as bindings, per-term type/evaluation entries, ambient singleton Views, and no `TypeEnv`/type components. |
| [`component-output-publication-plan.md`](component-output-publication-plan.md) | Stage outputs: typed View patches, `IntoViewPatch`, and components that return their result instead of calling `define_*` by hand. |
| [`ast-incremental-granularity-plan.md`](ast-incremental-granularity-plan.md) | The AST-only incremental pipeline: text → tokens → AST → metadata, locality, and the fixture that gates it. |
| [`kernel-change-notification-plan.md`](kernel-change-notification-plan.md) | A consumer reruns only when an observed unit changed. |
| [`routing-generations-plan.md`](routing-generations-plan.md) | Routing generations and unreachable-subtree retirement. |
| [`shared-output-arbitration-plan.md`](shared-output-arbitration-plan.md) | Shared View writers: ordered reads, write claims, and exact-port conflict errors. |
| [`shared-view-scope-graph-plan.md`](shared-view-scope-graph-plan.md) | Applying shared writers to the reference scope graph: one View, typing edges, manifest/content transparency, and the integrated fixture. |
| [`heterogeneous-granularity-plan.md`](heterogeneous-granularity-plan.md) | The View granularity audit and fixes. |
| [`runtime-visualization-plan.md`](runtime-visualization-plan.md) | The observation path, the projection, the page, and the gates that hold them. |

## Status of a plan

A status file belongs to the plan it names and records what shipped, where the
evidence is, and where the code diverges from the written plan:

| Status | Plan |
|---|---|
| [`runtime-visualization-status.md`](runtime-visualization-status.md) | `runtime-visualization-plan.md` |

## Records

- [`implementation-status.md`](implementation-status.md) — section-by-section
  coverage against the framework plan.
- [`evidence.md`](evidence.md) — the claims that matter, mapped to the test that
  proves each one.
- [`views-components-granularity-audit.md`](views-components-granularity-audit.md)
  — the View/component granularity matrix, the four increment extents, measured
  scenarios, and the open findings.
