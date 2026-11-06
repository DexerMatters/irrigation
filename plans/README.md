# Plans

One plan per area, and every file here describes work that is still current.
Plans that were superseded were removed from the tree; their history is in git,
and the note under each surviving plan is the only claim that has to stay true.

## Current

| Plan | What it covers |
|---|---|
| [`concurrent-incremental-system-plan.md`](concurrent-incremental-system-plan.md) | The framework design: views, units, routing, suspension. This is the design the other plans slice. |
| [`ast-incremental-granularity-plan.md`](ast-incremental-granularity-plan.md) | The AST-only incremental pipeline: text → tokens → AST → metadata, locality, and the fixture that gates it. |
| [`kernel-change-notification-plan.md`](kernel-change-notification-plan.md) | A consumer reruns only when an observed unit changed. |
| [`routing-generations-plan.md`](routing-generations-plan.md) | Routing generations and unreachable-subtree retirement. |
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
