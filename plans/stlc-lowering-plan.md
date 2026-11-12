# `lower`: the declared tree to MIPS

**Status:** implemented.
**Scope:** the STLC example's fourth stage — `machine.rs` (the `Machine` View and
one component), the root wiring, and the example's docs. The stage was developed
against a simulator of the emitted subset; that fixture and the example's tests
were removed when the examples became test-free (see §5).
**Out of scope:** the framework. The tree's accessors already answer everything a
code generator asks; nothing in the kernel, the macros, or the syntax crate
changed.
**Related:** [`ast-tree-view-plan.md`](ast-tree-view-plan.md) — the declared tree
and the rules this stage obeys (one emission per expression; identity from the
term; no whole-table publisher).

---

## 0. The design

One component, one View, one unit per instruction:

```rust
#[incremental_data]
pub struct Machine {
    /// The listing: one row per instruction position, in program order.
    pub program: Map<u32, Line>,
}

pub const SHELL: u32 = 7;
```

`lower(term, chain, at) -> u32` is the generator: a `match term.kind()` whose arms
write their instructions at `at`, recurse into children at the offsets they
compute, and return the first free row. No block type, no placement pass, no relay
functions — the arms are the generator, and each row is written where it is
emitted.

The rejected shapes are worth naming because each is the obvious first idea: a
subtree `Vec` per term re-versioned the whole program on every edit; one
`List<Line>` made the program a single unit; `(term, index)` blocks with a second
placement component bought position-independent code identity at the price of a
`Step` type and a duplicated layout. The listing's identity is its order, so
positions are the honest unit, and the rows are compared individually.

---

## 1. The machine

| Register | Holds |
|---|---|
| `$v0` | the value of the last expression |
| `$s0` | the current environment frame |
| `$t3` | the heap bump pointer (callees advance it; nothing else writes it) |
| `$a0` / `$a1` | the callee's argument / the frame its closure captured |
| `$sp` | one saved word at a time |
| `$t0` `$t1` `$t2` | scratch |

A frame is `{value, parent}`, a closure is `{code, env}`, eight bytes each out of
a fixed `.data` block. A reference's depth is `chain.len() - 1 - position`, and the
walk is a runtime loop over that constant because the arm's row count must not
depend on the chain, which a component body cannot iterate. Values are untagged:
the checker is the authority, exactly as `evaluate` is untyped.

---

## 2. The arms

`<id>` is `term.raw()` — stable and unique per node.

- **Var** (9 rows): `move $t0, $s0`, `li $t2, <d>`, `walk_<id>:`,
  `beq $t2, $zero, end_<id>`, `lw $t0, 4($t0)`, `addiu $t2, $t2, -1`,
  `b walk_<id>`, `end_<id>:`, `lw $v0, 0($t0)`. Unresolved: a comment and
  `li $v0, 0`.
- **Int** (1): `li $v0, <n>`; out of `i32`, a comment and `li $v0, 0`.
  **Bool** (1): `li $v0, 1` or `li $v0, 0`.
- **App**: `func` block, `addiu $sp, $sp, -4`, `sw $v0, 0($sp)`, `arg` block,
  `move $a0, $v0`, `lw $t0, 0($sp)`, `addiu $sp, $sp, 4`, `lw $a1, 4($t0)`,
  `lw $t1, 0($t0)`, `jalr $t1`.
- **Lam** (16 before the body, 5 after): closure `{code, env}` (6), `b done_<id>`,
  `lam_<id>:`, `$ra`/`$s0` saved on the stack (3), parameter frame
  `{value: $a0, parent: $a1}` (5); then the body; then `lw $s0, 0($sp)`,
  `lw $ra, 4($sp)`, `addiu $sp, $sp, 8`, `jr $ra`, `done_<id>:`.
- **Let**: `value` block, the frame push (5 rows), `body` block, then
  `lw $s0, 4($s0)`.
- **If**: `cond` block, `beq $v0, $zero, else_<id>`, `yes` block,
  `b done_<id>`, `else_<id>:`, `no` block, `done_<id>:`.
