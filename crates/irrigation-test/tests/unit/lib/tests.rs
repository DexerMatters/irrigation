//! Unit tests for `src/lib.rs`.

use super::*;

/// The generator is `StdRng`, not the linear congruential one this test was named for.
#[test]
fn the_seeded_rng_replays_the_same_sequence() {
    let mut first = TestRng::new(7);
    let mut second = TestRng::new(7);
    for _ in 0..16 {
        assert_eq!(first.next_u64(), second.next_u64());
    }
}

#[test]
fn bounded_draws_stay_in_range() {
    let mut rng = TestRng::new(1);
    for _ in 0..100 {
        assert!(rng.below(10) < 10);
        let value = rng.range(5, 9);
        assert!((5..9).contains(&value));
    }
    assert_eq!(rng.below(0), 0);
}
