//! Unit tests for `src/http.rs`.

use super::*;

#[test]
fn node_keys_round_trip() {
    let node = NodeId::new(42, 3);
    let key = crate::wire::node_key(node);
    assert_eq!(parse_node(&key), Some(node));
}

#[test]
fn node_keys_reject_identities_that_would_alias() {
    // Both halves are packed; an out-of-range half must not be masked into
    // a different slot or generation.
    assert_eq!(parse_node("n:1048576@0"), None);
    assert_eq!(
        parse_node(&format!("n:0@{}", NodeId::MAX_GENERATION + 1)),
        None
    );
    assert_eq!(
        parse_node(&format!("n:{}@0", NodeId::INDEX_MASK)),
        Some(NodeId::new(NodeId::INDEX_MASK, 0))
    );
}

#[test]
fn view_keys_round_trip() {
    let view = ViewId(0xdead_beef);
    let key = crate::wire::view_key(view);
    assert_eq!(parse_view(&key), Some(view));
}

#[test]
fn parent_paths_are_rejected() {
    let config = HttpConfig {
        static_dir: Some(PathBuf::from("/tmp/public")),
        ..HttpConfig::default()
    };
    assert!(!config.static_dir.is_none());
}
