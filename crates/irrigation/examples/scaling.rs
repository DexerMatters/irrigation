//! Scaling demonstration for the acceptance criteria in plan §14.
//!
//! Run with `cargo run --release --example scaling -p irrigation`.
//!
//! It reports, for a point edit at increasing list sizes:
//!
//! * primitive executions caused by the edit;
//! * trace validations performed;
//! * wall time for the transaction plus the observer read.
//!
//! The first number is the important one: identity-based mapped work must not
//! grow with the untouched list length.

use std::time::Instant;

use irrigation::{Runtime, irrigation, view};

#[view(source)]
pub struct Numbers {
    values: List<u64>,
}

#[irrigation]
mod bench {
    use super::Numbers;

    #[component]
    pub fn doubled(numbers: &Numbers) -> u64 {
        let values = numbers.values();
        let doubled = values.map(|value| *value * 2);
        doubled.iter().sum::<u64>()
    }
}

fn main() {
    println!("size\texecutions\tvalidations\tmicros");
    for size in [100usize, 1_000, 10_000, 50_000] {
        let runtime = Runtime::new();
        runtime.install(bench::Doubled).expect("install");
        runtime
            .edit(|edit| {
                let mut numbers = edit.view::<Numbers>();
                let mut values = numbers.values();
                for value in 0..size as u64 {
                    values.push(value);
                }
            })
            .expect("seed");
        let _ = runtime.read_outputs::<bench::Doubled>().expect("warm");

        let before = runtime.stats();
        let started = Instant::now();
        runtime
            .edit(|edit| {
                let mut numbers = edit.view::<Numbers>();
                let mut values = numbers.values();
                values.set(size / 2, 1_000_000);
            })
            .expect("edit");
        let _ = runtime.read_outputs::<bench::Doubled>().expect("read");
        let elapsed = started.elapsed();
        let after = runtime.stats();

        println!(
            "{size}\t{}\t{}\t{}",
            after.executions - before.executions,
            after.validations - before.validations,
            elapsed.as_micros()
        );
    }
}
