//! Unit tests for `src/key.rs`.

use super::*;

#[test]
fn keys_of_different_types_never_collide_semantically() {
    let mut table = KeyTable::new();
    table.insert(any_key(7u32), "u32");
    table.insert(any_key(7u64), "u64");
    assert_eq!(table.get(any_key(7u32).as_ref()), Some(&"u32"));
    assert_eq!(table.get(any_key(7u64).as_ref()), Some(&"u64"));
    assert_eq!(table.len(), 2);
}

#[test]
fn removal_requires_the_same_concrete_key() {
    let mut table = KeyTable::new();
    table.insert(any_key(String::from("a")), 1);
    assert!(table.remove(any_key(String::from("b")).as_ref()).is_none());
    assert_eq!(
        table
            .remove(any_key(String::from("a")).as_ref())
            .map(|(_, v)| v),
        Some(1)
    );
    assert!(table.is_empty());
}
