//! What observation costs on the same workload the scaling example measures.
//!
//! Run with
//! `cargo run --release --example observe_cost -p irrigation --features observatory`.
//!
//! Each level runs the identical point edit on a large list and reports the
//! median transaction+read time over several rounds. The ratio against `off` is
//! the overhead figure the plan's G7 gate asks for.

use std::sync::Arc;
use std::time::Instant;

use irrigation::{Runtime, RuntimeConfig, irrigation, view};
use irrigation_observe::{ObserveLevel, recorder};

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

const SIZE: usize = 10_000;
const ROUNDS: usize = 25;

fn run(level: ObserveLevel) -> (f64, u64) {
    let runtime = match level {
        ObserveLevel::Off => Runtime::with_config(RuntimeConfig::default()),
        level => {
            let recorder = recorder(level);
            let observer: Arc<dyn irrigation_observe::RuntimeObserver> =
                Arc::clone(&recorder) as Arc<_>;
            Runtime::with_observer(RuntimeConfig::default(), observer)
        }
    };
    runtime.install(bench::Doubled).expect("install");
    runtime
        .edit(|edit| {
            let mut numbers = edit.view::<Numbers>();
            let mut values = numbers.values();
            for value in 0..SIZE as u64 {
                values.push(value);
            }
        })
        .expect("seed");
    let _ = runtime.read_outputs::<bench::Doubled>().expect("warm");

    let mut samples = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        let started = Instant::now();
        runtime
            .edit(|edit| {
                let mut numbers = edit.view::<Numbers>();
                let mut values = numbers.values();
                values.set(SIZE / 2, 1_000_000 + round as u64);
            })
            .expect("edit");
        let _ = runtime.read_outputs::<bench::Doubled>().expect("read");
        samples.push(started.elapsed().as_secs_f64() * 1e6);
    }
    samples.sort_by(f64::total_cmp);
    (samples[samples.len() / 2], runtime.stats().executions)
}

fn main() {
    println!("level\tmedian_us\texecutions");
    let mut baseline = None;
    for level in [
        ObserveLevel::Off,
        ObserveLevel::Summary,
        ObserveLevel::Nodes,
        ObserveLevel::Full,
    ] {
        let (median, executions) = run(level);
        let ratio = baseline.map_or(0.0, |base: f64| (median / base - 1.0) * 100.0);
        let label = if level == ObserveLevel::Off {
            String::from("off")
        } else {
            format!("{}+{ratio:.1}%", level.as_str())
        };
        if level == ObserveLevel::Off {
            baseline = Some(median);
        }
        println!("{label}\t{median:.1}\t{executions}");
    }
}
