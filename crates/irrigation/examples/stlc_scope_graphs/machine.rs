//! The MIPS backend.
//!
//! One component, [`lower`], turns the declared tree into a listing: a `match` on
//! the term's kind whose arms write their instructions at the row they are given,
//! recurse into their children at the offsets they compute, and return the first
//! free row. Each row is one unit, written where it is emitted, so a rename
//! re-versions nothing and a literal edit re-versions one row.
//!
//! The convention, in one paragraph. `$v0` holds the value of the last
//! expression; `$s0` is the current environment frame; `$a0` and `$a1` carry the
//! callee's argument and the frame its closure captured; `$sp` saves one word at
//! a time; `$t3` is the heap bump pointer (every callee advances it, and nothing
//! else writes it). A frame is `{value, parent}` and a closure is `{code, env}`,
//! eight bytes each out of a fixed `.data` block. Values are untagged: the type
//! checker is the authority, exactly as the evaluator is untyped.

use crate::report::Line;
use crate::syntax::{TermKind, TermRef};

/// The listing: one row per instruction position, in program order.
#[irrigation::incremental_data]
pub struct Machine {
    /// One unit per row, keyed by its position.
    pub program: Map<u32, Line>,
}

/// The shell's length: `.data` through `la $t3, heap`.
pub const SHELL: u32 = 7;

