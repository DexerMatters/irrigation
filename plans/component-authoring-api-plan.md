# Component authoring API: less boilerplate without a second language

**Status:** implemented, with the deviations listed at the end of this file.
The phase order below is the one the workspace followed.
**Scope:** the authoring surface of `irrigation` — `#[view]`, `#[component]`,
`#[irrigation]`, `#[tree_view]`, `Runtime`, `Edit`, `Snapshot`, observers,
`GeneralComponent`, collection handles, and the macro-generated host types.
**Out of scope:** scheduler semantics, revision protocol, granularity policy,
View-family storage layouts, and the Observatory. Those are covered by the
plans listed in [`README.md`](README.md).
**Related:** [`concurrent-incremental-system-plan.md`](concurrent-incremental-system-plan.md)
§10 defines the intended public API;
[`views-components-granularity-audit.md`](views-components-granularity-audit.md)
defines the granularity boundaries this plan must preserve;
[`heterogeneous-granularity-plan.md`](heterogeneous-granularity-plan.md) owns
the family fixes.

---

## 0. Recommendation in one page

The API problem is not that the incremental model is hard to use; it is that
user code restates identities the declarations already contain. Fix that, in
this order:

1. **Fix the editor before adding convenience.** Two `edit.view` scopes in one
   transaction currently lose a write. This is a silent data-loss bug and the
   prerequisite for every edit API below (Phase 0).
2. **Generate typed field handles.** Every `#[view]` field gets a
   `ScalarField`/`MapField`/`ListField`/... constant carrying its descriptor,
   field id, and typed unit constructors. This deletes the 305 hand-written
   `FieldId` literals and the 248 hand-written `UnitKey` calls, and it is the
   foundation for typed `GeneralComponent` writes (Phase 1).
3. **Make component kind explicit and installation typed.** Add
   `#[component(root|callable)]`, let callable components take `&View`
   parameters, and return `Installed<R>` from `install_root`. This removes the
   signature heuristic and the repeated root type (Phase 2).
4. **Give `GeneralComponent` a macro and typed writers.** A function form for
   reset producers and an impl form that fills in ids/view lists; a trait
   default for `update = initialize`; typed field writers for the retained
   case (Phase 3).
5. **Add the two missing pieces of familiar Rust.** `if let` lowers exactly to
   `match`; top-level `return` lowers to a labeled block. Both preserve the
   dependency trace (Phase 4).
6. **Hygiene:** a prelude, `#[doc(hidden)]` on macro targets, a
   crate-path resolver, and built-in reductions/joins (Phase 5, partly
   interleaved).

If only three things can ship in the next release, ship Phase 0 (correctness),
the field constants for scalar and map (the two dominant families), and the
`#[general_component]` function form. Those three remove the most repeated
identity with the least new surface.

---

## 1. The question and the bar

The brief asks three things:

1. analyse the public APIs and find ways to reduce user boilerplate when
   writing components;
2. require new public APIs to be **functional** and **general**;
3. analyse the user's *potential* boilerplate and the logic that can be
   reduced rather than merely reformatted.

This plan treats "functional" and "general" as testable properties:

| Property | Test an API must pass |
|---|---|
| **Functional** | It is a value or an ordinary Rust function/closure. It composes in expressions, returns a result instead of hiding failure in a side effect, and takes its execution context (`&mut GeneralCx`, `&mut Edit`, `&Runtime`) as an argument instead of reaching for process-global state. |
| **General** | It works for every View family, for source and derived Views, for ordinary and `GeneralComponent` authors, and for root and callable components. No method may be specific to one example, one field name, or one family. |
| **Semantics-preserving** | It lowers to the same units, the same dependency trace, and the same execution counts as the long form. It must not buy ergonomics by coarsening granularity. |
| **Cost-visible** | Reads are named reads; writes are named writes; a generated constant adds no per-read allocation or lock. |
| **Additive first** | A new spelling lands next to the old one, with a migration path. Breaking changes are listed and staged. |

A plan that hides the unit model behind a second DSL would fail the first
test, so this plan deliberately adds a small, typed *view* of the existing
model, not a new model.

---

## 2. Method and evidence

The analysis was done by reading the facade, the runtime, the collection
crates, the macros, and every consumer in the workspace, then counting the
places where consumer code restates identity the framework already owns.

**Corpus.** 84,307 lines of Rust under `crates/` (tests included),
342 `#[view]` declarations, 967 declared View fields, 384 `#[component]`
functions, 235 `#[irrigation]` modules, and 95 `GeneralComponent` impls
outside the runtime crates. The proportion matters more than the absolute
number: tests and examples are written in the same style a user would write.

| Pattern in workspace code (tests included) | Occurrences |
|---|---:|
| `edit.view::<` | 590 |
| `read_outputs::<` / `read_outputs_general::<` | 436 |
| `ViewSchema>::DESCRIPTOR` | 409 |
| `field0()`, `field1()` | 307 |
| `FieldId(` | 305 |
| `UnitKey::field` / `UnitKey::entity` | 248 |
| `cx.read_typed` | 105 |
| `cx.output()` | 51 |
| `impl GeneralComponent for` | 95 |
| `impl GeneralComponent`, `update` calls `initialize` | 71 of 95 |
| View fields by family | Scalar 643, Map 163, List 95, Graph 36, Set 17, Reduced 13 |

The last row is why the design is family-general: scalar and map fields are
the common case, but a field API that only covers them would still leave a
third of the declared fields on the old path.

### Four probes

The following small experiments were run against the current tree and then
removed. They are the difference between a plausible review and a measured
one.

1. **`#[component]` works without `#[irrigation]`.** A free function with
   `#[component]` at module scope compiles and installs exactly like one
   inside the module macro. The module macro is ceremony, not a lowering
   requirement.
2. **Two editor scopes in one transaction lose a write, silently.** With
   `Numbers { values: List<u32> }`:

   ```rust
   runtime.edit(|edit| {
       edit.view::<Numbers>().values().push(1);
       edit.view::<Numbers>().values().push(2);
   })?;
   ```

   the committed list is `[2]`, not `[1, 2]`. The same code with one bound
   editor commits `[1, 2]`. Each `edit.view::<V>()` clones the committed
   content; each editor `Drop` stages a whole-content replacement; the commit
   keeps the last staged replacement for the content unit. This is a silent
   data-loss API, not an ergonomic preference.
3. **`if let` is rejected by the lowering.** A component containing
   `if let Some(value) = probe.value() { ... }` fails with
   ```text
   error: `let` expressions are not supported
   ```
   followed by a second error for the pattern binding. Users must spell the
   same program as `match` or as an `if kind == Some(...)` plus `unwrap`.
4. **Generated `define_*` methods do not publish from a `GeneralComponent`.**
   A producer whose `initialize` calls `Produced.define_value(7)` and then
   returns `()` commits no visible value; the unit-key path through
   `cx.output().field(...)` does. The typed definition methods are generated
   for ordinary components only. A `GeneralComponent` author therefore cannot
   use the typed macros they already have; they must drop to
   `FieldId`/`UnitKey`.

These four probes map directly to four findings below.

**Verdict.** Yes, the boilerplate can be reduced substantially, but not by
adding another abstraction over the runtime. The reducible part is *identity
and scaffolding the declarations already determine*: field ids, unit kinds,
component ids, view lists, root-vs-callable intent, and the reset `update`
body. The algorithm bodies (the lexer, the parser, the resolver) are not
reducible and should not be hidden; they are exactly what a
`GeneralComponent` exists for. The plan removes the first and leaves the
second in plain Rust.

---

## 3. The public surface today

The facade re-exports 167 names by a rough count. They are not one audience.

| Tier | Examples | Intended user |
|---|---|---|
| **Authoring** | `view`, `component`, `tree_view`, `Runtime`, `Edit`, `Snapshot`, `Subscription`, `ViewSchema`, `GeneralComponent`, `GeneralCx`, `ChangeBatch` | everyone |
| **Extension** | `RootComponent`, `ComponentSpec`, `Reduction`, `JoinSpec`, `Lattice`, `FixedPointProblem`, `PortProvider`, `RouteSelector`, `FieldDescriptor`, `ViewDescriptor` | advanced users and macro output |
| **Kernel plumbing** | `UnitKey`, `UnitKind`, `FieldId`, `ViewId`, `NodeId`, `Value`, `value_of`, `define_unit`, `ensure_view`, `observe_clone`, `read_unit_value`, `eval`, `scope`, `node_of` | generated code and the Observatory |

The boilerplate problem is mostly the third tier leaking into the first and
second. A user writing a `GeneralComponent` today must know that a scalar is
`UnitKey::field(view_id, FieldId(0), UnitKind::Value)`, that a map entry's
presence is a separate `UnitKind::Membership`, that a field's structure is a
third unit, and that `ensure_view` must be called before a definition or the
write group will not declare the View. All of that is derivable from the
`#[view]` declaration. Repeating it is not a feature; it is a missing API.

The current authoring flow is:

```text
#[view]                      field names and kinds
  │
  ├── generated: handle, content, descriptor, snapshot, editor
  │
#[component]                 function signature decides root vs callable
  │
  ├── generated: __impl, Args, Spec or Root marker
  │
Runtime::install             one root at a time
  │
edit(|edit| edit.view::<V>())      view-level editors, snapshot semantics
  │
read() / snapshot()          runtime snapshot
  │
snapshot.view::<V>()         typed snapshot handle
  │
observe::<V>(|snapshot|)     untyped callback, live handle reads
```

The friction points are at every arrow. The next section names them.

---

## 4. Findings

Summary first. IDs are stable for discussion; severity is about user pain and
silent wrongness, not implementation size.

| ID | Finding | Severity | Reducible |
|---|---|---|---|
| F1 | Field identity (`FieldId`, `UnitKey`, `ensure_view`) is hand-written and positional | high | yes, fully |
| F2 | Tuple View fields force `field0()`; single-field forwarding is partial | medium | yes |
| F3 | `List`/`Set` are not real types, although `#[view]` matches their names | medium | yes |
| F4 | Generated content/vtable internals are public and undocumented | low | partly |
| F5 | Root vs callable is inferred from parameter types; zero-arg helpers become roots | high | yes |
| F6 | `#[irrigation]` is redundant and generates no registry | low | yes |
| F7 | Host-callable invocation types are named `__FooArgs`/`__FooSpec` | medium | yes |
| F8 | No typed root handle; every read repeats the install type | medium | yes |
| F9 | Projection indices are magic numbers | low | partly |
| F10 | Generated paths hardcode `irrigation::`, so a renamed dependency breaks | medium | yes |
| F11 | Two editor scopes in one transaction silently lose writes | **critical** | yes |
| F12 | Editing requires nested `let mut`; scalar and collection accessors differ | medium | partly |
| F13 | No typed staged read for an editor | low | partly |
| F14 | `GeneralComponent` requires 8 items and 71/95 impls duplicate `update = initialize` | high | yes |
| F15 | Typed/local/async component traits multiply the manual surface | medium | partly |
| F16 | General definitions are unit-key only; generated typed defines do not publish there | high | yes |
| F17 | `EquiJoin` needs a bespoke `JoinSpec`, a fixed `ComponentId`, and raw field ids | medium | yes |
| F18 | Every reduction is hand-written | low | yes |
| F19 | `if let` and top-level `return` are missing from the component subset | medium-high | yes |
| F20 | Opaque helper calls are silently one coarse node; granularity is not declared | medium | partly |
| F21 | Recursive context arguments clone/hash large values; `Context` needs raw unit keys | medium | yes |
| F22 | Observers require a second downcast and raw `FieldId`s for entry granularity | medium | yes |
| F23 | `snapshot()` vs `read()` and `Snapshot` vs `V::Snapshot` are easy to confuse | low | partly |
| F24 | No prelude and no public/private split in the docs | low | yes |

