//! Unit tests for `src/inner/mod.rs`.

use super::*;

/// A handover with an equal value is a routing change, and the change batch
/// has to say so: that is the only way a consumer can learn its provider was
/// swapped for one publishing the same answer.
#[test]
fn a_handover_with_an_equal_value_reports_a_routing_change() {
    let mut state = State::default();
    let unit = UnitKey::field(ViewId(7), FieldId(0), UnitKind::Value);
    let mut record = NodeRecord::unit("handed over", unit);
    record.value_version = 3;
    record.routing_at = 9;
    let node = state.alloc(record);
    // The previous run observed value version 3 under routing generation 4:
    // the value did not move, the provider did.
    let dep = Dep {
        node,
        version: 3,
        routing: 4,
        validity: 0,
    };
    let changes = observed_changes(
        &state,
        std::slice::from_ref(&dep),
        &FxHashMap::default(),
        &FxHashSet::default(),
    );
    assert_eq!(changes.len(), 1, "a routing move is a change to report");
    assert!(changes[0].routing_changed, "the provider changed");
    assert_eq!(changes[0].previous, 3);
    assert_eq!(changes[0].current, 3);
}

/// A mere value move is not a routing change, so the flag cannot be
/// satisfied by the version comparison it used to perform.
#[test]
fn a_value_move_alone_is_not_a_routing_change() {
    let mut state = State::default();
    let unit = UnitKey::field(ViewId(8), FieldId(0), UnitKind::Value);
    let mut record = NodeRecord::unit("republished", unit);
    record.value_version = 5;
    record.routing_at = 2;
    let node = state.alloc(record);
    let dep = Dep {
        node,
        version: 4,
        routing: 2,
        validity: 0,
    };
    let changes = observed_changes(
        &state,
        std::slice::from_ref(&dep),
        &FxHashMap::default(),
        &FxHashSet::default(),
    );
    assert_eq!(changes.len(), 1);
    assert!(
        !changes[0].routing_changed,
        "the value moved but the provider did not"
    );
}

/// When the value and the provider both moved, the answer is "no": the doc
/// says *while the value stayed equal*, and the value did not. This is the
/// only case where the version comparison and a routing comparison disagree,
/// so it is the one that pins which is meant.
#[test]
fn a_value_move_with_a_handover_is_not_a_routing_change() {
    let mut state = State::default();
    let unit = UnitKey::field(ViewId(9), FieldId(0), UnitKind::Value);
    let mut record = NodeRecord::unit("moved twice", unit);
    record.value_version = 5;
    record.routing_at = 9;
    let node = state.alloc(record);
    let dep = Dep {
        node,
        version: 4,
        routing: 2,
        validity: 0,
    };
    let changes = observed_changes(
        &state,
        std::slice::from_ref(&dep),
        &FxHashMap::default(),
        &FxHashSet::default(),
    );
    assert_eq!(
        changes.len(),
        1,
        "the entry is reported, the flag explains why"
    );
    assert!(
        !changes[0].routing_changed,
        "the provider moved, but so did the value, so this is not a routing-only change"
    );
}

/// Host code the runtime invokes with the metadata lock held must not be
/// able to wait on that lock: a `parking_lot::Mutex` has no reentrancy, so
/// the call would hang the worker forever. It is refused by name instead.
#[test]
fn a_unit_initializer_that_calls_back_is_refused() {
    let inner = Inner::new(RuntimeConfig::default());
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        inner.unit_node_init(
            irrigation_core::ids::ViewId(0x6161),
            irrigation_core::key::any_key(1u32),
            &mut |_content| {
                let _ = inner.memory_report();
                None
            },
        )
    }));
    assert!(
        outcome.is_err(),
        "host code calling back under the metadata lock must be refused, not deadlocked"
    );
}

/// The refusal is scoped to host code: an ordinary initializer still runs.
#[test]
fn an_ordinary_unit_initializer_is_not_refused() {
    let inner = Inner::new(RuntimeConfig::default());
    let first = inner.unit_node_init(
        irrigation_core::ids::ViewId(0x6262),
        irrigation_core::key::any_key(1u32),
        &mut |_content| None,
    );
    // The same unit answers with the same cell, so the guard did not disturb
    // the ordinary path.
    let again = inner.unit_node_init(
        irrigation_core::ids::ViewId(0x6262),
        irrigation_core::key::any_key(1u32),
        &mut |_content| None,
    );
    assert_eq!(first, again);
}

