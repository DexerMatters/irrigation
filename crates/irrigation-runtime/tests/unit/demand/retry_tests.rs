//! Unit tests for `src/demand.rs`.

use crate::state::{NodeKind, NodeRecord};
use irrigation_core::ids::Revision;

use super::*;

fn staged(task: u64, owner: NodeId, revision: Revision) -> crate::state::PendingWrite {
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

/// A retried attempt has no drain left for the groups it staged, so they are
/// discarded rather than handed to the next attempt -- which would publish a
/// superseded write set and wake its dependents for work it is about to redo.
#[test]
fn a_retried_attempt_discards_its_staged_groups() {
    let mut state = State {
        revision: 3,
        ..State::default()
    };
    let owner = state.alloc(NodeRecord::empty("owner", NodeKind::Unit));
    state.pending_writes.push(staged(7, owner, 3));
    state.pending_writes.push(staged(8, owner, 3));

    Inner::reassign_pending_writes(&mut state, 7, None);

    assert_eq!(
        state.pending_writes.len(),
        1,
        "only the other task's group survives"
    );
    assert_eq!(state.pending_writes[0].task, 8);
    assert!(
        state.node(owner).suspect_at >= 3,
        "the discarded group's owner is marked suspect again"
    );
}

/// A terminal attempt hands its groups to the enclosing demand instead of
/// dropping them: that demand is the drain.
#[test]
fn a_terminal_attempt_hands_its_groups_to_the_parent() {
    let mut state = State {
        revision: 2,
        ..State::default()
    };
    let owner = state.alloc(NodeRecord::empty("owner", NodeKind::Unit));
    state.pending_writes.push(staged(4, owner, 2));

    Inner::reassign_pending_writes(&mut state, 4, Some(9));

    assert_eq!(state.pending_writes.len(), 1);
    assert_eq!(
        state.pending_writes[0].task, 9,
        "the enclosing demand now owns the group"
    );
}