### F1 — Field identity is restated by hand

**Evidence.** `crates/irrigation-syntax/src/views.rs:233-253` declares eight
`AstMetaField` constants by hand; `components/parser.rs:28-112` turns every
one into a `UnitKey` constructor; `components/lexer.rs:20-90` does the same
for `Tokens`; `components/text.rs:13-39` builds `SourceFile` and `Text` units
inline. The syntax and observe crates alone contain 52 `FieldId(` literals and
34 `UnitKey` constructors; the rest of the workspace adds 253 and 214 more,
mostly test probes.

**Effect.** A field declaration and its identity live in two places. Inserting
a field silently renumbers every later field, and every hand-written
constructor/observer follows the old number. The failure is not a compile
error; it is a read of the wrong unit, or a definition the runtime denies
because `ensure_view` was forgotten.

**Reducible logic.** The `#[view]` macro already knows the field's name, index,
family, and storage. It can emit the identity once. Nothing in the user code
needs to know `FieldId(5)`.

**Direction.** Generate one typed `Field` constant per declared field; expose
`unit()`, `structure_unit()`, `entry(key)`, typed reads, typed writes, and
observations from it. See §5.2.

### F2 — Tuple fields force `field0()`

**Evidence.** The README quick start reads
`snapshot.view::<Squares>().field0()` (`README.md:54`); the workspace has 307
`field0()`/`field1()` calls. `crates/irrigation-macros/src/view.rs:948` names
an unnamed field `field0`; `single_field_forwarding`
(`view.rs:1013-1347`) adds `get`/`len`/`define` only for a *single unnamed*
field. A named single field gets none of that forwarding.

**Effect.** The shortest declaration (`struct Squares(Map<..>);`) produces the
least readable reads. Named single-field Views and tuple Views do not offer
the same helpers. `field0` also appears in diagnostics and docs, so the
awkward name is defended as if it were intentional.

**Direction.** Allow `#[field(name = "...")]` on View fields; generate a
`value()` alias for any single-field View (named or unnamed); apply
single-field forwarding to named and unnamed fields alike, with collision
diagnostics. Keep `field0()` for compatibility. See §5.2 and §5.3.

### F3 — `List` and `Set` are macro syntax, not exported types

**Evidence.** `#[view]` recognizes field shapes by the unqualified last path
segment (`view.rs:960-987`). `Map`, `Graph`, and `Reduced` happen to exist as
real marker types (`map.rs:29`, `graph.rs:86`, `reduce.rs:46`), but `List` and
`Set` do not; only `SourceList`/`DerivedList` and `SourceSet`/`DerivedSet`
exist (`list.rs:29`, `set.rs:22`). A View that writes `numbers: List<u32>` is
valid because the macro consumes the tokens and never emits the type. The type
does not resolve in any other context.

**Effect.** IDE and documentation tooling see an unresolved type inside a
macro input. A user type named `Map<K, V>` is silently treated as a map field.
The View declaration does not describe itself in the language it is written
in.

**Direction.** Export real `List<T>` and `Set<T>` marker types (or recognize
the existing source/derived markers), document that field types are markers,
and keep name-based recognition only as a fallback. Reject a shape whose path
is a user type in a future major. See §5.2.

### F4 — Generated internals are public

**Evidence.** `#[view]` generates `pub struct NameContent` and a public
`NameSnapshot`/`NameEditor` at the declared visibility. The vtable functions
are free items in the user's module. No item is `#[doc(hidden)]`.

**Effect.** The docs present storage and vtable internals as API. Kernel tests
reach for `ConsumerTableContent` because it is the only way to stage a raw
replacement. Users must learn the content type to do something the framework
could expose as a typed operation.

**Direction.** Keep the items public for generated code and tests, mark them
`#[doc(hidden)]`, and add the one missing typed operation (`Edit` staged read)
if it earns its place. See §5.8.

### F5 — Root vs callable is a signature heuristic

**Evidence.** `crates/irrigation-macros/src/component.rs:90` says a component
is a root when *all* parameters are `&View` references. Consequences:

* a parameterless helper is vacuously a root;
* `fn total(numbers: &Numbers, strict: bool) -> u32` is rejected: a component
  with some references and some owned values is neither root nor legal
  callable (`component.rs:91-101`), and `RootComponent::run` has no way to take
  an install-time argument even if it were made a root;
* `fn check(expr: ExprRef, options: &Options) -> ...` hits the same rejection
  for the same reason;
* a `&View`-taking helper cannot be called as an interned nested component; it
  runs in a child scope of the caller instead.

**Effect.** The same signature shape decides installation, call identity, and
whether the function is legal at all. Moving a parameter in or out of a View
reference silently changes the kind of component. Users learn this by
compiler error.

**Direction.** Make the kind explicit (`#[component(root)]` /
`#[component(callable)]`), keep the old heuristic as a deprecated fallback for
one release, and let callable components take `&View` parameters by storing the
owned handle in `Args`. See §5.4.

### F6 — The module macro is ceremony

**Evidence.** `expand_module` (`component.rs:36-77`) iterates the module and
expands each `#[component]` item; it adds no code. Probe 1 confirms a direct
`#[component]` compiles and installs. 235 modules in the workspace carry the
attribute.

**Effect.** One attribute line, one import (`irrigation`), and one nesting
level per component group, for no generated behavior. The module attribute
also hides the fact that it is optional, so users believe components must live
inside it.

**Direction.** Document direct `#[component]` as the normal form and keep
`#[irrigation]` as a deprecated alias. Do not generate a registry from it; a
module can contain roots, callable components, and `GeneralComponent`s, so a
single `install` method would be ambiguous. See §5.4.

### F7 — Host-callable types are named like internals

**Evidence.** `component.rs:85-87` generates `__{Pascal}Args` and
`__{Pascal}Spec`. Host code must name them to call a non-root component:

```rust
runtime.call::<yielding::__SlowOnceSpec>(yielding::__SlowOnceArgs { tag: 7 })?;
```

(see `crates/irrigation/tests/suspension.rs:227` and
`crates/irrigation/tests/general_producer.rs:465`).

**Effect.** A supported operation — call one component from the host — has a
name that promises it is private. The public function `count(expr)` cannot be
used outside a computation frame, so the underscored spec is the only route.

**Direction.** Generate `{Pascal}Args` and `{Pascal}Spec` as the public names
and keep `__` aliases for compatibility. Add `Installed`/`InstalledGeneral`
handles so hosts do not need the spec for the common root/producer case. See
§5.4.

### F8 — Every read repeats the install type

**Evidence.** `runtime.install(analysis::Analyze)` returns `Result<()>`, and
every later read says `runtime.read_outputs::<analysis::Analyze>()`
(`runtime.rs:112,558`). The type is already known at install.

**Effect.** The root type appears at every call site; a mismatch is a runtime
error ("root output type does not match") rather than a type error. The
`Installed` value that install could return is missing.

**Direction.** Add `Runtime::install_root` returning `Installed<R>` with
`read`, `projection`, `runtime`, and `revision`; `install` delegates and drops
the handle. Add the analogous general-component handles in Phase 3. See §5.4.

### F9 — Projection indices are magic

**Evidence.** `read_projection::<R>(index)` (`runtime.rs:621`) takes a bare
`usize`. A multi-output component returns a tuple; nothing names the members.

**Effect.** `read_projection::<R>(0)` is a positional protocol with no
compiler help. Low frequency, but it is part of the same "restated identity"
family.

**Direction.** Generate `OUTPUT_<N>` constants next to a root marker and a
typed `Installed::projection` that accepts them. Do not invent names for tuple
members the user did not write. See §5.4.

### F10 — Macros hardcode `irrigation::`

**Evidence.** 213 `irrigation::` paths in `irrigation-macros/src/*.rs`. The
facade has `extern crate self as irrigation`, but an external crate that
renames the dependency (`irr = { package = "irrigation" }`) gets generated
paths that do not resolve.

**Effect.** The macros only work when the dependency is named exactly
`irrigation`. This is a general portability defect, not a style issue.

**Direction.** Resolve the crate name with `proc-macro-crate` once per
expansion and emit that path. Add a fixture crate whose dependency key is
renamed. See §5.8.

### F11 — Two editor scopes lose writes

This is proven by probe 2 and is the only critical finding.

**Evidence.** `Edit::view` (`edit.rs:68`) builds an editor from
`edit.content::<Content>(view)` (`edit.rs:140`), which reads the committed
region content, not content staged earlier in the same transaction. Each
generated editor's `Drop` stages a whole-content replacement
(`view.rs:887-896`). `Edit::commit` normalizes by key and keeps the last staged
update (`edit.rs:176-181`), so the second editor's snapshot replaces the
first's.

**Effect.** The natural shorthand

```rust
runtime.edit(|edit| {
    edit.view::<Numbers>().values().push(1);
    edit.view::<Numbers>().values().push(2);
})?;
```

commits `[2]`. It compiles, it does not warn, and the lost write is invisible
unless the user asserts the result. Any convenience API that encourages one
editor per statement makes this worse.

**Direction.** Add `Edit::staged_content(view)` that overlays staged content
replacement and unit writes on committed content; generated editors use it.
Add the regression gate. This fix is a prerequisite for every editing
convenience in this plan. See §5.3 and Phase 0.

### F12 — Editing is a nested ceremony

**Evidence.** The common shape is

```rust
runtime.edit(|edit| {
    let mut inputs = edit.view::<Inputs>();
    let mut numbers = inputs.numbers();
    numbers.push(1);
})?;
```

Scalar fields use `set_<name>`; collections use `<name>()` returning an editor;
the view editor must stay alive for both. The `let mut` on the view editor is
load-bearing and is easy to omit in the shorthand form (see F11).

**Effect.** Boilerplate with a semantic trap. The `let mut` is not a style
choice; it is the difference between committing and losing writes.

**Direction.** Fix F11 first. Then add `runtime.edit_view::<V>(|editor| ...)`
for the single-View case and `Edit::with_view::<V>(|editor| ...)` for a
multi-write transaction; both make the lifetime explicit and remove the
`let mut view` binding. Keep `edit.view` as the low-level form. See §5.3.

