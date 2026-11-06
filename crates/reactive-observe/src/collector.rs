//! The collector: drains events, keeps the journal, broadcasts frames.
//!
//! One dedicated thread does this work. It is not a runtime worker and it never
//! calls a runtime method that could demand: it drains the ring, folds events
//! into the journal, and publishes immutable frames. HTTP handlers do their own
//! projections on their own threads.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reactive_core::ids::{NodeId, Revision, ViewId};
use reactive_runtime::observe::{GraphQuery, GraphScope, RuntimeEvent, ViewFacts};
use reactive_runtime::{Runtime, RuntimeObserver};

use crate::journal::Journal;
use crate::project::{self, Caps, Renderers};
use crate::recorder::Recorder;
use crate::wire::{
    self, EventWire, GraphWire, Hello, Limits, RevisionWire, StatsWire, ViewRenderWire, ViewWire,
    WorkspaceWire, node_key,
};

/// One broadcast batch.
#[derive(Clone, Debug)]
pub struct Frame {
    /// Global sequence of this frame.
    pub seq: u64,
    /// Events in order.
    pub events: Vec<EventWire>,
}

/// A host write: apply new text, or explain why it was rejected.
pub type TextWriter = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

/// One View's text channel: a host-owned textual form plus a setter.
///
/// The page edits whatever the host says the View's text is; the host decides
/// how that text becomes units. A list, a map, or a tree can all expose one.
#[derive(Clone)]
pub struct TextChannel {
    /// Returns the committed text.
    pub read: Arc<dyn Fn() -> String + Send + Sync>,
    /// Applies new text and returns a one-line account of what it did.
    pub write: TextWriter,
}

impl std::fmt::Debug for TextChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextChannel").finish_non_exhaustive()
    }
}

/// A session's feed.
#[derive(Debug)]
pub struct Subscription {
    /// Subscription identity, for diagnostics.
    pub id: u64,
    /// Frames, oldest first.
    pub frames: Receiver<Arc<Frame>>,
    /// Sequence at subscribe time; a snapshot taken after this is consistent.
    pub seq: u64,
}

struct SubscriberRecord {
    id: u64,
    sender: SyncSender<Arc<Frame>>,
}

/// Shared observation state.
pub struct Collector {
    runtime: Runtime,
    recorder: Arc<Recorder>,
    journal: Mutex<Journal>,
    renderers: Arc<Renderers>,
    caps: Caps,
    seq: AtomicU64,
    subscribers: Mutex<Vec<SubscriberRecord>>,
    next_subscriber: AtomicU64,
    texts: Mutex<HashMap<ViewId, TextChannel>>,
    stop: Arc<AtomicBool>,
    dropped_seen: AtomicU64,
    lagged: AtomicU64,
}

impl std::fmt::Debug for Collector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Collector")
            .field("seq", &self.seq.load(Ordering::Relaxed))
            .field("history", &self.journal().len())
            .finish_non_exhaustive()
    }
}

impl Collector {
    /// Builds a collector around a runtime and its recorder.
    #[must_use]
    pub fn new(
        runtime: Runtime,
        recorder: Arc<Recorder>,
        renderers: Renderers,
        history: usize,
        caps: Caps,
    ) -> Arc<Self> {
        let level = RuntimeObserver::level(&*recorder);
        let journal = Journal::new(history, level);
        Arc::new(Self {
            runtime,
            recorder,
            journal: Mutex::new(journal),
            renderers: Arc::new(renderers),
            caps,
            seq: AtomicU64::new(0),
            subscribers: Mutex::new(Vec::new()),
            next_subscriber: AtomicU64::new(1),
            texts: Mutex::new(HashMap::new()),
            stop: Arc::new(AtomicBool::new(false)),
            dropped_seen: AtomicU64::new(0),
            lagged: AtomicU64::new(0),
        })
    }

