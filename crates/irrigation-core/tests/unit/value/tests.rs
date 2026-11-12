//! Unit tests for `src/value.rs`.

use super::*;

#[test]
fn round_trips_concrete_values() {
    let value = value_of(41u32);
    assert_eq!(value_downcast::<u32>(&value), Some(&41));
    assert_eq!(value_clone::<u32>(&value), Some(41));
    assert_eq!(value_downcast::<u64>(&value), None);
}

#[test]
fn cloning_a_value_shares_the_payload() {
    let value = value_of(String::from("shared"));
    let clone = Arc::clone(&value);
    assert_eq!(value_strong_count(&value), 2);
    drop(clone);
    assert_eq!(value_strong_count(&value), 1);
}