### F13 — No typed staged read

**Evidence.** `Edit::content::<C>(view)` (`edit.rs:140`) requires the
generated content type `C`, which is public but undocumented. Custom edits that
need the current state before deciding a delta use `edit.content` or raw
`current` on a unit.

**Effect.** Toggling or patching without an editor requires knowing the
generated content type; the rest of the API is typed by View.

**Direction.** Add `Edit::staged_view_content<V: ViewSchema>` or a typed
`Snapshot`-like read owned by the edit transaction. This is lower priority than
F11 and can land with the `Edit` convenience methods.

### F14 — `GeneralComponent` is an eight-item trait

**Evidence.** The trait (`general.rs:591-634`) requires `State`, `Output`,
`ID`, `NAME`, `VIEWS`, `OUTPUT_VIEWS`, `initialize`, and `update` — eight
items. 71 of 95
consumer impls write `update` as `self.initialize(cx)`; the duplicate is
visible in `components/text.rs:39`, `components/lexer.rs:324`, and the tests.

**Effect.** Every producer pays a fixed 15-25 lines of ceremony even when its
policy is the default `Reset`. The `ID` constants are hand-allocated
(`0x00B2_0000`, `0x00B2_0001`, ...), which is an invitation to collide.

**Direction.** Add `#[general_component]` in two forms: an inherent-`run`
function form for reset producers, and a trait-impl form that fills in
`ID`/`NAME`/`VIEWS`/`OUTPUT_VIEWS` and defaults `update` to `initialize`. See
§5.5.

### F15 — Four component traits, three adapters, four install methods

**Evidence.** `GeneralComponent`, `TypedGeneralComponent` (general.rs:652),
`LocalGeneralComponent` (general.rs:772), and `AsyncGeneralComponent`
(general.rs:1262); `TypedAdapter`, `LocalAdapter`, `AsyncAdapter`; and
`install_general`, `install_typed_general`, `install_local_general`,
`install_general_component`. `TypedGeneralComponent` has no production user;
it appears only in the test suite.

**Effect.** A user choosing a component form must read four trait
definitions and four install methods. The typed variant also duplicates
`initialize`/`update` signatures just to re-read inputs.

**Direction.** Make the typed field handles the common way to read inputs, make
`GeneralComponent` the one extension trait, and treat the typed/local/async
forms as adapter details. Do not delete them in this plan; stop presenting them
as peer choices. See §5.5.

### F16 — General definitions are unit-key only

**Evidence.** Probe 4: `Produced.define_value(7)` inside `initialize` commits
nothing, while `cx.output().field(<Produced>::view_id(), FieldId(0), value)`
works. `OutputTxn` (`general.rs:515-580`) exposes `field`, `entry`,
`membership`, `retract`, and `replace_all`, all in raw ids. The generated
`define_*` methods in `view.rs:745-816` use ambient `define_unit`, which is a
different definition path.

**Effect.** A `GeneralComponent` author cannot use the typed macros they
already have. The parser and lexer components are built from dozens of
`UnitKey::entity(..., FieldId(5), UnitKind::Value, any_key(id))` calls. The
two definition paths have different authorization and commit behavior, and
the user must know which one applies.

**Direction.** Add typed field writers that call `cx.output()` and carry the
View descriptor, so `cx.write(Tokens::PAYLOADS).entry(id).set(payload)` works
from a `GeneralComponent`. Make `ensure_view` automatic. Keep the ambient
`define_*` methods for ordinary components and document the split. See §5.2
and §5.5.

### F17 — `EquiJoin` is a type-level ceremony with a fixed identity

**Evidence.** `JoinSpec` (`join.rs:35-60`) requires four associated types,
`left()`/`right()` returning `(ViewId, FieldId)`, `combine`, and an optional
`target`. `EquiJoin<S>` uses one constant `ComponentId`
(`join.rs:271`), so two different joins cannot be installed on one runtime:
`install_general_component` rejects the duplicate id. The join re-reads
membership/value units by hand (`join.rs:84-123`).

**Effect.** A user must declare a new type for every join and write raw field
ids; the second join fails at install time. The API is general in principle,
but not in use.

**Direction.** Make `JoinSpec` field-typed (`MapField<K, V>`), add a
closure-based constructor, and let the component identity be derived from the
field pair (or let the user name the join type) so multiple joins coexist.
See §5.7.

### F18 — Reductions are hand-written

**Evidence.** `demo.rs:39-57` defines a `Sum` type with four methods just to
sum `u64`s. `Reduction` (`reduce.rs:25-39`) is a good algebra, but the common
algebras are missing.

**Effect.** Ten to fifteen lines per sum, count, min, max, or union.

**Direction.** Export `reductions::{Count, Sum, Max, Min, Union}` with lawful
integer/general implementations. Do not add a closure-based reduction; the
trait requires an explicit lawful algebra.

### F19 — `if let` and `return` are missing

**Evidence.** `lower.rs:467-479` lowers `if` by lowering the condition as an
expression; `lower.rs:587` rejects `Expr::Let`, so `if let` fails (probe 3).
`lower.rs:568-572` rejects `return`; the README documents both as deliberate
boundaries (`README.md:205-208`). Users write nested `match` and inverted
conditions; `name_resolver.rs:54-83` is the shape.

**Effect.** Familiar Rust control flow is unavailable for reasons unrelated
to incrementality. `if let` is pure control flow and has an exact `match`
lowering; `return` is a control-flow convenience whose dependencies are still
recorded. Rejecting them increases boilerplate without protecting the model.

**Direction.** Lower `if let` to a `match` with the same observation point;
lower top-level `return` to a labeled block. Keep loops, `let mut`, effects,
and `let else` rejected. See §5.6.

### F20 — Opaque calls choose granularity silently

**Evidence.** A call to a free function is lowered to one `eval` node
(`lower.rs:406-421`). The function body is not lowered, so its reads are
recorded but its internal computations are not separately memoized. This is
the documented permissive mode (`docs/architecture.md:325-349`), and the STLC
pipeline depends on it: `publish_terms`, `publish_names`,
`publish_typing`, and `publish_evaluation` are plain functions called from
components (`examples/stlc_scope_graphs/main.rs:151-241`).
The demo does the same with `rules()`, `payloads()`, and `bound_names()`
(`examples/observatory/demo.rs:157-205`).

**Effect.** A one-line helper can hide an entire algorithm behind one node.
The dependency trace is still correct, but a payload edit re-executes the whole
helper; the user sees "why did my parser rerun?" The `strict` mode exists but
is opt-in and rejects every undeclared call, so users learn to turn it off.

**Direction.** Make the choice explicit at the call site: `#[component]` for a
lowered/interned helper, `#[component(opaque)]` (or a documented `unsafe`
analogue, e.g. `PlainFn`) for a deliberately coarse call. Do not turn strict
on by default in this plan; add the vocabulary first. See §5.6.

### F21 — Recursive context arguments are expensive and ad hoc

**Evidence.** `Context<K, V>` exists (`context.rs:1-78`) precisely to separate
invocation identity from the observed value, but it requires a raw `UnitKey`.
The only in-tree use constructs it by hand
(`crates/irrigation/tests/gates.rs:6670-6682`). Recursive components such as
`unresolved(expr, defined: Vec<String>)` clone the whole vector into every
child call and hash it into the invocation key
(`examples/name_resolver.rs:54-71`).

**Effect.** Large environments make invocation keys large, fragment
memoization, and clone on every branch. The safe alternative exists but costs
more boilerplate than the unsafe one, so it is not used.

**Direction.** Give `Context` constructors from typed field entries
(`field.entry(key).context()`), so the invocation key is a small identity and
the value is a tracked read. See §5.6.

### F22 — Observers are untyped at the call site

**Evidence.** `observe::<V>(|snapshot| ...)` hands the callback a `&Snapshot`;
the user must call `snapshot.view::<V>()` again inside the callback
(`runtime.rs:692`). For entry granularity, `observe_entry::<V, K>` takes a
`FieldId` (`runtime.rs:787`), so the call site restates the field position.

**Effect.** The View type is written twice and the second one can be the wrong
one (a runtime panic, not a type error). The entry observer cannot be written
from a View handle because the handle exposes no typed unit constructor.

**Direction.** Add `observe_view::<V>(|view: V::Snapshot<'_>| ...)` and
`observe_view_with_changes`; add `Field::observe` / `entry.observe` so the
field identity comes from the declaration. See §5.3.

### F23 — Two snapshot APIs and two Snapshot names

**Evidence.** `Runtime::snapshot()` captures committed View contents without
demanding the root; `Runtime::read()` demands the root; `inspect_snapshot()`
creates no demand task (`runtime.rs:327-351,478-508`). A general producer is
lazy, so `snapshot()` after an edit can show zeros until a consumer reads an
output View (probe 4 shows this shape). Separately, the generated
`V::Snapshot<'a>` has the same conceptual name as `runtime::Snapshot`.

**Effect.** A host that wants "the current results" can silently read stale
content. The documentation says "without demanding a root" but the method name
does not.

**Direction.** Keep the methods (they serve different diagnostics), document
the distinction prominently, and prefer `read()` in examples. Rename the
generated handle to `V::View<'a>`? That is a breaking rename across every
generated type; defer to 1.0. Add a `read_view::<V>()` convenience only if it
can return an owned snapshot without a lifetime trap.

### F24 — No prelude and no public/private split

**Evidence.** The facade (`crates/irrigation/src/lib.rs`) re-exports kernel
functions next to authoring types. The README quick start imports four items;
`GeneralComponent` authors need a dozen. Nothing marks macro-target functions
as implementation details.

**Effect.** Documentation surface is flat. Users discover `define_unit` and
`observe_clone` before they discover `Field`/`Installed` because both are at
the same level.

**Direction.** Add `irrigation::prelude`, mark macro targets
`#[doc(hidden)]`, and describe the three tiers in the crate docs. See §5.8.

---

## 5. Proposed API

The design has one organizing idea:

> **Derive, don't restate.** Every runtime identity that can be derived from a
> declaration is emitted by the macro that sees the declaration. User code
> names *fields*, *views*, and *components*; it does not name field numbers,
> unit kinds, or scope ids.

The APIs below are additive. The only behavioral change is the F11 fix. The
Rust blocks are signature sketches, not compiled examples; where a signature
needs a refinement (a lifetime, an associated type), the implementation note
says so.

### 5.1 The typed field handle

Add one generic wrapper in `irrigation-collections::field` (the facade
re-exports it as `irrigation::fields`). It cannot live in
`irrigation-runtime`: the runtime cannot name `MapRef` or the other family
handles, because the collections crate depends on the runtime, not the other
way around. The wrapper belongs where the handles live; the runtime keeps the
`ScalarRef` and the field traits it already owns.