    /// Registers one View's text channel.
    pub fn set_text_view(&self, view: ViewId, channel: TextChannel) {
        self.texts
            .lock()
            .expect("text registry poisoned")
            .insert(view, channel);
    }

    /// Whether this View can be edited from the page.
    #[must_use]
    pub fn is_editable(&self, view: ViewId) -> bool {
        self.texts
            .lock()
            .expect("text registry poisoned")
            .contains_key(&view)
    }

    /// Reads a View's committed text.
    #[must_use]
    pub fn text_view(&self, view: ViewId) -> Option<String> {
        let texts = self.texts.lock().expect("text registry poisoned");
        texts.get(&view).map(|channel| (channel.read)())
    }

    /// Applies a View's new text.
    pub fn apply_text_view(&self, view: ViewId, text: &str) -> Option<Result<String, String>> {
        let texts = self.texts.lock().expect("text registry poisoned");
        texts.get(&view).map(|channel| (channel.write)(text))
    }

    /// The runtime being observed.
    #[must_use]
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// The recorder feeding this collector.
    #[must_use]
    pub fn recorder(&self) -> &Arc<Recorder> {
        &self.recorder
    }

    /// Server caps.
    #[must_use]
    pub fn caps(&self) -> Caps {
        self.caps
    }

    /// Current sequence.
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    /// Registers a session feed.
    pub fn subscribe(&self) -> Subscription {
        let (sender, frames) = sync_channel(256);
        let id = self.next_subscriber.fetch_add(1, Ordering::Relaxed);
        // Read the sequence before the snapshot so no frame can slip between
        // the snapshot and the first event the client applies.
        let seq = self.seq();
        self.subscribers
            .lock()
            .expect("subscriber list poisoned")
            .push(SubscriberRecord { id, sender });
        Subscription { id, frames, seq }
    }

    /// Unregisters a session feed.
    pub fn unsubscribe(&self, id: u64) {
        self.subscribers
            .lock()
            .expect("subscriber list poisoned")
            .retain(|record| record.id != id);
    }

    /// Signals the drain thread to stop.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Borrows the journal, recovering from a poisoned lock.
    ///
    /// A panic in an unrelated handler must not take the page down; the journal
    /// is append-only bookkeeping, not a shared invariant.
    fn journal(&self) -> std::sync::MutexGuard<'_, Journal> {
        self.journal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The first frame of a session.
    #[must_use]
    pub fn hello(&self) -> Hello {
        let (oldest, retained) = {
            let journal = self.journal();
            (journal.oldest(), journal.len())
        };
        Hello {
            protocol: wire::PROTOCOL,
            level: self.recorder.level().as_str(),
            runtime: root_name(&self.runtime),
            limits: self.limits_with(retained),
            history_from: oldest,
        }
    }

    /// Server caps in wire form.
    #[must_use]
    pub fn limits(&self) -> Limits {
        self.limits_with(self.journal().len())
    }

    fn limits_with(&self, retained: usize) -> Limits {
        Limits {
            node_limit: self.caps.node_limit,
            edge_limit: self.caps.edge_limit,
            rows: self.caps.rows,
            tree_rows: self.caps.tree_rows,
            set_members: self.caps.set_members,
            graph_nodes: self.caps.graph_nodes,
            history: retained,
        }
    }

    /// View metadata and counters.
    #[must_use]
    pub fn workspace(&self) -> WorkspaceWire {
        let revision = self.runtime.revision();
        let seq = self.seq();
        let facts = self.runtime.view_facts();
        let journal = self.journal();
        let current = journal.record(revision).or_else(|| journal.iter().next());
        let views = project::workspace_views(&facts, revision, current, &self.renderers, &|view| {
            self.is_editable(view)
        });
        let dropped = self.recorder.dropped();
        drop(journal);
        let components = self
            .runtime
            .component_facts()
            .iter()
            .map(wire::component_wire)
            .collect();
        WorkspaceWire {
            seq,
            revision,
            runtime: root_name(&self.runtime),
            level: self.recorder.level().as_str(),
            views,
            components,
            stats: StatsWire::from_stats(&self.runtime.stats()),
            dropped,
            last_noop: None,
            last_noop_revision: None,
        }
    }

