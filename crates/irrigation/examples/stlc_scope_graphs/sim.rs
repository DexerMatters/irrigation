//! A simulator for the subset of MIPS the lowering emits.
//!
//! It exists to make the instruction table a claim rather than a drawing: the
//! tests run a listing and compare what it prints with what `evaluate` says. It
//! supports exactly the instructions `machine::lower` writes, plus the directives
//! it writes for them — nothing else, and no attempt at being an assembler.

use std::collections::HashMap;

const HEAP: i32 = 0x4000;
const STACK_TOP: i32 = 0x3_0000;
const MEMORY_WORDS: usize = 0x4_0000 / 4;
const STEP_LIMIT: u64 = 2_000_000;

/// What one run printed.
#[derive(Debug, Default)]
pub struct Run {
    /// The words the program printed, in order.
    pub printed: Vec<i32>,
    /// Instructions retired.
    pub steps: u64,
}

/// Runs a listing.
///
/// # Panics
///
/// Panics on an unsupported instruction or on a program that does not halt; both
/// mean the lowering emitted something the simulator was not told about.
#[must_use]
pub fn run(program: &[String]) -> Run {
    let mut labels: HashMap<String, usize> = HashMap::new();
    let mut code: Vec<&str> = Vec::new();
    for line in program {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('.') {
            continue;
        }
        // A line is a label, a data label followed by a directive, or an
        // instruction. `heap: .space 65536` is the second kind.
        if let Some((label, rest)) = line.split_once(':') {
            let rest = rest.trim();
            if rest.is_empty() {
                labels.insert(label.trim().to_string(), code.len());
                continue;
            }
            if rest.starts_with('.') {
                continue;
            }
        }
        code.push(line);
    }

    let mut registers = [0i32; 32];
    let mut memory = vec![0i32; MEMORY_WORDS];
    registers[register("$sp")] = STACK_TOP;
    let mut run = Run::default();
    let mut pc = 0usize;
    while pc < code.len() {
        let mut parts = code[pc].split_whitespace();
        let op = parts.next().unwrap_or_default();
        let args: Vec<&str> = parts.collect();
        let mut next = pc + 1;
        match op {
            "li" => registers[register(args[0])] = number(args[1]),
            "la" => {
                // `la $t1, <label>` is written for two kinds of name: a code
                // label (a jump target) and `heap` (an address).
                let target = args[1];
                let value = match labels.get(target) {
                    Some(label) if target != "heap" => *label as i32,
                    _ => HEAP,
                };
                registers[register(args[0])] = value;
            }
            "move" => registers[register(args[0])] = registers[register(args[1])],
            "lw" => {
                let (offset, base) = memory_operand(args[1]);
                let address = registers[base] + offset;
                registers[register(args[0])] = memory[word(address)];
            }
            "sw" => {
                let (offset, base) = memory_operand(args[1]);
                let address = registers[base] + offset;
                memory[word(address)] = registers[register(args[0])];
            }
            "addiu" => {
                registers[register(args[0])] =
                    registers[register(args[1])].wrapping_add(number(args[2]));
            }
            "b" => next = target(&labels, args[0]),
            "beq" => {
                if registers[register(args[0])] == registers[register(args[1])] {
                    next = target(&labels, args[2]);
                }
            }
            "jalr" => {
                registers[register("$ra")] = next as i32;
                next = registers[register(args[0])] as usize;
            }
            "jr" => next = registers[register(args[0])] as usize,
            "syscall" => match registers[register("$v0")] {
                1 => run.printed.push(registers[register("$a0")]),
                10 => break,
                other => panic!("unsupported syscall {other}"),
            },
            other => panic!("unsupported instruction `{other}`"),
        }
        run.steps += 1;
        assert!(
            run.steps < STEP_LIMIT,
            "the program did not halt within {STEP_LIMIT} instructions"
        );
        pc = next;
    }
    run
}

fn target(labels: &HashMap<String, usize>, label: &str) -> usize {
    labels
        .get(label)
        .copied()
        .unwrap_or_else(|| panic!("undefined label `{label}`"))
}

fn word(address: i32) -> usize {
    usize::try_from(address).expect("a positive address") / 4
}

/// `0($sp)` as `(offset, base register)`.
fn memory_operand(text: &str) -> (i32, usize) {
    let text = text.trim_end_matches(',');
    let (offset, base) = text
        .split_once('(')
        .unwrap_or_else(|| panic!("unsupported operand `{text}`"));
    (number(offset), register(base.trim_end_matches(')')))
}

fn number(text: &str) -> i32 {
    let text = text.trim_end_matches(',');
    text.parse()
        .unwrap_or_else(|_| panic!("unsupported number `{text}`"))
}

fn register(name: &str) -> usize {
    let name = name.trim_end_matches(',');
    let index = match name {
        "$zero" => 0,
        "$v0" => 2,
        "$v1" => 3,
        "$a0" => 4,
        "$a1" => 5,
        "$a2" => 6,
        "$a3" => 7,
        "$t0" => 8,
        "$t1" => 9,
        "$t2" => 10,
        "$t3" => 11,
        "$t4" => 12,
        "$t5" => 13,
        "$t6" => 14,
        "$t7" => 15,
        "$s0" => 16,
        "$s1" => 17,
        "$s2" => 18,
        "$s3" => 19,
        "$ra" => 31,
        "$sp" => 29,
        other => panic!("unsupported register `{other}`"),
    };
    index
}