- **No declared variant**: one comment row.

Correctness, in one paragraph: `$s0` is the only environment base, so a lambda
saves it (and `$ra`, which the body's calls clobber) on the stack and restores both
at exit; a `let` restores `$s0` from the frame it pushed, and every nested binder
restores its own, so the restores balance. `$sp` is balanced at every term
boundary: `App` pushes one word around the argument and pops it before the call.

---

## 3. The shell

The root writes rows `0..SHELL` (`.data`, `heap: .space 65536`, `.text`,
`.globl main`, `main:`, `li $s0, 0`, `la $t3, heap`), the entry term's rows, and
then `move $a0, $v0`, `li $v0, 1`, `syscall`, `li $v0, 10`, `syscall`. No
indentation: the board renders a row per instruction and would collapse it.

---

## 4. Granularity, edit by edit

| Edit | Rows |
|---|---|
| Rename | `lower` re-runs (it reads binder names for `<d>`) and writes the same text at the same positions: no row's version moves |
| Literal `1`→`2` | one row |
| Lambda annotation | none: annotations are not lowered, so every row keeps its value |
| Structural insert | the rows before it are unchanged; the rows after it are re-placed — a listing's identity is its order |
| Parse error | the shell plus `li $v0, 0` |

Each row is one `Map` unit written by the invocation that emitted it, so rows a
term no longer writes retract with that term's invocation. Nothing is republished
wholesale and there is no draft.

Three things the implementation settled:

1. **"An annotation edit does not even re-run the stage" is false, and false for
   every stage.** A revision that publishes a parse re-runs `resolve`, `check`,
   `evaluate`, and `lower` alike; measured with `explain_node`, a whitespace-only
   edit does it too, so the cause is the parse's own reads rather than anything
   this stage observes. The contract is about published values, so the gate
   asserts the rows are equal, not the execution counts.
2. **The shell's rows are seven explicit `define` calls** in the root body, since
   a component body cannot loop over a table; `the_program_starts_at_main` pins
   the seven rows, so `SHELL` cannot drift from what is written.
3. **The stage is called `machine::lower`** (the module holds the View, the
   constant, and the component), and `pipeline::stlc` calls it once; the
   three-stage gate became `the_pipeline_has_four_stage_components`.
4. **The body binds the field handle once** (`let program = Machine.program();`).
   Without it `rustfmt` breaks every `Machine.program().define(…)` chain across
   three lines; with it each instruction is one line and the file is 187 lines
   rather than 296.

---

## 5. Tests

Nothing here is asserted any more. The stage was developed against a suite of ten
checks in the example — the shell and the single `main:`, one row per instruction
with no holes, a literal's one `li`, a call's closure label and argument ordering,
a `let`'s frame push and pop, an `if`'s branch, the row-equality gates for rename /
literal / annotation, and `the_program_prints_what_the_interpreter_says`, which
ran the listing in a simulated MIPS subset and compared the printed word with
`evaluate` for seven programs including the seed.

That simulator and those tests went when the examples became test-free: an example
is a demonstration, not a fixture, and Cargo cannot import one. What remains is
the claim a reader can check by running the example and comparing two plates:
one row per instruction in program order on `Machine`, and the value on
`Evaluation` beside it.

`scripts/verify.py` passes: format, clippy with warnings denied in both feature
sets, 56 suites, markdown links, doc comments.

---

## 6. Files

- [`../crates/irrigation/examples/stlc_scope_graphs/machine.rs`](../crates/irrigation/examples/stlc_scope_graphs/machine.rs)
  — the View, the shell constant, and `lower`.
- [`../crates/irrigation/examples/stlc_scope_graphs/main.rs`](../crates/irrigation/examples/stlc_scope_graphs/main.rs)
  — the root's shell, entry, and exit.
- [`../crates/irrigation/examples/stlc_scope_graphs/report.rs`](../crates/irrigation/examples/stlc_scope_graphs/report.rs)
  — `From<&str>` and `From<String>` for `Line`.
