//! Worker scaling: independent list members execute on chunk threads.
//!
//! Run with `cargo run --release --example parallel -p reactive`.
//!
//! Each member of the mapped list burns a fixed amount of CPU. The list is
//! re-evaluated by changing the scalar factor, so every member executes on every
//! iteration; only the worker count changes between rows.

use std::time::Instant;

use reactive::{reactive, view, Runtime};

const MEMBERS: u64 = 1_000;
const WORK: u64 = 40_000;

#[view(source)]
pub struct Numbers {
    values: List<u64>,
    factor: u64,
}

fn burn(seed: u64, factor: u64) -> u64 {
    let mut state = seed
        .wrapping_mul(factor.wrapping_mul(2).wrapping_add(1))
        .wrapping_add(factor);
    for _ in 0..WORK {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
    }
    state
}

#[reactive]
mod scaling {
    use super::{burn, Numbers};

    #[component]
    pub fn total(numbers: &Numbers) -> u64 {
        let values = numbers.values();
        let factor = numbers.factor();
        values.map(|value| burn(*value, factor)).iter().sum()
    }
}

fn main() {
    println!("workers\tmedian_micros\tspeedup");
    let mut baseline = 0.0f64;
    for workers in [1usize, 2, 4] {
        let runtime = Runtime::with_config(reactive::RuntimeConfig {
            workers,
            stack_size: 16 * 1024 * 1024,
            ..reactive::RuntimeConfig::default()
        });
        runtime.install(scaling::Total).expect("install");
        runtime
            .edit(|edit| {
                let mut numbers = edit.view::<Numbers>();
                {
                    let mut values = numbers.values();
                    for index in 0..MEMBERS {
                        values.push(index);
                    }
                }
                numbers.set_factor(1);
            })
            .expect("seed");
        let expected_warm = runtime.read_outputs::<scaling::Total>().expect("warm");
        // Warm, then re-evaluate every member by changing the factor.
        let mut samples = Vec::new();
        for round in 0..5u64 {
            let factor = round + 2;
            runtime
                .edit(|edit| {
                    edit.view::<Numbers>().set_factor(factor);
                })
                .expect("edit");
            let started = Instant::now();
            let total = runtime.read_outputs::<scaling::Total>().expect("read");
            samples.push(started.elapsed().as_secs_f64() * 1e6);
            let expected: u64 = (0..MEMBERS).map(|value| burn(value, factor)).sum();
            assert_eq!(total, expected);
            assert_ne!(total, expected_warm);
        }
        samples.sort_by(f64::total_cmp);
        let median = samples[samples.len() / 2];
        if workers == 1 {
            baseline = median;
        }
        println!("{workers}\t{median:.0}\t{:.2}", baseline / median);
    }
}
