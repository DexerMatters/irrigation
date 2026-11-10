//! What actually happened, per revision.
//!
//! The runtime keeps current facts, not history: a node record says what its
//! version is now, not what it was at revision 120. The journal therefore
//! records only what the event stream reported. A page may replay activity from
//! it, but it must never derive an old change from a current node field.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use irrigation_core::ids::{NodeId, Revision, ViewId};
use irrigation_runtime::observe::{ObserveLevel, RuntimeEvent};

/// Node activity for one revision, capped.
#[derive(Clone, Debug, Default)]
pub struct Activity {
    /// Nodes whose closure committed.
    pub executed: HashSet<NodeId>,
    /// Nodes whose value changed.
    pub changed: HashSet<NodeId>,
    /// Nodes whose closure ran and produced an equal value.
    pub cutoff: HashSet<NodeId>,
    /// Nodes whose trace validated.
    pub validated: HashSet<NodeId>,
    /// Nodes whose attempt failed.
    pub failed: HashSet<NodeId>,
    /// Whether the cap dropped activity.
    pub capped: bool,
}

impl Activity {
    fn insert(set: &mut HashSet<NodeId>, node: NodeId, cap: usize, capped: &mut bool) {
        if set.len() >= cap {
            *capped = true;
            return;
        }
        set.insert(node);
    }
}

/// What one revision caused.
#[derive(Clone, Debug)]
pub struct RevisionRecord {
    /// Revision number.
    pub revision: Revision,
    /// Host transaction label.
    pub label: String,
    /// Staged writes.
    pub staged: u32,
    /// Units whose value changed.
    pub changed: u32,
    /// Whether a revision was published.
    pub published: bool,
    /// Nodes newly marked suspect.
    pub invalidated: u64,
    /// Commit duration.
    pub duration_ns: u64,
    /// Evaluating demands.
    pub demands: u32,
    /// Distinct causes.
    pub demand_causes: BTreeSet<&'static str>,
    /// Callbacks fired.
    pub notifications: u32,
    /// Node activity.
    pub activity: Activity,
    /// Views with a changed unit.
    pub changed_views: BTreeSet<ViewId>,
    /// Views redefined to an equal value.
    pub cutoff_views: BTreeSet<ViewId>,
    /// First nodes the invalidation walk touched.
    pub invalidated_seeds: Vec<NodeId>,
    /// Which View each observed node belongs to, as reported by the runtime.
    pub views: HashMap<NodeId, ViewId>,
    /// Whether the recorder dropped events for this revision.
    pub incomplete: bool,
}

impl RevisionRecord {
    /// The View a node belongs to, when the event stream named one.
    #[must_use]
    pub fn view_of(&self, node: NodeId) -> Option<ViewId> {
        self.views.get(&node).copied()
    }

    fn new(revision: Revision) -> Self {
        Self {
            revision,
            label: String::new(),
            staged: 0,
            changed: 0,
            published: true,
            invalidated: 0,
            duration_ns: 0,
            demands: 0,
            demand_causes: BTreeSet::new(),
            notifications: 0,
            activity: Activity::default(),
            changed_views: BTreeSet::new(),
            cutoff_views: BTreeSet::new(),
            invalidated_seeds: Vec::new(),
            views: HashMap::new(),
            incomplete: false,
        }
    }
}

/// A bounded history of published revisions.
#[derive(Debug)]
pub struct Journal {
    records: BTreeMap<Revision, RevisionRecord>,
    cap: usize,
    activity_cap: usize,
    level: ObserveLevel,
    /// A transaction that published nothing, kept so the page can say so.
    last_noop: Option<(String, Revision)>,
    /// Events dropped while collecting, by revision.
    drops: u64,
}

impl Journal {
    /// Builds a journal retaining `cap` revisions.
    #[must_use]
    pub fn new(cap: usize, level: ObserveLevel) -> Self {
        Self {
            records: BTreeMap::new(),
            cap: cap.max(1),
            activity_cap: 2_000,
            level,
            last_noop: None,
            drops: 0,
        }
    }

