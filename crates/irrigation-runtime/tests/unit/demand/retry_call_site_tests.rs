//! Unit tests for `src/demand.rs`.

use super::*;
use crate::state::{NodeKind, NodeRecord, TaskSlot};
use crate::task::{TaskMode, TaskStatus};

/// A body that never runs: `finish_attempt` only needs one to hand back.
struct Idle;

impl Task for Idle {
    fn attempt(&mut self) -> Attempt {
        Attempt::Done
    }

    fn deliver(self: Box<Self>) {}

    fn fail(self: Box<Self>, _error: irrigation_core::error::ComputeError) {}
}

fn pending(task: u64, owner: NodeId, revision: u64) -> crate::state::PendingWrite {
    crate::state::PendingWrite {
        owner,
        task,
        revision,
        generation: 0,
        drafts: Vec::new(),
        retractions: Vec::new(),
        restate_all: false,
        enforce_views: false,
        requirements: Vec::new(),
        trace: Vec::new(),
        claims: Vec::new(),
    }
}

/// The **retry branch** discards the attempt's staged groups, not only the helper.
///
/// The existing test drives `reassign_pending_writes` directly, so it passes whether or not
/// `finish_attempt` calls it. This drives the transition: a queued task with a staged group
/// finishes its attempt as `Retry`, and the group is gone afterwards.
#[test]
fn a_retried_attempt_drops_its_groups_at_the_transition() {
    let inner = Inner::new(crate::RuntimeConfig {
        workers: 0,
        ..crate::RuntimeConfig::default()
    });
    let id: TaskId = 1;
    let owner = {
        let mut state = inner.lock();
        let owner = state.alloc(NodeRecord::empty("owner", NodeKind::Unit));
        let revision = state.revision;
        state.tasks.insert(
            id,
            TaskSlot {
                body: Some(Box::new(Idle)),
                status: TaskStatus::Running,
                mode: TaskMode::Inline,
                gate: false,
                cancel: CancelToken::default(),
                held: Vec::new(),
                retries: 0,
            },
        );
        state.pending_writes.push(pending(1, owner, revision));
        owner
    };

    inner.finish_attempt(id, Box::new(Idle), Attempt::Retry);

    let state = inner.lock();
    assert!(
        state.pending_writes.iter().all(|write| write.task != 1),
        "the retried attempt's group is discarded at the transition"
    );
    let _ = owner;
}
