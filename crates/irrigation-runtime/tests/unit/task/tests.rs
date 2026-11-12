//! Unit tests for `src/task.rs`.

use super::*;

/// `TaskId` is allocated per runtime, so the thread's current task can be
/// another runtime's. A caller asking "am I inside one of my tasks" must not be
/// answered by a task of the same number in a different runtime.
#[test]
fn a_task_id_from_another_runtime_does_not_answer() {
    let _enter = enter(1, 7);
    assert_eq!(current_in(1), Some(7));
    assert_eq!(current_in(2), None, "another runtime's stack is not mine");
    assert_eq!(
        current(),
        Some(7),
        "the unkeyed reading still reports the thread's task"
    );
}

#[test]
fn nested_tasks_restore_the_enclosing_one() {
    let outer = enter(1, 1);
    assert_eq!(current(), Some(1));
    active_push(NodeId(1));
    {
        let _inner = enter(1, 2);
        assert_eq!(current(), Some(2));
        assert!(active_snapshot().is_empty());
        active_push(NodeId(2));
    }
    assert_eq!(current(), Some(1));
    assert_eq!(active_snapshot(), vec![NodeId(1)]);
    active_pop(NodeId(1));
    drop(outer);
    assert_eq!(current(), None);
}

#[test]
fn detached_identity_is_stable_per_thread() {
    let first = current_or_detached();
    assert_eq!(first, current_or_detached());
    assert!(first >= 1 << 63);
    assert_eq!(current(), Some(first));
    active_push(NodeId(3));
    assert!(active_contains(NodeId(3)));
    active_pop(NodeId(3));
    let other = std::thread::spawn(current_or_detached)
        .join()
        .expect("thread");
    assert_ne!(first, other);
}

#[test]
fn detached_claims_are_taken_from_their_mark() {
    let _ = take_detached_claims_since(0);
    let outer = detached_claim_mark();
    hold_detached_claim(NodeId(4));
    let inner = detached_claim_mark();
    hold_detached_claim(NodeId(5));
    hold_detached_claim(NodeId(5));
    assert_eq!(take_detached_claims_since(inner), vec![NodeId(5)]);
    assert_eq!(take_detached_claims_since(outer), vec![NodeId(4)]);
    assert!(take_detached_claims_since(outer).is_empty());
    hold_detached_claim(NodeId(6));
    forget_detached_claim(NodeId(6));
    assert!(take_detached_claims_since(outer).is_empty());
}