/// Lowers one term to MIPS at row `at`, returning the first free row.
///
/// The chain is the binders in scope, innermost last: a reference's depth is how
/// far back its binder sits, so the code for a term depends on the term and its
/// chain and nothing else. A rename re-runs this invocation and writes the same
/// instructions at the same rows, because the depth does not spell the name.
#[irrigation::component(callable)]
pub fn lower(term: TermRef, chain: Vec<TermRef>, at: u32) -> u32 {
    let program = Machine.program();
    match term.kind() {
        Some(TermKind::Var) => {
            let name = term.name().unwrap_or_default();
            let depth = chain
                .iter()
                .rposition(|frame| frame.name().as_deref() == Some(name.as_str()))
                .map(|index| chain.len() - 1 - index);
            match depth {
                Some(depth) => {
                    let walk = format!("walk_{}", term.raw());
                    let end = format!("end_{}", term.raw());
                    program.define(at, "move $t0, $s0".into(), "lower");
                    program.define(at + 1, format!("li $t2, {depth}").into(), "lower");
                    program.define(at + 2, format!("{walk}:").into(), "lower");
                    program.define(at + 3, format!("beq $t2, $zero, {end}").into(), "lower");
                    program.define(at + 4, "lw $t0, 4($t0)".into(), "lower");
                    program.define(at + 5, "addiu $t2, $t2, -1".into(), "lower");
                    program.define(at + 6, format!("b {walk}").into(), "lower");
                    program.define(at + 7, format!("{end}:").into(), "lower");
                    program.define(at + 8, "lw $v0, 0($t0)".into(), "lower");
                    at + 9
                }
                None => {
                    program.define(at, format!("# unresolved `{name}`").into(), "lower");
                    program.define(at + 1, "li $v0, 0".into(), "lower");
                    at + 2
                }
            }
        }
        Some(TermKind::Int) => {
            let value = term.int().unwrap_or_default();
            match i32::try_from(value) {
                Ok(value) => {
                    program.define(at, format!("li $v0, {value}").into(), "lower");
                    at + 1
                }
                Err(_) => {
                    program.define(at, format!("# {value} does not fit a word").into(), "lower");
                    program.define(at + 1, "li $v0, 0".into(), "lower");
                    at + 2
                }
            }
        }
        Some(TermKind::Bool) => {
            let value = u8::from(term.flag().unwrap_or_default());
            program.define(at, format!("li $v0, {value}").into(), "lower");
            at + 1
        }
        Some(TermKind::App) => {
            let after_func = match term.func() {
                Some(func) => lower(func, chain.clone(), at),
                None => at,
            };
            program.define(after_func, "addiu $sp, $sp, -4".into(), "lower");
            program.define(after_func + 1, "sw $v0, 0($sp)".into(), "lower");
            let after_arg = match term.arg() {
                Some(arg) => lower(arg, chain, after_func + 2),
                None => after_func + 2,
            };
            program.define(after_arg, "move $a0, $v0".into(), "lower");
            program.define(after_arg + 1, "lw $t0, 0($sp)".into(), "lower");
            program.define(after_arg + 2, "addiu $sp, $sp, 4".into(), "lower");
            program.define(after_arg + 3, "lw $a1, 4($t0)".into(), "lower");
            program.define(after_arg + 4, "lw $t1, 0($t0)".into(), "lower");
            program.define(after_arg + 5, "jalr $t1".into(), "lower");
            after_arg + 6
        }
        Some(TermKind::Let) => {
            let after_value = match term.value() {
                Some(value) => lower(value, chain.clone(), at),
                None => at,
            };
            program.define(after_value, "move $t0, $t3".into(), "lower");
            program.define(after_value + 1, "sw $v0, 0($t0)".into(), "lower");
            program.define(after_value + 2, "sw $s0, 4($t0)".into(), "lower");
            program.define(after_value + 3, "addiu $t3, $t3, 8".into(), "lower");
            program.define(after_value + 4, "move $s0, $t0".into(), "lower");
            let next = chain.iter().cloned().chain([term.clone()]).collect();
            let after_body = match term.body() {
                Some(body) => lower(body, next, after_value + 5),
                None => after_value + 5,
            };
            program.define(after_body, "lw $s0, 4($s0)".into(), "lower");
            after_body + 1
        }
        Some(TermKind::Lam) => {
            let id = term.raw();
            program.define(at, "move $t0, $t3".into(), "lower");
            program.define(at + 1, format!("la $t1, lam_{id}").into(), "lower");
            program.define(at + 2, "sw $t1, 0($t0)".into(), "lower");
            program.define(at + 3, "sw $s0, 4($t0)".into(), "lower");
            program.define(at + 4, "addiu $t3, $t3, 8".into(), "lower");
            program.define(at + 5, "move $v0, $t0".into(), "lower");
            program.define(at + 6, format!("b done_{id}").into(), "lower");
            program.define(at + 7, format!("lam_{id}:").into(), "lower");
            program.define(at + 8, "addiu $sp, $sp, -8".into(), "lower");
            program.define(at + 9, "sw $ra, 4($sp)".into(), "lower");
            program.define(at + 10, "sw $s0, 0($sp)".into(), "lower");
            program.define(at + 11, "move $t0, $t3".into(), "lower");
            program.define(at + 12, "sw $a0, 0($t0)".into(), "lower");
            program.define(at + 13, "sw $a1, 4($t0)".into(), "lower");
            program.define(at + 14, "addiu $t3, $t3, 8".into(), "lower");
            program.define(at + 15, "move $s0, $t0".into(), "lower");
            let next = chain.iter().cloned().chain([term.clone()]).collect();
            let after_body = match term.body() {
                Some(body) => lower(body, next, at + 16),
                None => at + 16,
            };
            program.define(after_body, "lw $s0, 0($sp)".into(), "lower");
            program.define(after_body + 1, "lw $ra, 4($sp)".into(), "lower");
            program.define(after_body + 2, "addiu $sp, $sp, 8".into(), "lower");
            program.define(after_body + 3, "jr $ra".into(), "lower");
            program.define(after_body + 4, format!("done_{id}:").into(), "lower");
            after_body + 5
        }
        Some(TermKind::If) => {
            let id = term.raw();
            let after_cond = match term.cond() {
                Some(cond) => lower(cond, chain.clone(), at),
                None => at,
            };
            program.define(
                after_cond,
                format!("beq $v0, $zero, else_{id}").into(),
                "lower",
            );
            let after_yes = match term.yes() {
                Some(yes) => lower(yes, chain.clone(), after_cond + 1),
                None => after_cond + 1,
            };
            program.define(after_yes, format!("b done_{id}").into(), "lower");
            program.define(after_yes + 1, format!("else_{id}:").into(), "lower");
            let after_no = match term.no() {
                Some(no) => lower(no, chain, after_yes + 2),
                None => after_yes + 2,
            };
            program.define(after_no, format!("done_{id}:").into(), "lower");
            after_no + 1
        }
        None => {
            program.define(
                at,
                format!("# term {} has no declared variant", term.raw()).into(),
                "lower",
            );
            at + 1
        }
    }
}
