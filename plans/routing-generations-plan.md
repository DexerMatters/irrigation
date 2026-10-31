# Implementing routing generations

> **Status (round 33): step 1 is implemented.** `Inner::commit` retires
> observations that the new trace no longer contains, releasing their
> definitions and clearing the node. View unit nodes are never cleared. The gate
> `a_definition_inside_an_untaken_branch_is_retracted` passes. Step 2 (handover
> from a suspect owner) was attempted and reverted: the owner's suspectness is
> not the right signal because a branch node whose *condition* changed stays
> clean, while a cached provider with no dependencies can be falsely taken over.
> Deviation 7 therefore remains open; steps 2–3 below still describe the
> intended direction.

Deviations 7 and 8 both come from the same gap: a definition is owned by the
node that staged it, and ownership is only released when that node commits
again. Two consequences follow:

* a provider that stops defining a unit in the same revision another provider
  starts defining it can be diagnosed as a duplicate depending on demand order;
* a definition staged inside an untaken branch is never retracted, because the
  branch node is never demanded again.

The plan's answer is a routing generation: an immutable recipe for one region
that names its providers, published atomically and retired as a whole. The
following is the smallest version that preserves the runtime's existing
invariants.

## 1. Retire unreachable subtrees at commit

`Inner::commit` already has both traces. After replacing reverse edges, compute
the set of old observations that are absent from the new trace and call

```
retire(node):
    if node has any dependents: return          # still shared
    for each unit this node defines: retract the unit
    clear the node's value, trace, and definitions (Vacant)
    for each dependency of its old trace: retire(dependency)
```

Clearing the value matters: the retired branch must re-execute when it becomes
reachable again, which is what restages its definitions.

Retraction invalidates the unit's dependents through the existing
`invalidate` path, so observers see the change.

## 2. Allow handover from a non-current provider

In `apply_definitions`, a unit owned by another provider is a genuine conflict
only when that provider is certified for the current revision
(`verified_at == revision`). If it is suspect, it may be about to retract the
unit; allow the takeover.

`retract_unit` must then only release ownership it still holds:

```
if owners[key].node != provider: return Ok(())
```

## 3. Diagnose duplicates inside one commit

Attributing a branch's definitions to the branch node means two providers in
one body execution remain distinct owners, and step 2 may let both install.
Guard at the draft level instead: if one commit stages the same unit twice,
return `DuplicateDefinition` with both labels. That keeps the dynamic
collision gate (`a_dynamic_expansion_that_collides_is_diagnosed`) passing.

## 4. Gates to add before landing

* handover in both demand orders: provider B demanded first takes over from A,
  A's retraction does not steal the unit back;
* branch switch: a definition staged in the taken branch is retracted when the
  branch is no longer taken, and restaged when it is taken again;
* two providers defining one unit in the same execution still fail;
* the full existing suite, including the supported-composition gate.

## 5. Why this is a generation, not a hack

Steps 1–3 make the set of live definitions a function of the *current graph*
rather than of demand history: unreachable providers release their units, a
current provider is authoritative, and an ambiguous commit is rejected. That is
the observable behaviour the plan requires from routing generations. A later
optimization can replace the reachability sweep with explicit per-region
generation records without changing any of these gates.