```rust
/// One declared View field: its descriptor, its position, and a typed handle.
pub struct Field<H> {
    descriptor: &'static ViewDescriptor,
    id: FieldId,
    handle: H,
}

impl<H> Field<H> {
    pub const fn new(
        descriptor: &'static ViewDescriptor,
        id: FieldId,
        handle: H,
    ) -> Self;

    pub fn view(&self) -> ViewId;              // descriptor.id
    pub fn id(&self) -> FieldId;
    pub fn descriptor(&self) -> &'static ViewDescriptor;

    /// The entity-free `Value` unit of the field. Scalar fields use this as
    /// their only unit; collection fields use it for a whole-field definition.
    pub fn unit(&self) -> UnitKey;

    /// The field's structure projection unit, for families that have one.
    pub fn structure_unit(&self) -> UnitKey;
}
```

`Field<H>` derefs to `H`, so the existing family methods (`get`, `to_vec`,
`keys`, `nodes`, `aggregate`, ...) remain available. The wrapper adds the
missing context and identity methods; it does not replace the collection
handle types.

Family aliases keep signatures readable:

```rust
pub type ScalarField<T>      = Field<ScalarRef<T>>;
pub type MapField<K, V>      = Field<MapRef<K, V>>;
pub type SourceListField<T>  = Field<SourceListRef<T>>;
pub type DerivedListField<T> = Field<DerivedListRef<T>>;
pub type SourceSetField<T>   = Field<SourceSetRef<T>>;
pub type DerivedSetField<T>  = Field<DerivedSetRef<T>>;
pub type GraphField<N, E>    = Field<GraphRef<N, E>>;
pub type ReducedField<R>     = Field<ReducedRef<R>>;
```

The existing `*Ref::new` constructors become `const fn` (they already store
only `ViewId`, `FieldId`, and a function pointer), and the handle types derive
`Copy`. That is what lets a field be an associated constant. The facade
re-exports this module as `irrigation::fields`.

### 5.2 What `#[view]` emits

For every declared field, in addition to today's accessors, `#[view]`
generates:

1. a private content function returning the field's storage;
2. a public associated constant with the family's typed handle;
3. a field-level doc comment (so `#![warn(missing_docs)]` users do not get a
   warning from code they did not write).