#[test]
fn wait_cycles_reports_a_task_cycle() {
    let inner = Inner::new(RuntimeConfig {
        workers: 1,
        stack_size: 4 * 1024 * 1024,
        ..RuntimeConfig::default()
    });
    let (first, second, task_a, task_b) = {
        let mut state = inner.lock();
        let task_a = state.alloc_task(None, TaskMode::Pooled, false, CancelToken::default());
        let task_b = state.alloc_task(None, TaskMode::Pooled, false, CancelToken::default());
        let first = state.alloc(NodeRecord::empty("first", NodeKind::Unit));
        let second = state.alloc(NodeRecord::empty("second", NodeKind::Unit));
        state.node_mut(first).state = NodeState::Running {
            owner: task_b,
            generation: 1,
        };
        state.node_mut(second).state = NodeState::Running {
            owner: task_a,
            generation: 1,
        };
        state.wait_graph.insert(task_a, first);
        state.wait_graph.insert(task_b, second);
        (first, second, task_a, task_b)
    };
    let _ = (task_a, task_b);
    let cycles = inner.wait_cycles();
    assert_eq!(cycles.len(), 1);
    assert_eq!(cycles[0].len(), 2);
    assert!(cycles[0].contains(&first));
    assert!(cycles[0].contains(&second));
}

#[test]
fn a_pooled_request_suspends_the_whole_task() {
    let inner = Inner::new(RuntimeConfig {
        workers: 1,
        stack_size: 4 * 1024 * 1024,
        ..RuntimeConfig::default()
    });
    let (node, task) = {
        let mut state = inner.lock();
        let node = state.alloc(NodeRecord::empty("unit", NodeKind::Unit));
        state.node_mut(node).state = NodeState::Running {
            owner: 7,
            generation: 1,
        };
        let task = state.alloc_task(None, TaskMode::Pooled, false, CancelToken::default());
        (node, task)
    };
    let _enter = task::enter(inner.local_key, task);
    assert!(inner.suspend_request(node));
    {
        let state = inner.lock();
        assert!(matches!(
            state.tasks[&task].status,
            TaskStatus::Waiting(waited) if waited == node
        ));
        assert!(state.node(node).waiters.contains(&task));
        assert_eq!(state.wait_graph.get(&task), Some(&node));
    }
    let mut waiters = Inner::wake_waiters(&mut inner.lock(), node);
    assert!(waiters.tasks.contains(&task));
    waiters.wake();
}

#[test]
fn a_pending_request_registers_and_wakes_its_waker() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Flag(Arc<AtomicBool>);
    impl std::task::Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let inner = Inner::new(RuntimeConfig {
        workers: 1,
        stack_size: 4 * 1024 * 1024,
        ..RuntimeConfig::default()
    });
    let node = {
        let mut state = inner.lock();
        let node = state.alloc(NodeRecord::empty("unit", NodeKind::Unit));
        state.node_mut(node).state = NodeState::Running {
            owner: 99,
            generation: 1,
        };
        node
    };
    let flag = Arc::new(AtomicBool::new(false));
    let waker = std::task::Waker::from(Arc::new(Flag(Arc::clone(&flag))));
    let poll = inner.try_read_unit(node, &waker).expect("poll");
    assert!(matches!(poll, std::task::Poll::Pending));
    let mut waiters = Inner::wake_waiters(&mut inner.lock(), node);
    waiters.wake();
    assert!(flag.load(Ordering::SeqCst));
}

