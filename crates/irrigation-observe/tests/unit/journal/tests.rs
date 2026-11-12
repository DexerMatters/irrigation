//! Unit tests for `src/journal.rs`.

use super::*;
use irrigation_runtime::observe::DemandCause;

fn committed(revision: Revision, changed: u32) -> RuntimeEvent {
    RuntimeEvent::TransactionCommitted {
        label: "seed".into(),
        revision,
        staged: changed,
        changed,
        published: true,
        invalidated: 3,
        views: Default::default(),
        duration_ns: 10,
    }
}

#[test]
fn transactions_are_recorded_and_noops_are_separate() {
    let mut journal = Journal::new(4, ObserveLevel::Nodes);
    journal.apply(&committed(1, 2));
    journal.apply(&RuntimeEvent::TransactionCommitted {
        label: "noop".into(),
        revision: 1,
        staged: 1,
        changed: 0,
        published: false,
        invalidated: 0,
        views: Default::default(),
        duration_ns: 1,
    });
    assert_eq!(journal.current(), Some(1));
    assert_eq!(journal.record(1).expect("record").changed, 2);
    assert_eq!(
        journal.last_noop().map(|(label, _)| label.as_str()),
        Some("noop")
    );
}

#[test]
fn eviction_keeps_the_newest_records() {
    let mut journal = Journal::new(2, ObserveLevel::Nodes);
    for revision in 1..=5 {
        journal.apply(&committed(revision, revision as u32));
    }
    assert_eq!(journal.len(), 2);
    assert_eq!(journal.oldest(), Some(4));
    assert!(journal.record(1).is_none());
}

#[test]
fn activity_is_bounded_and_marked_capped() {
    let mut journal = Journal::new(4, ObserveLevel::Nodes);
    journal.activity_cap = 2;
    journal.apply(&committed(1, 0));
    for index in 0..5 {
        journal.apply(&RuntimeEvent::NodeCommitted {
            node: NodeId::new(index, 0),
            views: std::iter::once(ViewId(7)).collect(),
            equal_views: Default::default(),
            changed: true,
            invalidated: 0,
            definitions: 0,
            duration_ns: 1,
        });
    }
    let record = journal.record(1).expect("record");
    assert_eq!(record.activity.executed.len(), 2);
    assert!(record.activity.capped);
    assert!(record.changed_views.contains(&ViewId(7)));
}

#[test]
fn demand_causes_are_distinct() {
    let mut journal = Journal::new(4, ObserveLevel::Summary);
    journal.apply(&committed(1, 0));
    for cause in [DemandCause::HostRead, DemandCause::HostRead] {
        journal.apply(&RuntimeEvent::DemandStarted { task: 1, cause });
    }
    journal.apply(&RuntimeEvent::DemandStarted {
        task: 2,
        cause: DemandCause::HostSnapshot,
    });
    let record = journal.record(1).expect("record");
    assert_eq!(record.demands, 2);
    assert_eq!(record.demand_causes.len(), 1);
}