For `Tokens` (the syntax crate's real declaration, abbreviated):

```rust
#[view(derived)]
pub struct Tokens {
    pub text: TextDocument,
    pub order: List<TokenId>,
    pub delta: TokenDelta,
    pub diagnostics: Map<DiagnosticId, Diagnostic>,
    pub payloads: Map<TokenId, Arc<TokenPayload>>,
    // ...
}
```

the macro emits, conceptually:

```rust
impl Tokens {
    pub const TEXT: ScalarField<TextDocument> =
        Field::new(<Self as ViewSchema>::DESCRIPTOR, FieldId(0), ScalarRef::new(...));
    pub const ORDER: DerivedListField<TokenId> =
        Field::new(<Self as ViewSchema>::DESCRIPTOR, FieldId(1), DerivedListRef::new(...));
    pub const DIAGNOSTICS: MapField<DiagnosticId, Diagnostic> =
        Field::new(<Self as ViewSchema>::DESCRIPTOR, FieldId(3), MapRef::new(...));
    pub const PAYLOADS: MapField<TokenId, Arc<TokenPayload>> =
        Field::new(<Self as ViewSchema>::DESCRIPTOR, FieldId(5), MapRef::new(...));
}
```

The constants are `Copy`, `'static`, and usable in `const` context. The
private content functions are monomorphic per field; the macro already emits
the equivalent closure in each live accessor (`view.rs:573-608`), so this is a
refactor of generated code, not new machinery.

The descriptor order stays authoritative. Generated constants and hand-written
code agree by construction, which removes F1. The syntax crate's
`AstMetaField` constants and its nine `UnitKey` constructors are deleted in
the migration; the field constants replace them.

Field-name metadata (F2/F3) is part of the same emission:

```rust
#[view(derived)]
pub struct Squares(#[field(name = "entries")] Map<u32, u32>);
```

* `#[field(name = ...)]` names a tuple field (the accessor and the constant,
  `ENTRIES`); on a named field it is an error. Without it an unnamed field is
  `field0`/`FIELD0`.
* A single-field View also gets `value()` (live and snapshot) unless the field
  is literally named `value`.
* Single-field forwarding (`get`, `len`, `from`, ...) applies to named and
  unnamed fields alike.
* `List<T>` and `Set<T>` become real exported markers so a View declaration is
  valid Rust outside the macro. `Map`, `Graph`, and `Reduced` are already real.
* The constant's visibility follows the field's visibility, and generated docs
  name the field, so a private field does not gain a public accessor.

Tree Views are declared by `#[tree_view]`, not `#[view]`, and already expose
typed node refs (`ExprRef`, `ExprKind`, per-variant accessors) instead of a
`FieldId` surface. They keep that API. A `TreeField` wrapper would add a second
handle family for no identity removal; it is explicitly not part of Phase 1.

### 5.3 The four contexts, one vocabulary

A field is read or written in one of four contexts. Each context gets exactly
one spelling; there is no ambient "current transaction" API.

| Context | Read | Write |
|---|---|---|
| Component body (tracked, ambient frame) | `Tokens::TEXT.read()`, `Tokens::PAYLOADS.get(&id)` | `Tokens.define_payloads(id, payload)` (existing generated method) |
| `GeneralComponent` | `Tokens::TEXT.read_in(cx)?`, `Tokens::PAYLOADS.get_in(cx, &id)?` | `cx.write(Tokens::TEXT).set(value)`, `cx.write(Tokens::PAYLOADS).entry(id).set(value)` |
| Edit transaction | `edit.view::<SourceFile>()` (typed editor) | `runtime.edit_view::<SourceFile>(|view| ...)` |
| Observer | `observe_view::<V>(|view| ...)` | `Tokens::PAYLOADS.entry(id).observe(&runtime, cb)` |

The naming rule is: a bare verb is a tracked read in the ambient component
frame; `_in(cx)` is an explicit `GeneralCx` operation; `write(...).entry(...)`
is an explicit output transaction. That is the "functional" part: the context
is an argument, not a hidden global.

#### Reading

The aliases are for signatures and constants. An inherent impl must name the
underlying `Field<...>` type; Rust does not allow inherent impls on a type
alias. The sketches below do that.

```rust
impl<T: Clone + PartialEq + Send + Sync + 'static> Field<ScalarRef<T>> {
    pub fn read(&self) -> T;                                  // tracked
    pub fn read_in(&self, cx: &mut GeneralCx) -> Result<T>;   // explicit
}

impl<K, V> Field<MapRef<K, V>> {
    pub fn entry(&self, key: K) -> MapEntry<K, V>;
    pub fn get(&self, key: &K) -> Option<V>;                  // tracked
    pub fn get_in(&self, cx: &mut GeneralCx, key: &K) -> Result<Option<V>>;
    pub fn keys(&self) -> Vec<K>;                             // tracked structure
    pub fn entries_in(&self, cx: &mut GeneralCx) -> Result<Vec<(K, V)>>;
}

impl<T> Field<SourceListRef<T>> {
    pub fn ids(&self) -> PSeq<u64>;
    pub fn get(&self, index: usize) -> Option<T>;
    pub fn to_vec(&self) -> Vec<T>;
    pub fn map<U, F>(&self, f: F) -> Vec<U> where F: Fn(Param<T>) -> U + Send + Sync;
    pub fn fold<A, F>(&self, init: A, f: F) -> A where F: Fn(A, Param<T>) -> A;
}
```

`fold` is new: it replaces `to_vec().iter().fold(...)` and keeps the reads
tracked. It is a whole-node fold, not a per-member incremental operator; the
docs say so. `map`/`filter`/`flat_map` keep their per-member memoization.

#### Writing from a `GeneralComponent`

```rust
impl<T> Field<ScalarRef<T>> {
    pub fn write<'a>(&self, cx: &'a mut GeneralCx) -> ScalarWriter<'a, T>;
}

impl<K, V> Field<MapRef<K, V>> {
    pub fn write<'a>(&self, cx: &'a mut GeneralCx) -> MapWriter<'a, K, V>;
}

pub struct MapWriter<'a, K, V> { /* cx + field */ }

impl<K, V> MapWriter<'_, K, V> {
    pub fn entry(&mut self, key: K) -> MapEntryWriter<'_, K, V>;
    pub fn define_all(&mut self, entries: impl IntoIterator<Item = (K, V)>);
}

impl<K, V> MapEntryWriter<'_, K, V> {
    pub fn set(&mut self, value: V);        // value + membership true
    pub fn present(&mut self, present: bool);
    pub fn retract(&mut self);              // membership false + value retracted
}
```

The writer calls `ensure_view(self.descriptor)` and
`cx.output().define(...)` internally. That fixes F16 without changing the
commit semantics: the definitions still stage through the dispatcher-issued
output transaction, and `OUTPUT_VIEWS` authorization still applies.

`write` is available on every field handle, but authorization is the runtime's:
writing a source field or a View outside the component's `writes(...)` list
fails at commit with the existing definition error. A type-state split
(`Field` for reads, `DerivedField` for writes) would make the mistake a compile
error; it is a worthwhile follow-up but not required for the first pass.

The families follow the same shape:

* the list handle: `field.write(cx).define_all(items)` or `.define_keyed(items)`;
* the set handle: `field.write(cx).member(value).present(true/false)`;
* the graph handle: `field.write(cx).node(id).set(payload)`,
  `.edge(id).set(from, to, payload)`, `.edge(id).remove()`;
* the reduced handle: `field.write(cx).define_all(items)`.

A whole-field write claims `WriteScope::Field(view, field)` before staging the
storage value, matching `DerivedListRef::define_all` and the other collection
writers. Per-entry writes do not claim the whole field; they claim the entry's
port through the output transaction. The two paths must not be mixed in one
output group without the existing claim rules.

#### Source edits

F11 must be fixed first. Concretely:

* add `Edit::staged_content<C>(view) -> Arc<C>` that overlays staged unit writes
  and content replacements on the committed region content;
* generate `EditableView::edit` with `staged_content`, not `content`;
* keep `Edit::content` as the committed-content accessor for diagnostics and
  raw replacements;
* add the regression test from probe 2.

Then add two convenience forms:

```rust
impl Runtime {
    /// One transaction, one View editor. The editor lives for the closure.
    pub fn edit_view<V: EditableView, T>(
        &self,
        f: impl FnOnce(&mut V::Editor<'_>) -> T,
    ) -> Result<T>;
}

impl Edit {
    /// One editor for part of a multi-View transaction.
    pub fn with_view<V: EditableView, T>(
        &mut self,
        f: impl FnOnce(&mut V::Editor<'_>) -> T,
    ) -> T;
}
```

`with_view` calls `V::edit(self)` and passes the editor by reference; the drop
happens after the closure. Two `with_view` calls on one `Edit` compose because
of the `staged_content` fix. This removes the load-bearing `let mut` binding
without introducing a second transaction.

### 5.4 Components and installation

Declare the kind explicitly:

```rust
#[component]                       // legacy rule: root when every parameter is &View,
                                   // callable otherwise
#[component(callable)]             // force a callable component
#[component(root)]                 // installable root; every parameter must be &View
#[component(strict, allows = [..])]
```

The legacy implicit root rule stays for one release: a bare `#[component]`
whose parameters are all `&View` still generates a root marker, so existing
code keeps compiling. New code and docs use `root`/`callable`; a root may not
take an owned parameter until the root-parameter design in R4 lands.

A callable component may take `&View` parameters. The macro stores the owned,
`Copy` handle in `Args` and passes a reference to the body:

```rust
#[component(callable)]
pub fn check(expr: ExprRef, options: &Options, strict: bool) -> Vec<Diagnostic> { ... }

// generated, conceptually
pub struct CheckArgs {
    pub expr: ExprRef,
    pub options: Options,   // owned ZST, Hash + Eq
    pub strict: bool,
}
pub struct Check;
impl ComponentSpec for Check { type Args = CheckArgs; ... }
pub fn check(expr: ExprRef, options: &Options, strict: bool) -> Vec<Diagnostic> {
    call_component::<Check>(CheckArgs { expr, options: *options, strict })
}
```

This removes the mixed-parameter rejection at `component.rs:91-101`.
`#[view]` handles already derive `Clone, Copy, Debug, Default, PartialEq, Eq`;
add `Hash` so they can be interned.

Generated invocation types lose the underscore prefix:

```rust
pub struct CountArgs { ... }
pub struct Count;                 // the ComponentSpec
#[doc(hidden)] pub type __CountArgs = CountArgs;   // compatibility alias
#[doc(hidden)] pub type __CountSpec = Count;
```

Hosts can call a component without reading a macro's private naming
convention:

```rust
let diagnostics = runtime.call::<analysis::Check>(
    analysis::CheckArgs { expr, options: Options, strict: true },
)?;
```

Root installation returns a handle:

```rust
pub struct Installed<R: RootComponent> {
    runtime: Runtime,
    marker: PhantomData<fn() -> R>,
}

impl<R: RootComponent> Installed<R> {
    pub fn read(&self) -> Result<R::Output>;
    pub fn read_with_cancel(&self, cancel: CancelToken) -> Result<R::Output>;
    pub fn projection(&self, index: usize) -> Result<Value>;
    pub fn revision(&self) -> Revision;
    pub fn runtime(&self) -> &Runtime;
    pub fn observe_view<V: ViewSchema>(&self, cb: ...) -> Result<Subscription>;
}

impl Runtime {
    pub fn install_root<R: RootComponent>(&self, root: R) -> Result<Installed<R>>;
    // install() delegates and drops the handle
}
```

The old `read_outputs::<R>()` stays. New code writes:

```rust
let app = runtime.install_root(analysis::Analyze)?;
let (squares, total) = app.read()?;
```

For multi-output roots, generate constants next to the marker:

```rust
impl Analyze {
    pub const OUTPUT_0: usize = 0;
    pub const OUTPUT_1: usize = 1;
}
```

The names are positional because tuple fields are positional; the constant is
at least a single place to change if the output order changes.

`#[irrigation]` remains accepted but is no longer used in docs or examples.
The migration removes the module wrapper from the syntax examples and tests
where it adds nothing; modules that provide namespacing stay as ordinary
`mod` blocks.

### 5.5 General components

Two forms, one attribute. The macro decides the form by the item it receives:
an inherent `impl` is the function form; an `impl GeneralComponent for ...`
is the impl form.

#### Function form (reset producers)

For the common producer that recomputes from scratch:

```rust
pub struct TextComponent;

#[general_component(writes(Text), reads(SourceFile))]
impl TextComponent {
    fn run(cx: &mut GeneralCx) -> Result<()> {
        let document = SourceFile::DOCUMENT.read_in(cx)?;
        let version = document.snapshot().version().0;
        cx.write(Text::DOCUMENT).set(document.clone());
        cx.write(Text::VERSION).set(version);
        for (index, edit) in document.snapshot().edits().iter().enumerate() {
            cx.write(Text::EDITS).entry(index as u64).set(edit.clone());
            cx.write(Text::EDITS).entry(index as u64).present(true);
        }
        Ok(())
    }
}
```

The macro generates:

```rust
impl GeneralComponent for TextComponent {
    type State = ();
    type Output = ();
    const ID: ComponentId = ComponentId(const_hash(concat!(module_path!(), "::", "TextComponent")));
    const NAME: &'static str = concat!(module_path!(), "::", "TextComponent");
    const VIEWS: &'static [&'static ViewDescriptor] = &[<SourceFile as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] = &[<Text as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> Result<((), ())> {
        Self::run(cx).map(|()| ((), ()))
    }

    // no `update`: the default reinitializes on every invalidation
}
```

The `run` function is plain Rust and returns `Result<()>`. If it returns
`Result<T>`, the macro sets `Output = T` and wraps the result as
`((), T)`; the `State` is always `()` in this form. It is not lowered: it is a
`GeneralComponent` body, so the author controls the algorithm. The
`reads` list is optional; field reads register a View lazily, so it exists for
install-time diagnostics and board registration, not correctness. The `writes`
list is required for a producer and drives `OUTPUT_VIEWS` authorization.

#### Impl form (retained state / incremental)

```rust
#[general_component(
    reads(SourceFile),
    writes(Text),
    incremental,
    id(0x00B2_0000),
)]
impl GeneralComponent for TextComponent {
    type State = TextDocument;
    type Output = ();

    fn initialize(&self, cx: &mut GeneralCx) -> Result<(Self::State, Self::Output)> { ... }
    fn update(&self, cx: &mut GeneralCx, state: &Self::State, changes: &ChangeBatch)
        -> Result<(Self::State, Self::Output)> { ... }
}
```

The macro fills `ID`/`NAME` if absent, `VIEWS`/`OUTPUT_VIEWS` from the
attribute, and `dependency_updates` when `incremental` is present. Explicit
`const ID` and `const NAME` are respected for compatibility and collision
control.

The trait itself gains a default `update`:

```rust
fn update(
    &self,
    cx: &mut GeneralCx,
    _state: &Self::State,
    _changes: &ChangeBatch,
) -> Result<(Self::State, Self::Output)> {
    self.initialize(cx)
}
```

This is always sound for `Reset` and is a correct-but-coarse fallback under
`Incremental`: a component that forgot to override `update` reinitializes
instead of silently reusing a stale state. The `#[general_component]` macro
still rejects an explicit `incremental` impl that omits `update`, because the
declaration would otherwise promise incrementality the component does not
have.

#### Typed, local, and async forms

`TypedGeneralComponent`, `LocalGeneralComponent`, and `AsyncGeneralComponent`
remain. This plan does not add macro support for all three in the first pass.
It does three things:

* make typed fields the normal way to read inputs, so `GeneralInputs` is no
  longer necessary for new code;
* stop presenting `TypedGeneralComponent` as a peer choice in the README and
  docs; it is an adapter for existing typed code;
* give each form a single install method that returns a handle, so
  `read_outputs_general::<T>()` is not the only way to demand it.

Concretely, add `install_general_root` (the current `install_general`) and
`install_general_producer` (the current `install_general_component`); keep the
old names as aliases. Each returns `InstalledGeneral<T>` with `read()`.

### 5.6 Lowering additions

#### `if let`

Lower:

```rust
if let Some(value) = probe.value() {
    value
} else {
    0
}
```

into the same shape `match` already produces:

```rust
match <scrutinee> {
    Some(value) => { observe_local(node); <then> }
    _ => <else>,
}
```

Implementation notes:

* the scrutinee is lowered by `expr_node`, preserving its node;
* the pattern is installed with `shadow_pattern` in the then scope, exactly
  as `match` does (`lower.rs:480-511`);
* if the scrutinee is backed by a node, observe it once before the arm body;
* `if let ... else if let ...` recurses through the else expression;
* no `while let`, no `let else`; those remain rejected.

#### `return`

Support top-level `return expr` by wrapping the component body in a labeled
block and lowering `return` to `break 'component expr`.

* Only at `inline_depth == 0`. Inside a closure, `return` keeps its Rust
  meaning (return from the closure) and is left untouched.
* The returned expression is still lowered; its tracked reads are recorded.
* `?` keeps its current meaning (return from the generated implementation
  function). The labeled block does not change it.
* `return` with no value lowers to `break 'component ()` when the component
  output is `()`; otherwise the macro reports the type mismatch through normal
  Rust type checking.

This is a targeted relaxation of the documented "no `return`" boundary. Loops,
`let mut`, `let else`, effects, `unsafe`, and `async` stay rejected.

### 5.7 Collection, join, and reduction conveniences

**Join.** Make the join spec field-typed and add a closure constructor:

```rust
pub trait JoinSpec: Send + Sync + 'static {
    type Key: ...;
    type Left: ...;
    type Right: ...;
    type Joined: ...;

    fn left() -> MapField<Self::Key, Self::Left>;
    fn right() -> MapField<Self::Key, Self::Right>;
    fn combine(left: &Self::Left, right: &Self::Right) -> Self::Joined;
    fn target() -> Option<MapField<Self::Key, Self::Joined>> { None }
}

let join = EquiJoin::new(
    Left::ROWS,
    Right::ROWS,
    |left: &u64, right: &u64| left * right,
)
.publish_to(Joined::ROWS);

runtime.install_general_producer(join)?;
```

`EquiJoin::new` stores the two fields and a combine closure; `publish_to`
stores the optional target field. Its state is the same per-key diff. The
component identity is derived at install time from the two field identities
(and a type name), so two joins no longer collide. This requires two additive
changes to `GeneralComponent`:

```rust
fn component_id(&self) -> ComponentId { Self::ID }

/// Dynamic producers (a join with a runtime-chosen target) override this.
fn output_views(&self) -> Vec<&'static ViewDescriptor> {
    Self::OUTPUT_VIEWS.to_vec()
}
```

`Runtime::install_general*` uses `component_id()` for the uniqueness check and
`output_views()` for registration and producer ownership; the const
`OUTPUT_VIEWS` remains the default. `EquiJoin` overrides both from the field
pair and the target field. This is what lets a join with a target be installed
as a producer without a type-level `JoinSpec` and a type-level component id.

**Reductions.** Export:

```rust
pub mod reductions {
    pub struct Count;
    pub struct Sum<T>;
    pub struct Max<T>;
    pub struct Min<T>;
    pub struct Union<T>;
}
```

`Sum` is implemented for the integer types, not for floats: reassociation is
not lawful for IEEE addition, and the plan's cutoff rule must not be weakened
by convenience. `Max`/`Min` use `Aggregate = Option<T>` so `identity()` is
`None`; `Union` uses `BTreeSet<T>`.

### 5.8 Surface hygiene

**Prelude.**

```rust
pub mod prelude {
    pub use crate::{
        component, tree_view, view,
        ChangeBatch, ComputeError, DependencyUpdates, Edit, GeneralComponent,
        GeneralCx, Result, Runtime, RuntimeConfig, Snapshot, Subscription,
        ViewSchema,
    };
    pub use crate::fields::{
        DerivedListField, DerivedSetField, Field, GraphField, MapField,
        ReducedField, ScalarField, SourceListField, SourceSetField,
    };
}
```

`#[irrigation]` is not in the prelude. The macros still resolve through the
crate root, so the prelude changes imports, not code generation.

**Crate path.** Add `proc-macro-crate` to `irrigation-macros` and replace the
213 hardcoded `irrigation::` paths with the resolved crate path. A `FoundCrate`
resolver is enough:

```rust
fn irrigation_path() -> TokenStream {
    match crate_name("irrigation") {
        Ok(FoundCrate::Itself) => quote!(crate),
        Ok(FoundCrate::Name(name)) => Ident::new(&name, Span::call_site()).to_token_stream(),
        Err(_) => quote!(irrigation),
    }
}
```

A fixture crate with `irr = { package = "irrigation", path = "../irrigation" }`
must compile `#[view]`, `#[component]`, and `#[tree_view]`.

**Documentation split.** Mark `eval`, `scope`, `node_of`,
`observe_clone`, `observe_local`, `observe_ref`, `value_of`, `values_equal`,
`define_unit`, `ensure_view`, `read_unit_value`, `claim_field`, and the
primitive port helpers `#[doc(hidden)]`. They must stay public for generated
code. Add a crate-level section that names the three tiers and points new
readers at the prelude.

### 5.9 The scorecard

| Proposed API | Functional because | General because | Gate |
|---|---|---|---|
| `Field` constants and handles | `Copy` values; reads and writes take their context explicitly | one shape for every `#[view]` family; generated from the declaration | per-family unit/read/write gates |
| `Edit::staged_content` + `edit_view`/`with_view` | closure-scoped transaction; result returned | any `EditableView`, source or tree | two-editor and no-op gates |
| `#[component(root|callable)]` | declarative kind on the item; no runtime mode flag | root and callable; mixed `&View`/owned parameters | root/callable and mixed-param gates |
| `Installed<R>` | typed value returned by install | any `RootComponent` | installed/read equality |
| `#[general_component]` | plain Rust methods returning `Result` | reset and incremental; any View; root or producer | function/impl form and output gates |
| `if let` / `return` | ordinary Rust control flow | any component body, any output type | trace-equality gates |
| `EquiJoin::new(fields, closure)` | combine is a value; constructor is ordinary | any pair of map fields; multiple joins | two-join and combine-count gates |
| `reductions::*` | lawful algebra values | any element type the algebra supports | aggregate-equality gates |
| crate-path resolver | generated paths are data | every macro, every dependency name | renamed-dependency fixture |

The table is the answer to the "functional and general" requirement in one
place: every row can be used as a value or closure, takes its context as an
argument, and is not specialized to one example.

---

## 6. Reducible logic: what should disappear

The distinction that matters is not "short code vs long code". It is:

* **restated identity** — the framework knows it, the user writes it again;
* **restated structure** — a trait item, field id, or command sequence that
  follows mechanically from a declaration;
* **workaround logic** — a shape that exists only because an API is missing
  (dummy seeds, duplicate `update`, manual `ensure_view`).

| Reducible item | Where | Count | Replacement | Effect on user code |
|---|---|---|---:|---|---|
| Field ids and unit kinds | syntax, observe, tests | 305 `FieldId(`, 248 `UnitKey::*` | generated `Field` constants | deleted, not shortened |
| Unit-key constructor functions | syntax | ~19 functions | typed field entries | ~150 lines in the syntax crate |
| `ensure_view` calls before definitions | syntax, examples | scattered | `Field::write` / generated define | deleted |
| `ID`/`NAME`/`VIEWS`/`OUTPUT_VIEWS` | general impls | 95 impls | `#[general_component]` | ~8-12 lines each |
| `update = initialize` | general impls | 71 of 95 | default `update` | ~4 lines each |
| Dummy `seed: u8` parameters | STLC example | 4 components | explicit callable components | ~20 lines + call identity |
| `#[irrigation]` wrapper | 235 modules | 235 | direct `#[component]` | 1 line + 1 nesting level each |
| `__FooArgs` / `__FooSpec` | host calls | all callable components | `FooArgs` / `Foo` | name-only |
| `read_outputs::<R>` at each read | hosts | 436 | `Installed<R>::read` | type only |
| `snapshot.view::<V>()` in observer | observer tests | second type spelling | `observe_view::<V>` | one line |
| `field0()` | tests, README | 307 | `.value()` / `#[field(name)]` | readable, no line change |
| Hand-written `Sum`/`Count` | demo, user code | 1 per algebra | `reductions::*` | ~12 lines per algebra |
| Manual `JoinSpec` | user code | 1 per join | `EquiJoin::new(field, field, closure)` | ~20-30 lines per join |
| Nested `match` for `Option` | `name_resolver`, tree components | common | `if let` | several lines per access |
| `to_vec().iter().sum()` | components | common | `fold` / `sum` | one allocation |
| `defined.clone()` in recursion | `name_resolver` | per child | `Context::from_entry` | no clone, smaller keys |
| `rules()` / `payloads()` re-materialization | demo | 2 helpers | typed general component / field read | correctness and cost |
| `TypedGeneralComponent` + `GeneralInputs` | tests only | 4 impls | typed field reads | one trait layer |

Some of these are line-count wins; some are correctness wins. F11 (lost
writes), F1 (wrong unit), F5 (unexpected root/callable), F16 (silent
non-publication), and F20 (hidden coarse node) are the correctness cases and
outrank the line counts.

### What *not* to reduce

* The `UnitKey`/`UnitKind` model must stay public: the Observatory, the
  arbitration and claim APIs, and advanced `PortProvider`/`RouteSelector`
  users need it. The plan hides it from *ordinary* authoring, not from the
  extension tier.
* `GeneralComponent` must stay as the algorithm-level escape hatch. The
  proposed macro fills boilerplate; it must not pretend every algorithm is a
  lowered component.
* The `Result` return of `edit` and the atomic commit rule stay. A panicking
  `edit` or a silently retried one would be worse than a `?`.
* The `if let`/`return` additions must not sneak in loops or mutable locals.
  Those rejections protect the trace model; they are not cosmetic.

### Estimated shape after migration

These are order-of-magnitude expectations, to be replaced by measurements at
each phase:

* the syntax pipeline drops from three general-component scaffolds plus
  hand-written unit constructors to three macro-annotated impls/functions plus
  field constants; a 100-200 line reduction in component plumbing, with no
  semantic change;
* the 95 test/example general impls lose their constant blocks and, in 71
  cases, their `update` method;
* the 235 module wrappers become optional; examples and the syntax crate stop
  using them;
* ordinary user code stops naming `FieldId`, `UnitKey`, `ViewSchema::view_id`,
  and `ensure_view`; those names remain in the extension tier and the kernel;
* no existing gate changes its expected execution counts. If a phase changes
  execution counts, it has changed granularity and must be redesigned, not
  blessed.

---

## 7. Work plan

The phases are ordered by dependency and risk. Each phase must leave the
workspace green; none waits for a later phase to be correct.

### Phase 0 — stop the silent data loss (prerequisite)

**Goal.** Two editor scopes in one transaction commit both writes.

**Changes.**
* `irrigation-runtime::edit`: add `staged_content`; keep `content` as the
  committed read.
* `irrigation-macros::view` and `irrigation-macros::tree`: generated
  `EditableView::edit` and the tree editor use `staged_content`
  (`view.rs:887-896`, `tree.rs:660`).
* `irrigation-collections`: no change; editors already stage unit writes.

**Gates.**
* the probe-2 regression (two `edit.view` calls, two pushes, list `[1, 2]`);
* two editors touching different fields of one View;
* scalar write followed by collection write in one transaction;
* a whole-content replacement after unit writes still normalizes correctly
  (the existing arbitration gates must stay green);
* a no-op second editor still publishes no revision.

**Risk.** The commit normalization is subtle (`edit.rs:186-248`). The overlay
must be used only for constructing an editor, not for the final committed
value comparison. Add the gate before changing the macro.

### Phase 1 — typed field handles and generated constants

**Goal.** Every declared field has one identity, generated from its
declaration, usable in components and `GeneralComponent`s.

**Changes.**
* `irrigation-collections::field`: `Field<H>`, family aliases, `MapEntry`,
  `read_in`/`get_in`/`fold`, `write(...)` writers; make `*Ref::new` const and
  the handles `Copy`; add `Hash` to generated View handles.
* `irrigation-macros::view`: generate field content functions, associated
  constants, docs, `#[field(name = ...)]`, `value()` aliases, generalized
  single-field forwarding.
* `irrigation-collections`: export real `List<T>`/`Set<T>` markers; add
  `const fn` constructors.
* `irrigation-syntax`: migrate `Text`, `Tokens`, `AstMeta`, `SourceFile`, and
  the data views to the constants; delete the hand-written field constants and
  unit constructors.

**Gates.**
* a compile test that a field constant is usable in `const` position;
* each family: `field.unit()` equals the unit the old generated path used;
* each family: `read_in` inside a `GeneralComponent` matches a tracked read;
* each family: `write(cx)` publishes the same snapshot as
  `cx.output().field(...)`;
* a field inserted in the middle of a View updates every constant and every
  generated define method, with no hand-written id left to drift;
* the existing per-family gates for source/derived reads, negative reads, and
  observers pass unchanged;
* `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  (generated docs satisfy `missing_docs`).

**Risk.** Const constructors for the handle types; associated-const privacy;
macro expansion size. Add the constants behind no feature flag: they are
additive and compile-time only.

### Phase 2 — component declaration and installation

**Goal.** One explicit kind, one typed install handle, one public host-call
spelling.

**Changes.**
* `#[component(root|callable)]`; keep the implicit root rule for
  compatibility, documented as deprecated.
* callable components accept `&View` parameters via owned `Args`;
* generate `FooArgs` / `Foo` (spec) with `__` aliases;
* `Runtime::install_root` + `Installed<R>`; `install` delegates;
* output index constants on root markers;
* direct `#[component]` in docs; `#[irrigation]` kept as an alias but removed
  from the quick start.

**Gates.**
* a root with only `&View` params installs and reads identically to today;
* a callable with mixed `&View` and owned params compiles and interns;
* a parameterless callable is not a root; `runtime.install` rejects it and
  the host calls it through `runtime.call` instead;
* `Installed::read` and `read_outputs` return equal values;
* host `call::<Foo>(FooArgs { ... })` works outside a component;
* a dependency renamed to `irr` compiles (moved to Phase 5 if the fixture
  lags).

**Risk.** The implicit-root compatibility rule is the subtle part. Keep the
generated behavior byte-for-byte for components without the new attribute;
add a compile-fail test for `#[component(root)]` with a non-`&View` parameter
until root parameters are designed.

### Phase 3 — general components and joins

**Goal.** A producer declares what it reads/writes and writes code, not trait
ceremony.

**Changes.**
* `#[general_component]` function form (`impl Type { fn run(cx) }`);
* impl form with `reads`/`writes`/`incremental`/`id` and default `update`;
* `GeneralComponent::component_id(&self)` and `output_views(&self)` default
  methods;
* `EquiJoin::new(field, field, closure).publish_to(target)` and field-typed
  `JoinSpec`;
* `reductions::{Count, Sum, Max, Min, Union}`;
* typed handles for `TypedGeneralComponent` inputs, without deleting the
  trait.

**Gates.**
* a reset producer's `run` compiles without `ID`/`NAME`/`VIEWS`;
* two general producers with different types coexist;
* an incremental impl that omits `update` fails to compile with a useful
  message;
* two `EquiJoin`s over different field pairs install and produce independent
  results;
* a `publish_to` join installs as a producer and its target View receives one
  entry per joined key;
* a join over a deleted key retracts its entry;
* a join over an unchanged key executes no combine calls (existing
  combine-count gate);
* built-in `Sum`/`Count` yield the same aggregates as the hand-written types.

**Risk.** Dynamic component identity changes install checks; audit every
`T::ID` use in `runtime.rs` and `general.rs`. The default `update` must not be
injected for `incremental` impls.

### Phase 4 — lowering and context

**Goal.** Familiar control flow and cheap recursive contexts.

**Changes.**
* `if let` lowering;
* top-level `return` lowering;
* `Context::from_entry` / `field.entry(key).context()`;
* `fold` on list handles (if not landed in Phase 1);
* explicit granularity vocabulary for opaque helpers (`opaque` attribute or a
  documented `PlainFn` wrapper). Do not change the default strictness.

**Gates.**
* `if let` over `Option` and over a tree kind produces the same trace and
  result as the equivalent `match`;
* `return` in the top level and in a nested block works; `return` inside a
  closure still returns from the closure;
* a component with `return` has the same executed-node counts as the inverted
  form;
* `Context`-based recursion over a large environment does not rekey
  descendants when an unrelated field changes;
* a deliberately opaque helper still produces one node, and the declaration
  is visible in the source.

**Risk.** `return` interacts with `?` and with the statement-level effect
barrier (`lower.rs:299-320`). Test the trace, not only the value. `if let`
must not observe the scrutinee twice.

### Phase 5 — surface hygiene, docs, migration

**Goal.** One obvious path for a new user; old paths still compile.

**Changes.**
* `prelude`;
* `#[doc(hidden)]` on macro targets;
* `proc-macro-crate` + renamed-dependency fixture;
* README quick start and `docs/architecture.md` API section rewritten around
  the new vocabulary;
* migrate examples and tests to constants/`if let`/`Installed` where it
  clarifies them; do not mechanically churn tests that intentionally probe
  raw units;
* deprecation notes for `#[irrigation]`, implicit roots, and `field0`
  guidance.

**Gates.**
* `python3 scripts/check-md-links.py`;
* README code blocks compile (add a doctested quick-start module or an
  `examples/api_quickstart.rs`);
* `cargo check -p irrigation-runtime --no-default-features` and
  `cargo check -p irrigation --all-targets`;
* the public API inventory in Appendix B matches the facade.

**Risk.** Documentation churn. Keep the migration table as the source of
truth; do not rewrite every test.

---

## 8. Acceptance gates

The existing gates remain the contract:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo check -p irrigation-runtime --no-default-features
cargo check -p irrigation --all-targets
cargo build -p irrigation --examples --features observatory
python3 scripts/check-md-links.py
```

New gates, by phase:

| Gate | Proves |
|---|---|
| `two_editor_scopes_commit_both_writes` | F11 fix |
| `staged_content_sees_earlier_writes` | `Edit::staged_content` semantics |
| `field_constants_match_descriptor_order` | every generated id matches `FieldDescriptor` order |
| `field_read_in_matches_tracked_read` | `read_in` is the same unit read |
| `field_write_publishes_in_general_component` | F16 fix |
| `callable_component_accepts_view_reference` | F5 fix |
| `parameterless_component_is_not_a_root` | F5 fix |
| `installed_read_matches_read_outputs` | F8 fix |
| `general_component_function_form` | Phase 3 function form |
| `incremental_component_requires_update` | compile-fail diagnostic |
| `two_joins_coexist` | F17 fix |
| `if_let_matches_match_trace` | F19 lowering |
| `return_matches_inverted_trace` | F19 lowering |
| `renamed_dependency_compiles` | F10 fix |
| `observe_view_is_typed` | F22 fix |

Compile-fail tests: the workspace does not currently use `trybuild`. Either
add `trybuild` as a dev-dependency (it is a normal test dependency, not a
runtime one) or use a small script that runs `cargo check` on a fixture crate
and asserts a substring. The plan prefers `trybuild` for diagnostics that are
part of the public contract (`if let` rejection before Phase 4, mixed
root/callable before Phase 2, incremental-without-update), because a substring
check on rustc output is brittle.

Performance gate: re-run `cargo run --release --example scaling` and the
workload matrix. Field constants are compile-time; the only runtime additions
are `ensure_view` checks (a hash lookup) on field operations. Measure that the
per-edit execution counts and the release edit+read numbers do not regress
beyond noise before and after each phase.

---

## 9. Compatibility and migration

| API | Change | Migration | Earliest removal |
|---|---|---|---|
| `edit.view::<V>()` multiple times per transaction | behavior fix | none; code keeps working and now commits every write | never |
| `Edit::content` | unchanged (committed read) | none | never |
| `Edit::staged_content` | additive | use in generated editors only | never |
| `Field` / family aliases | additive | adopt incrementally | never |
| `#[view]` generated field constants | additive | delete hand-written ids when convenient | never |
| `#[field(name = ...)]` | additive | use for tuple Views | never |
| Single-field `value()` / forwarding | additive | prefer over `field0()` | never |
| `List`/`Set` markers | additive | import from `irrigation` | never |
| `#[component(root|callable)]` | additive | new code uses explicit; old code keeps the heuristic | 1.0 flips the default |
| `FooArgs` / `Foo` spec names | additive (aliases for `__`) | new host calls use the public names | 1.0 may drop `__` aliases |
| `install_root` / `Installed` | additive | prefer in new host code | never |
| `#[general_component]` | additive | new producers use it; old impls stay | never |
| `GeneralComponent::component_id` | additive default | no action | never |
| `if let` / `return` | additive language features | use them in new components | never |
| `#[irrigation]` | docs deprecation | remove from new code; stays compiling | 1.0 may warn |
| `field0()` | docs preference | use `.value()` or `#[field(name)]` | never (or deprecate in 1.0) |
| hardcoded `irrigation::` | behavior fix | renamed dependencies start working | never |
| `TypedGeneralComponent` | no new investment | existing code keeps working | not in this plan |
| `V::Snapshot` naming | open | no action yet | future major |

The rule for the 0.x series: **additive APIs land with docs and gates; the old
spelling stays until a named removal release.** The one exception is F11,
which is a silent data-loss bug and must be fixed even though it changes what
a program commits (the old behavior was never a contract).

---

## 10. Risks and open questions

**R1 — The editor overlay may interact with commit normalization.** The
staged content is an input to the next editor, not a claim about the final
value. The overlay must not cause `Edit::commit` to treat a prior content
replacement as a unit write. The Phase 0 gates cover this; do not proceed to
`edit_view` until they pass.

**R2 — Const handles and MSRV.** The workspace's `rust-version` is 1.85.
`const fn` constructors with function pointers and associated consts of
non-`Copy` generic types need a quick compile check. If a family handle cannot
be a const, make the constant a `fn` returning the handle
(`Tokens::documents()`) and keep the API shape; the macro can choose per
family.

**R3 — Implicit root compatibility.** Changing the default to callable would
break 235 modules and every example. The plan keeps the heuristic for 0.x and
adds explicit attributes. The long-term cost is two rules; the alternative is
a large, untested migration now.

**R4 — Root parameters.** A root that needs install-time configuration cannot
be expressed without changing `RootComponent::run` and `RootRecord::run`. The
plan defers this; `#[general_component]` is the supported answer. If it becomes
a priority, the design is a `Params` associated type plus an `Arc<dyn Fn()>`
run closure in `RootRecord`, not a second root trait.

**R5 — `if let` observation semantics.** The match lowering must observe the
scrutinee exactly once and must shadow pattern bindings. A wrong lowering can
make a component depend on the wrong node or miss a negative dependency. Gate
it against `match` on the same input, including the `None` branch and a
tree-kind `Option`.

**R6 — `return` and effect barriers.** A `return` inside a statement that
stages definitions can retire a branch earlier or later than the equivalent
tail expression. The trace gate must include a branch that defines a unit.

**R7 — `proc-macro-crate` adds a build dependency.** It is cached on this
machine (`proc-macro-crate-3.5.0`), but the workspace's offline baseline must
be checked. If it cannot be added, the fallback is a `crate = path` attribute
on each macro, which is worse. Prefer the resolver.

**R8 — Public field constants increase macro output.** 967 fields × a content
function + constant is more code at compile time. Measure `cargo check` on
`irrigation-syntax` before/after; if it is material, emit constants only for
fields used through `Field` (impossible to know) or accept the cost. The
current macro already emits a closure per live accessor, so the marginal cost
should be small.

**R9 — `EquiJoin` dynamic identity.** Deriving a component id from a field
pair makes two joins over the same pair collide by design. That is the correct
semantic answer (they would define the same units), but the error message must
say "two joins over the same fields" rather than "duplicate component".
`component_id()` must also be used by the observation board and the identity
report; audit those reads.

**R10 — Field name collisions.** `foo` and `FOO` both produce `FOO` constants;
a field named `value` suppresses the single-field `value()` alias. The macro
must diagnose the first and skip the second rather than emit a cryptic
duplicate-definition error.

**Decision points for the maintainer.**

1. Is a 0.x breaking change to the `#[component]` default acceptable, or
   should the heuristic stay until 1.0? **Recommendation: stay until 1.0.**
2. Should `Field` constants replace the collection accessor return types, or
   remain additive? **Recommendation: additive.** Changing `MapRef` returns to
   `Field<MapRef>` is source-compatible only with `Deref` and adds churn for
   little gain.
3. Is a separate `irrigation-api-tests` fixture crate acceptable for the
   renamed-dependency and compile-fail gates? **Recommendation: yes**, as a
   dev-only workspace member; it avoids a package cycle.
4. Should `#[general_component]` support the function form and the impl form
   in one release? **Recommendation: yes.** The function form is the big win;
   the impl form is a small parser on top.
5. Should `if let`/`return` land before the field macro, or after? **Recommendation:
   after Phase 1.** They are independent, but the field work is the larger
   user pain and the language change touches the macro lowering that Phase 1
   also edits.

---

## Appendix A — Before and after

### A.1 The README quick start

Before:

```rust
let entries = snapshot.view::<Squares>().field0();
let total = runtime.read_outputs::<analysis::Analyze>()?.1;
```

After:

```rust
let entries = snapshot.view::<Squares>().value();
let app = runtime.install_root(analysis::Analyze)?;
let (_, total) = app.read()?;
// or, if Squares keeps a named field:
// snapshot.view::<Squares>().entries()
```

### A.2 A syntax publisher

Before (the shape in `components/text.rs`):

```rust
fn read_source(cx: &mut GeneralCx) -> Result<TextDocument> {
    cx.read_typed(UnitKey::field(
        <SourceFile as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
    ))
}

fn publish(cx: &mut GeneralCx, document: &TextDocument) {
    let mut output = cx.output();
    output.field(Text::view_id(), FieldId(0), document.clone());
    output.field(Text::view_id(), FieldId(1), version);
    output.entry(Text::view_id(), FieldId(2), index, edit.clone());
    output.membership(Text::view_id(), FieldId(2), index, true);
}
```

After:

```rust
#[general_component(writes(Text), reads(SourceFile))]
impl TextComponent {
    fn run(cx: &mut GeneralCx) -> Result<()> {
        let document = SourceFile::DOCUMENT.read_in(cx)?;
        let version = document.snapshot().version().0;
        cx.write(Text::DOCUMENT).set(document.clone());
        cx.write(Text::VERSION).set(version);
        for (index, edit) in document.snapshot().edits().iter().enumerate() {
            let mut edits = cx.write(Text::EDITS);
            edits.entry(index as u64).set(edit.clone());
            edits.entry(index as u64).present(true);
        }
        Ok(())
    }
}
```

The `for` loop is legal: this is a `GeneralComponent` body, not a lowered
component. The lowered component subset is unchanged for `#[component]`.

### A.3 A recursive component with `if let`

Before:

```rust
#[component]
pub fn unresolved(expr: ast::ExprRef, defined: Vec<String>) -> Vec<String> {
    let kind = expr.kind();
    if kind == Some(ast::ExprKind::Var) {
        let name = expr.var().unwrap_or_default();
        if defined.contains(&name) { Vec::new() } else { vec![name] }
    } else if kind == Some(ast::ExprKind::Call) {
        let args = expr.args();
        args.iter()
            .flat_map(|arg| unresolved(arg.clone(), defined.clone()))
            .collect()
    } else {
        Vec::new()
    }
}
```

After (`if let` and `Context`):

```rust
#[component]
pub fn unresolved(expr: ast::ExprRef, defined: Context<ScopeId, Vec<String>>) -> Vec<String> {
    if let Some(kind) = expr.kind() {
        match kind {
            ast::ExprKind::Var => {
                let name = expr.var().unwrap_or_default();
                if defined.read().contains(&name) { Vec::new() } else { vec![name] }
            }
            ast::ExprKind::Call => {
                let args = expr.args();
                args.iter()
                    .flat_map(|arg| unresolved(arg.clone(), defined.clone()))
                    .collect()
            }
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    }
}
```

The `Context` key is small; the environment is read through a tracked unit, so
a change to it re-executes the invocation without rekeying its descendants.
The recursive call clones the context handle, not the vector.

### A.4 An observer

Before:

```rust
let _sub = runtime.observe::<Squares>(|snapshot| {
    let squares = snapshot.view::<Squares>().value();
    render(squares);
})?;
```

After:

```rust
let _sub = runtime.observe_view::<Squares>(|squares| {
    render(squares.value());
})?;

// per-entry granularity from the declaration, not a field number:
let _entry = Numbers::VALUES.entry(3).observe(&runtime, |snapshot| {
    render(snapshot.view::<Numbers>().values());
})?;
```

---

## Appendix B — Public API inventory and intended tier

| Tier | Current items | Intended placement |
|---|---|---|
| Authoring | `view`, `component`, `tree_view`, `Runtime`, `RuntimeConfig`, `Edit`, `Snapshot`, `Subscription`, `ViewSchema`, `SourceView`, `DerivedView`, `EditableView`, `Result`, `ComputeError` | prelude + docs |
| Deprecated | `irrigation` | docs only; kept compiling |
| Typed fields | `ScalarRef`, `MapRef`, `SourceListRef`, `DerivedListRef`, `SourceSetRef`, `DerivedSetRef`, `GraphRef`, `ReducedRef`, `Context`, `Param` | prelude (handles) |
| Extension | `GeneralComponent`, `GeneralCx`, `ChangeBatch`, `DependencyUpdates`, `TypedGeneralComponent`, `LocalGeneralComponent`, `AsyncGeneralComponent`, `Reduction`, `JoinSpec`, `Lattice`, `FixedPointProblem`, `PortProvider`, `RouteSelector`, `RootComponent`, `ComponentSpec` | documented, not prelude |
| Diagnostic | `Snapshot::version`, `contains`, `Runtime::stats`, `dump`, `explain`, `explain_node`, `identity_report`, `routing_*`, `memory_report` | docs under "Diagnostics" |
| Macro target | `eval`, `scope`, `node_of`, `observe_clone`, `observe_local`, `observe_ref`, `value_of`, `values_equal`, `define_unit`, `read_unit_value`, `ensure_view`, `claim_field`, `provide_ports`, `wait_for_port`, `require_*` | `#[doc(hidden)]`; public for generated code |

The inventory is not a removal list. The macro target tier must remain public;
it is simply not the first thing a new user should read.

---

## Appendix C — Probe transcripts

**P1 — direct component.** A free `#[component] fn total(numbers: &Numbers) -> u32`
at module scope compiled and installed through `Total`; no `#[irrigation]`
module was present.

**P2 — two editors.** With `Numbers { values: List<u32> }`, the closure in §2
committed `[2]`. The same writes through one bound editor committed `[1, 2]`.

**P3 — `if let`.** `cargo check` on a component containing
`if let Some(value) = probe.value()` reported
`error: \`let\` expressions are not supported` at the `if let`, plus an
unresolved `value` in the then-branch.

**P4 — generated define in a general component.** A producer whose
`initialize` called `Produced.define_value(7)` and returned `()` did not
publish the value; a root consumer read `0`. The unit-key form through
`cx.output().field(...)` read `7`.

These probes are not tests yet; Phases 0-3 turn them into permanent gates.

---

## Appendix D — References

- [`concurrent-incremental-system-plan.md`](concurrent-incremental-system-plan.md) —
  §10 is the original intended public API, including `runtime.install(analysis::analyze)`
  and `snapshot.read::<(Types, Diagnostics)>()`; this plan closes the gap
  between that sketch and the shipped surface.
- [`views-components-granularity-audit.md`](views-components-granularity-audit.md) —
  the four increment extents and the boundaries every new API must preserve.
- [`heterogeneous-granularity-plan.md`](heterogeneous-granularity-plan.md) —
  the family-specific fixes that this plan depends on.
- [`../docs/architecture.md`](../docs/architecture.md) — component lowering,
  custom algorithms, and the current extension points.
- [`../crates/irrigation-macros/src/view.rs`](../crates/irrigation-macros/src/view.rs) —
  field parsing, single-field forwarding, editor drop.
- [`../crates/irrigation-macros/src/component.rs`](../crates/irrigation-macros/src/component.rs) —
  root detection and generated invocation types.
- [`../crates/irrigation-macros/src/lower.rs`](../crates/irrigation-macros/src/lower.rs) —
  the lowered subset, `if let`/`return` rejection points.
- [`../crates/irrigation-runtime/src/edit.rs`](../crates/irrigation-runtime/src/edit.rs) —
  transaction normalization.
- [`../crates/irrigation-runtime/src/general.rs`](../crates/irrigation-runtime/src/general.rs) —
  component traits, `GeneralCx`, `OutputTxn`.
- [`../crates/irrigation-collections/src/join.rs`](../crates/irrigation-collections/src/join.rs) —
  `EquiJoin` and `JoinSpec`.
- [`../crates/irrigation-syntax/src/views.rs`](../crates/irrigation-syntax/src/views.rs) —
  the hand-written `AstMetaField` constants.
- [`../crates/irrigation-syntax/src/components/parser.rs`](../crates/irrigation-syntax/src/components/parser.rs) —
  the hand-written unit constructors.
- [`../crates/irrigation/examples/observatory/demo.rs`](../crates/irrigation/examples/observatory/demo.rs) —
  opaque helper calls over Views.
- [`../crates/irrigation/examples/stlc_scope_graphs/main.rs`](../crates/irrigation/examples/stlc_scope_graphs/main.rs) —
  dummy seed parameters and the stage chain.

---

## 11. Implementation status

The phases in §7 are implemented in the workspace. Evidence:

| Area | Where |
|---|---|
| Editor staged-content fix | `Edit::staged_content`, generated View/tree editors, `crates/irrigation/tests/authoring_api.rs` |
| Typed field handles | `irrigation-collections::field`, generated `Field` constants in `#[view]`, typed readers/writers, `Context::from_entry`, `fold` |
| Explicit component kind | `#[component(root)]` / `#[component(callable)]`, callable `&View` parameters, public `FooArgs`/`Foo` with `__` aliases, `Installed` |
| General component authoring | `#[general_component]` function and trait-impl forms, default `update`, `component_id`, `output_views` |
| Lowering | `if let` to match, top-level `return` to a labeled block |
| Joins and reductions | `Join::new(...).publish_to(...)`, dynamic identity/output views, built-in `Count`/`Sum`/`Max`/`Min`/`Union` |
| Surface hygiene | `prelude`, `#[doc(hidden)]` macro targets, crate-path rewriting and the `irrigation-api-rename` fixture |
| Migration | README quick start, `docs/architecture.md`, the syntax pipeline, and the STLC example |

Deviations from the written plan, stated explicitly:

1. **Tree fields do not get a `Field` constant.** `#[tree_view]` already
   generates typed node refs and has no per-field `FieldId` surface; a
   `TreeField` would be a second handle family with no identity removed. Tree
   editors do use `staged_content`.
2. **Root parameters are still deferred.** `#[component(root)]` requires all
   parameters to be `&View`; a configured root is a `GeneralComponent`.
3. **`#[general_component]` covers `GeneralComponent` only.** The typed,
   local, and async adapters remain manual; the typed field handles remove most
   of their input boilerplate.
4. **The closure join is a second implementation, not a refactor of
   `JoinSpec`.** `EquiJoin<S>` keeps the type-level trait; `Join` offers the
   closure form. A future refactor can share the per-key loop.
5. **`field0()` remains.** It has a `value()` alias for one-field Views and
   `#[field(name = ...)]` for tuple fields, but the generated method is not
   deprecated in this change.
6. **`#[irrigation]` remains accepted.** It is no longer required or used in
   the README quick start; no compiler deprecation is emitted.
7. **`Field::fold` takes `Fn(A, T) -> A`.** The plan sketched a `Param<T>`
   argument; `Param` requires a per-member node, and `fold` is deliberately a
   whole-node operation here. `map`/`filter`/`flat_map` keep per-member
   `Param`s.