#[test]
fn trace_validation_rejects_a_routing_change_with_an_equal_value() {
    let inner = Inner::new(RuntimeConfig {
        workers: 1,
        stack_size: 4 * 1024 * 1024,
        ..RuntimeConfig::default()
    });
    let node = {
        let mut state = inner.lock();
        let node = state.alloc(NodeRecord::empty("unit", NodeKind::Unit));
        let record = state.node_mut(node);
        record.value = Some(value_of(42u32));
        record.changed_at = 1;
        record.value_version = 1;
        record.verified_at = 1;
        record.routing_at = 1;
        record.state = NodeState::Ready;
        node
    };
    let trace = vec![Dep {
        node,
        version: 1,
        routing: 1,
        validity: 0,
    }];
    assert!(inner.validate_trace(&trace, 1));
    inner.lock().node_mut(node).routing_at = 2;
    assert!(
        !inner.validate_trace(&trace, 1),
        "an equal value with a new provider must invalidate the observation"
    );
}

#[test]
fn commit_rejects_a_stale_generation() {
    let inner = Inner::new(RuntimeConfig {
        workers: 1,
        stack_size: 4 * 1024 * 1024,
        ..RuntimeConfig::default()
    });
    let node = {
        let mut state = inner.lock();
        let node = state.alloc(NodeRecord::empty("claimed", NodeKind::Operator));
        state.node_mut(node).state = NodeState::Running {
            owner: 1,
            generation: 7,
        };
        node
    };
    let eq = |_: &Value, _: &Value| false;
    let cancel = CancelToken::default();
    let stale = inner.commit(
        node,
        value_of(1u32),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        (Vec::new(), false),
        true,
        true,
        None,
        FxHashMap::default(),
        FxHashSet::default(),
        &eq,
        0,
        1,
        6,
        &cancel,
    );
    assert!(matches!(stale, Err(ComputeError::Internal { .. })));
    let state = inner.lock();
    assert!(
        matches!(
            state.node(node).state,
            NodeState::Running {
                owner: 1,
                generation: 7
            }
        ),
        "a stale commit must not disturb the live claim"
    );
    assert!(state.node(node).value.is_none());
}

#[test]
fn commit_checks_cancellation_under_the_lock() {
    let inner = Inner::new(RuntimeConfig {
        workers: 1,
        stack_size: 4 * 1024 * 1024,
        ..RuntimeConfig::default()
    });
    let node = {
        let mut state = inner.lock();
        let node = state.alloc(NodeRecord::empty("claimed", NodeKind::Operator));
        state.node_mut(node).state = NodeState::Running {
            owner: 1,
            generation: 1,
        };
        node
    };
    let eq = |_: &Value, _: &Value| false;
    let cancel = CancelToken::default();
    cancel.cancel();
    let result = inner.commit(
        node,
        value_of(1u32),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        (Vec::new(), false),
        true,
        true,
        None,
        FxHashMap::default(),
        FxHashSet::default(),
        &eq,
        0,
        1,
        1,
        &cancel,
    );
    assert!(matches!(result, Err(ComputeError::Cancelled)));
    let state = inner.lock();
    assert!(
        matches!(state.node(node).state, NodeState::Vacant),
        "a cancelled commit releases its claim"
    );
}

#[test]
fn retirement_leaves_a_running_node_alone() {
    let inner = Inner::new(RuntimeConfig {
        workers: 1,
        stack_size: 4 * 1024 * 1024,
        ..RuntimeConfig::default()
    });
    let unit = UnitKey::field(ViewId(1), FieldId(0), UnitKind::Value);
    let mut state = inner.lock();
    let node = state.alloc(NodeRecord::empty("running", NodeKind::Operator));
    {
        let record = state.node_mut(node);
        record.state = NodeState::Running {
            owner: 7,
            generation: 1,
        };
        record.value = Some(value_of(7u32));
        record.verified_at = 1;
    }
    state.register_claim(node, 7, WriteScope::entity(&unit), "running", None);
    let mut changed = Vec::new();
    inner.retire_unreachable(&mut state, node, 1, &mut changed);
    let record = state.node(node);
    assert!(record.is_running(), "a live claim must not be retired");
    assert!(record.value.is_some(), "a live claim keeps its value");
    assert!(changed.is_empty());
    // The attempt can still publish, so retirement must leave its claim in
    // place: another writer of the port has to meet this claim, not race it.
    assert!(
        state
            .claim_covering(&unit)
            .is_some_and(|claim| claim.owner == node),
        "retirement released the claim of a task that is still executing"
    );
    assert!(record.claims.contains(&WriteScope::entity(&unit)));
}