    /// View metadata only.
    #[must_use]
    pub fn views(&self) -> Vec<ViewWire> {
        let revision = self.runtime.revision();
        let facts = self.runtime.view_facts();
        let journal = self.journal();
        let current = journal.record(revision).or_else(|| journal.iter().next());
        project::workspace_views(&facts, revision, current, &self.renderers, &|view| {
            self.is_editable(view)
        })
    }

    /// One View's facts, by wire key.
    #[must_use]
    pub fn view_facts(&self, view: ViewId) -> Option<ViewFacts> {
        self.runtime
            .view_facts()
            .into_iter()
            .find(|facts| facts.view == view)
    }

    /// A View's rendered body.
    #[must_use]
    pub fn render(&self, view: ViewId) -> Option<ViewRenderWire> {
        let facts = self.view_facts(view)?;
        // Every registered View has a drawing: the host's, or the generic one
        // built from the runtime's facts.
        project::view_render_wire(&self.runtime, &facts, &self.renderers, self.caps)
    }

    /// Retained revisions, newest first.
    #[must_use]
    pub fn revisions(&self) -> Vec<RevisionWire> {
        self.journal().iter().map(wire::revision_wire).collect()
    }

    /// One revision's record.
    #[must_use]
    pub fn revision(&self, revision: Revision) -> Option<RevisionWire> {
        self.journal().record(revision).map(wire::revision_wire)
    }

    /// One node's detail.
    #[must_use]
    pub fn node(&self, node: NodeId) -> Option<wire::NodeDetailWire> {
        project::node_detail(&self.runtime, node, &self.renderers)
    }

    /// A bounded node-link projection.
    #[must_use]
    pub fn graph(&self, scope: GraphScope, include_sites: bool) -> GraphWire {
        let facts = self.runtime.probe(&GraphQuery {
            scope,
            include_sites,
            node_limit: self.caps.node_limit,
            edge_limit: self.caps.edge_limit,
        });
        wire::graph_wire(&facts)
    }

    /// The most recent no-op transaction, when one is pending.
    #[must_use]
    pub fn last_noop(&self) -> Option<(String, Revision)> {
        self.journal().last_noop().cloned()
    }

