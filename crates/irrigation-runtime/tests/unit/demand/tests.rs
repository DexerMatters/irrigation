//! Unit tests for `src/demand.rs`.

use super::*;
use crate::inner::RuntimeConfig;
use crate::state::{NodeKind, NodeRecord};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

/// A host waker that fails the way a sandboxed callback can: mid-teardown.
struct PanickingWaker;

impl std::task::Wake for PanickingWaker {
    fn wake(self: Arc<Self>) {
        panic!("the host waker panicked");
    }
}

#[test]
fn a_panicking_waker_does_not_leak_the_task_gate_permit() {
    let inner = Inner::new(RuntimeConfig {
        workers: 1,
        stack_size: 4 * 1024 * 1024,
        ..RuntimeConfig::default()
    });
    // The read permit a pooled demand takes before it allocates its task.
    inner.gate.enter_read();
    let task = {
        let mut state = inner.lock();
        let node = state.alloc(NodeRecord::empty("unit", NodeKind::Unit));
        let task = state.alloc_task(None, TaskMode::Pooled, true, CancelToken::default());
        state.node_mut(node).state = NodeState::Running {
            owner: task,
            generation: 1,
        };
        let slot = state.tasks.get_mut(&task).expect("task was just allocated");
        slot.held.push(node);
        state
            .node_mut(node)
            .wakers
            .push(std::task::Waker::from(Arc::new(PanickingWaker)));
        task
    };
    // The slot is gone by the time the waker runs, so this unwind is the
    // only path left that can release the permit.
    let closed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        inner.close_task(task, None)
    }));
    assert!(closed.is_err(), "the teardown must dial the waker it woke");

    // Every edit takes the write side, which waits for a leaked permit.
    let (sender, receiver) = mpsc::channel();
    let writer = Arc::clone(&inner);
    std::thread::spawn(move || {
        let _guard = writer.gate.write();
        let _ = sender.send(());
    });
    assert!(
        receiver.recv_timeout(Duration::from_secs(10)).is_ok(),
        "a panicking waker leaked the task's gate permit; no edit can start"
    );
}