    /// Applies one event.
    ///
    /// Events below the journal's level are ignored; the runtime should not
    /// send them, and a mismatch is not the journal's problem.
    pub fn apply(&mut self, event: &RuntimeEvent) {
        match event {
            RuntimeEvent::TransactionCommitted {
                label,
                revision,
                staged,
                changed,
                published,
                invalidated,
                views,
                duration_ns,
            } => {
                if !published {
                    self.last_noop = Some((label.to_string(), *revision));
                    return;
                }
                let activity_cap = self.activity_cap;
                let record = self
                    .records
                    .entry(*revision)
                    .or_insert_with(|| RevisionRecord::new(*revision));
                record.label = label.to_string();
                record.staged = *staged;
                record.changed = *changed;
                record.published = true;
                record.invalidated = *invalidated;
                record.duration_ns = *duration_ns;
                record.changed_views.extend(views.iter().copied());
                record.incomplete = self.drops > 0;
                let _ = activity_cap;
                self.evict();
            }
            RuntimeEvent::Invalidated {
                revision,
                seeds,
                count,
            } => {
                if let Some(record) = self.records.get_mut(revision) {
                    record.invalidated = *count;
                    record
                        .invalidated_seeds
                        .extend(seeds.iter().copied().take(4));
                }
            }
            RuntimeEvent::DemandStarted { cause, .. } => {
                let Some(record) = self.open_mut() else {
                    return;
                };
                if cause.evaluates() {
                    record.demands = record.demands.saturating_add(1);
                    record.demand_causes.insert(cause.kind());
                }
            }
            RuntimeEvent::SubscriptionNotified { .. } => {
                if let Some(record) = self.open_mut() {
                    record.notifications = record.notifications.saturating_add(1);
                }
            }
            RuntimeEvent::NodeCommitted {
                node,
                changed,
                views,
                equal_views,
                ..
            } => {
                let cap = self.activity_cap;
                let Some(record) = self.open_mut() else {
                    return;
                };
                Activity::insert(
                    &mut record.activity.executed,
                    *node,
                    cap,
                    &mut record.activity.capped,
                );
                let target = if *changed {
                    &mut record.activity.changed
                } else {
                    &mut record.activity.cutoff
                };
                Activity::insert(target, *node, cap, &mut record.activity.capped);
                for view in views {
                    record.views.entry(*node).or_insert(*view);
                    if *changed {
                        record.changed_views.insert(*view);
                    }
                }
                for view in equal_views {
                    record.views.entry(*node).or_insert(*view);
                    record.cutoff_views.insert(*view);
                }
            }
            RuntimeEvent::NodeValidated { node, .. } => {
                let cap = self.activity_cap;
                if let Some(record) = self.open_mut() {
                    Activity::insert(
                        &mut record.activity.validated,
                        *node,
                        cap,
                        &mut record.activity.capped,
                    );
                }
            }
            RuntimeEvent::NodeFailed { node, .. } => {
                let cap = self.activity_cap;
                if let Some(record) = self.open_mut() {
                    Activity::insert(
                        &mut record.activity.failed,
                        *node,
                        cap,
                        &mut record.activity.capped,
                    );
                }
            }
            _ => {}
        }
    }

    /// Records a drop so the next revision is marked incomplete.
    pub fn note_drops(&mut self, dropped: u64) {
        self.drops = self.drops.saturating_add(dropped);
    }

    /// The newest published revision.
    #[must_use]
    pub fn current(&self) -> Option<Revision> {
        self.records.keys().next_back().copied()
    }

    /// The oldest retained revision.
    #[must_use]
    pub fn oldest(&self) -> Option<Revision> {
        self.records.keys().next().copied()
    }

    /// The journal's recording level.
    #[must_use]
    pub fn level(&self) -> ObserveLevel {
        self.level
    }

    /// One revision's record.
    #[must_use]
    pub fn record(&self, revision: Revision) -> Option<&RevisionRecord> {
        self.records.get(&revision)
    }

    /// Records, newest first.
    pub fn iter(&self) -> impl Iterator<Item = &RevisionRecord> {
        self.records.values().rev()
    }

    /// Number of retained revisions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether nothing has been retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The most recent transaction that published nothing.
    #[must_use]
    pub fn last_noop(&self) -> Option<&(String, Revision)> {
        self.last_noop.as_ref()
    }

    fn open_mut(&mut self) -> Option<&mut RevisionRecord> {
        let revision = self.records.keys().next_back().copied()?;
        self.records.get_mut(&revision)
    }

    fn evict(&mut self) {
        while self.records.len() > self.cap {
            let Some(oldest) = self.records.keys().next().copied() else {
                return;
            };
            self.records.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
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
}
