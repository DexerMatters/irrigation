//! Balanced lawful reduction scaling (plan §5.3, §14).
//!
//! Run with `cargo run --release --example reduction -p reactive`.
//!
//! A point edit in a balanced sequence with a cached aggregate recomputes only
//! the ancestor path. The combine count should grow logarithmically with size.

use std::sync::atomic::{AtomicU64, Ordering};

use reactive::AggSeq;

static COMBINES: AtomicU64 = AtomicU64::new(0);

fn leaf(value: &u64) -> u64 {
    *value
}

fn add(left: &u64, right: &u64) -> u64 {
    COMBINES.fetch_add(1, Ordering::Relaxed);
    left + right
}

fn main() {
    println!("size\tcombines(set)\tcombines(insert)\taggregate");
    for size in [1_000usize, 10_000, 100_000, 1_000_000] {
        let mut seq = AggSeq::new(leaf, add, 0);
        for value in 0..size as u64 {
            seq = seq.push(value);
        }
        let expected: u64 = (0..size as u64).sum();

        COMBINES.store(0, Ordering::Relaxed);
        let edited = seq.set(size / 2, 1);
        let set_combines = COMBINES.load(Ordering::Relaxed);

        COMBINES.store(0, Ordering::Relaxed);
        let inserted = edited.insert(size / 2, 7);
        let insert_combines = COMBINES.load(Ordering::Relaxed);

        assert_eq!(edited.aggregate(), expected - (size as u64 / 2) + 1);
        assert_eq!(
            inserted.aggregate(),
            expected - (size as u64 / 2) + 1 + 7
        );
        println!(
            "{size}\t{set_combines}\t{insert_combines}\t{}",
            inserted.aggregate()
        );
        seq = inserted;
    }
}