    fn broadcast(&self, frame: &Arc<Frame>) {
        let mut subscribers = self.subscribers.lock().expect("subscriber list poisoned");
        let mut lagged = false;
        subscribers.retain(|record| match record.sender.try_send(Arc::clone(frame)) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                // A slow client misses this frame; its next frame has a gap in
                // `seq`, which the page treats as a resync request. The runtime
                // and every other client are unaffected.
                lagged = true;
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        });
        if lagged {
            self.lagged.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Runs the drain loop until [`Collector::stop`] is called.
    pub fn run(self: Arc<Self>) {
        let mut events = Vec::with_capacity(4_096);
        loop {
            if self.stop.load(Ordering::Acquire) {
                break;
            }
            self.recorder.drain(4_096, &mut events);
            let dropped = self.recorder.dropped();
            let seen = self.dropped_seen.swap(dropped, Ordering::Relaxed);
            if dropped > seen {
                self.journal().note_drops(dropped - seen);
            }
            if events.is_empty() {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            {
                let mut journal = self.journal();
                for event in &events {
                    journal.apply(event);
                }
            }
            let seq = self.seq.fetch_add(1, Ordering::AcqRel) + 1;
            let frame = Arc::new(Frame {
                seq,
                events: events.iter().map(map_event).collect(),
            });
            self.broadcast(&frame);
        }
    }

    /// Spawns the drain loop on its own thread.
    #[must_use]
    pub fn spawn(self: &Arc<Self>) -> std::thread::JoinHandle<()> {
        let collector = Arc::clone(self);
        std::thread::Builder::new()
            .name(String::from("observatory-collector"))
            .spawn(move || collector.run())
            .expect("spawn collector thread")
    }
}

/// The installed root's name, when there is one.
fn root_name(runtime: &Runtime) -> Option<String> {
    runtime.explain().root.map(str::to_owned)
}

/// Maps a runtime event to its wire form.
fn map_event(event: &RuntimeEvent) -> EventWire {
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
        } => EventWire::Transaction {
            revision: *revision,
            label: label.to_string(),
            staged: *staged,
            changed: *changed,
            published: *published,
            invalidated: *invalidated,
            views: views
                .iter()
                .map(|view| format!("v:{:016x}", view.0))
                .collect(),
            duration_ns: *duration_ns,
        },
        RuntimeEvent::Invalidated {
            revision,
            seeds,
            count,
        } => EventWire::Invalidated {
            revision: *revision,
            count: *count,
            seeds: seeds.iter().map(|node| node_key(*node)).collect(),
        },
        RuntimeEvent::DemandStarted { task, cause } => EventWire::Demand {
            task: *task,
            cause: cause.kind(),
            subscription: None,
        },
        RuntimeEvent::DemandFinished {
            task,
            ok,
            duration_ns,
        } => EventWire::DemandDone {
            task: *task,
            ok: *ok,
            duration_ns: *duration_ns,
        },
        RuntimeEvent::NodeClaimed { node, task } => EventWire::Claim {
            node: node_key(*node),
            task: *task,
        },
        RuntimeEvent::NodeValidated { node, deps } => EventWire::Validated {
            node: node_key(*node),
            deps: *deps,
        },
        RuntimeEvent::NodeCommitted {
            node,
            views,
            changed,
            duration_ns,
            ..
        } => EventWire::Committed {
            node: node_key(*node),
            view: views.first().map(|view| format!("v:{:016x}", view.0)),
            changed: *changed,
            duration_ns: *duration_ns,
        },
        RuntimeEvent::NodeFailed { node, kind } => EventWire::Failed {
            node: node_key(*node),
            error: kind,
        },
        RuntimeEvent::NodeReused { node } => EventWire::Reused {
            node: node_key(*node),
        },
        RuntimeEvent::SubscriptionNotified {
            view,
            previous,
            current,
        } => EventWire::Notified {
            view: format!("v:{:016x}", view.0),
            previous: *previous,
            current: *current,
        },
        RuntimeEvent::TaskRunning { task } => EventWire::Running { task: *task },
        RuntimeEvent::TaskWaiting { task, node } => EventWire::Waiting {
            task: *task,
            node: node_key(*node),
        },
        RuntimeEvent::TaskWoken { task, node } => EventWire::Woken {
            task: *task,
            node: node_key(*node),
        },
        RuntimeEvent::Collected { nodes, scopes } => EventWire::Collected {
            nodes: *nodes,
            scopes: *scopes,
        },
        RuntimeEvent::CatalogChanged => EventWire::Catalog,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn slow_subscribers_do_not_block_broadcasts() {
        let collector = test_collector();
        // A subscriber that never drains only fills its own channel.
        let _idle = collector.subscribe();
        let frame = Arc::new(Frame {
            seq: 1,
            events: Vec::new(),
        });
        for seq in 1..=1_000 {
            collector.broadcast(&Arc::new(Frame {
                seq,
                events: frame.events.clone(),
            }));
        }
        assert_eq!(collector.seq(), 0);
        assert!(collector.lagged.load(Ordering::Relaxed) > 0);
    }

    fn test_collector() -> Arc<Collector> {
        let recorder = crate::recorder::recorder(reactive_runtime::observe::ObserveLevel::Nodes);
        let runtime = reactive_runtime::Runtime::with_observer(
            reactive_runtime::RuntimeConfig::default(),
            recorder.clone(),
        );
        Collector::new(runtime, recorder, Renderers::default(), 8, Caps::default())
    }
}
