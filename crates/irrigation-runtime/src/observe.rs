//! Optional runtime observation.
//!
//! An observer receives compact facts about work the runtime already did: a
//! transaction that published, a node that validated or executed, a task that
//! waited. It is fixed when the runtime is constructed, so there is no
//! attach/detach race and no early-event gap.
//!
//! Observers are passive. They must not call back into the runtime: the trait
//! contract and the development server both hold that line. Events carry IDs,
//! versions, and counts; resolving them into names happens outside the runtime
//! lock in `irrigation-observe`.
//!
//! Compiled only under the `observe` feature.

use std::sync::Arc;

use smallvec::SmallVec;

use irrigation_core::ids::{NodeId, Revision, Version, ViewId};

/// How much detail an observer wants.
///
/// Ordering matters: `Full` enables everything cheaper levels see.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObserveLevel {
    /// Observation disabled; no events are produced.
    #[default]
    Off,
    /// Transactions, revisions, invalidation counts, and demand boundaries.
    Summary,
    /// Adds per-node claims, validations, commits, cutoffs, and failures.
    Nodes,
    /// Adds task transitions, clean reuses, and collection.
    Full,
}

impl ObserveLevel {
    /// The name used on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Summary => "summary",
            Self::Nodes => "nodes",
            Self::Full => "full",
        }
    }
}

pub use crate::task::DemandCause;

/// A compact fact emitted by the runtime.
///
/// Every variant names a minimum [`ObserveLevel`] in [`RuntimeEvent::level`].
#[derive(Clone, Debug)]
pub enum RuntimeEvent {
    /// A View or component was registered.
    CatalogChanged,
    /// A write-arbitration fact: claim, conflict, deferral, wait, or retry.
    Arbitration {
        /// Stable event kind (`write_claim`, `write_conflict`, ...).
        kind: &'static str,
        /// Node that owns the action.
        node: NodeId,
        /// Port or unit description.
        port: String,
    },
    /// A transaction reached its commit decision.
    TransactionCommitted {
        /// Host label for the transaction, empty when unset.
        label: Arc<str>,
        /// The published revision, or the current one when nothing published.
        revision: Revision,
        /// Staged unit writes.
        staged: u32,
        /// Units whose final value differed.
        changed: u32,
        /// Whether a revision was published.
        published: bool,
        /// Nodes newly marked suspect.
        invalidated: u64,
        /// Views with a changed unit, deduplicated and capped.
        views: SmallVec<[ViewId; 8]>,
        /// Time from commit entry to publication.
        duration_ns: u64,
    },
    /// A reverse-dependency walk marked nodes suspect.
    Invalidated {
        /// Revision of the invalidation.
        revision: Revision,
        /// Nodes that seeded the walk, capped.
        seeds: SmallVec<[NodeId; 4]>,
        /// Exact number of nodes marked, even when `seeds` is truncated.
        count: u64,
    },
    /// A demand task was queued.
    DemandStarted {
        /// Task identity.
        task: u64,
        /// What asked for it.
        cause: DemandCause,
    },
    /// A demand task finished.
    DemandFinished {
        /// Task identity.
        task: u64,
        /// Whether it produced a value.
        ok: bool,
        /// Total time the task existed.
        duration_ns: u64,
    },
    /// A task began an attempt on a worker.
    TaskRunning {
        /// Task identity.
        task: u64,
    },
    /// A task suspended on another task's node.
    TaskWaiting {
        /// Task identity.
        task: u64,
        /// The awaited node.
        node: NodeId,
    },
    /// A task suspended on `node` was queued again.
    TaskWoken {
        /// Task identity.
        task: u64,
        /// The completed node.
        node: NodeId,
    },
    /// A task claimed a node for execution.
    NodeClaimed {
        /// The claimed node.
        node: NodeId,
        /// The claiming task.
        task: u64,
    },
    /// A node's trace was validated and certified without executing.
    NodeValidated {
        /// The validated node.
        node: NodeId,
        /// Number of observations checked.
        deps: u32,
    },
    /// A node's closure committed.
    NodeCommitted {
        /// The committed node.
        node: NodeId,
        /// Views whose units this commit actually changed.
        views: SmallVec<[ViewId; 4]>,
        /// Views this commit redefined to an equal value (a cutoff).
        equal_views: SmallVec<[ViewId; 4]>,
        /// Whether the value changed; `false` is a cutoff.
        changed: bool,
        /// Nodes newly marked suspect by this commit.
        invalidated: u64,
        /// Definitions installed with the commit.
        definitions: u32,
        /// Time spent inside the closure.
        duration_ns: u64,
    },
    /// A node's closure failed; nothing was published.
    NodeFailed {
        /// The failed node.
        node: NodeId,
        /// The error variant name.
        kind: &'static str,
    },
    /// A node was served without validation or execution.
    NodeReused {
        /// The reused node.
        node: NodeId,
    },
    /// An observation callback fired.
    SubscriptionNotified {
        /// The observed View.
        view: ViewId,
        /// Content version before the refresh.
        ///
        /// Equal to `current` for a unit-granularity notification: those are woken
        /// by a per-unit move that does not advance the View's content.
        previous: Version,
        /// Content version now.
        current: Version,
        /// How many watched units moved for this notification.
        ///
        /// Zero for a whole-View subscription, which watches the content instead.
        /// For a unit subscription this is the number that explains a
        /// notification whose content versions above are equal.
        moved: u32,
    },
    /// Cache reclamation ran.
    Collected {
        /// Nodes reclaimed.
        nodes: u32,
        /// Scopes reclaimed.
        scopes: u32,
    },
}

impl RuntimeEvent {
    /// The cheapest observation level that produces this event.
    #[must_use]
    pub fn level(&self) -> ObserveLevel {
        match self {
            Self::CatalogChanged
            | Self::TransactionCommitted { .. }
            | Self::Invalidated { .. }
            | Self::DemandStarted { .. }
            | Self::DemandFinished { .. }
            | Self::SubscriptionNotified { .. } => ObserveLevel::Summary,
            Self::NodeClaimed { .. }
            | Self::NodeValidated { .. }
            | Self::NodeCommitted { .. }
            | Self::NodeFailed { .. } => ObserveLevel::Nodes,
            Self::TaskRunning { .. }
            | Self::TaskWaiting { .. }
            | Self::TaskWoken { .. }
            | Self::NodeReused { .. }
            | Self::Collected { .. }
            | Self::Arbitration { .. } => ObserveLevel::Full,
        }
    }
}

/// Receives runtime events.
///
/// `record` runs on the thread that produced the event, sometimes inside a
/// demand task. It must be cheap, must not block, and must not call back into
/// the runtime; the runtime guards against panics but not against re-entry.
pub trait RuntimeObserver: Send + Sync + 'static {
    /// The detail this observer wants. Events below it are never built.
    fn level(&self) -> ObserveLevel;

    /// Records one event.
    fn record(&self, event: RuntimeEvent);
}

/// Enforces the level an observer asked for.
///
/// The runtime installs this in front of the observer it was given, so the
/// level is consulted on the one path every event leaves the kernel through
/// instead of being a promise each consumer keeps for itself.
pub(crate) struct LevelFilter {
    observer: Arc<dyn RuntimeObserver>,
}

impl LevelFilter {
    pub(crate) fn new(observer: Arc<dyn RuntimeObserver>) -> Self {
        Self { observer }
    }
}

impl RuntimeObserver for LevelFilter {
    fn level(&self) -> ObserveLevel {
        self.observer.level()
    }

    fn record(&self, event: RuntimeEvent) {
        if event.level() <= self.observer.level() {
            self.observer.record(event);
        }
    }
}

/// A static error-kind name for observation events.
#[must_use]
pub fn error_kind(error: &irrigation_core::error::ComputeError) -> &'static str {
    use irrigation_core::error::ComputeError;
    match error {
        ComputeError::Cycle { .. } => "cycle",
        ComputeError::DuplicateDefinition { .. } => "duplicate_definition",
        ComputeError::Cancelled => "cancelled",
        ComputeError::Suspended => "suspended",
        ComputeError::Panicked { .. } => "panicked",
        ComputeError::DepthExceeded { .. } => "depth_exceeded",
        ComputeError::OutsideComputation { .. } => "outside_computation",
        ComputeError::Unsupported { .. } => "unsupported",
        ComputeError::DefinitionDenied { .. } => "definition_denied",
        ComputeError::MissingNode { .. } => "missing_node",
        ComputeError::Absent { .. } => "absent",
        ComputeError::TypeMismatch { .. } => "type_mismatch",
        ComputeError::Internal { .. } => "internal",
        // `ComputeError` is non-exhaustive; a new variant is still an error.
        _ => "other",
    }
}

/// How much identity the runtime gives a field's elements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Granularity {
    /// Every element has its own unit (tree node, list item, map key).
    PerEntity,
    /// One definition for the whole field; rows are display rows only.
    WholeValue,
    /// The projection cannot tell; claim nothing.
    Unknown,
}

impl Granularity {
    /// The wire name for this granularity.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PerEntity => "per_entity",
            Self::WholeValue => "whole_value",
            Self::Unknown => "unknown",
        }
    }
}

/// One field of a View, as the board needs it.
#[derive(Clone, Debug)]
pub struct FieldFacts {
    /// Field identity.
    pub field: irrigation_core::ids::FieldId,
    /// Field name as declared.
    pub name: &'static str,
    /// Declared shape.
    pub kind: irrigation_core::descriptor::FieldKind,
    /// Element identity, derived from the region.
    pub granularity: Granularity,
    /// Installed logical units for this field.
    pub units: usize,
    /// Display elements, when cheap to count.
    pub elements: Option<usize>,
    /// Content version of the owning View.
    pub content_version: Version,
    /// Units whose value changed at the current revision.
    pub changed_units: u32,
}

/// One entity of a field, as a generic drawing needs it.
///
/// The runtime is the only place that knows a field's entities in its own order:
/// a list keeps a sequence, a map and a set keep a keyed table. A projection gets
/// them already ordered, with the unit that carries the identity and the version
/// that answers "did this row move".
#[derive(Clone, Debug)]
pub struct EntityFacts {
    /// The identity-bearing unit (`Membership` for a collection, `Value` for a
    /// scalar field).
    pub unit: irrigation_core::descriptor::UnitKey,
    /// Revision this entity last changed at.
    pub version: Version,
    /// Whether it changed at the current revision.
    pub changed: bool,
}

/// One registered View.
#[derive(Clone, Debug)]
pub struct ViewFacts {
    /// View identity.
    pub view: ViewId,
    /// Fully qualified name.
    pub name: &'static str,
    /// Source or derived.
    pub kind: irrigation_core::descriptor::ViewKind,
    /// Declared fields.
    pub fields: Vec<FieldFacts>,
    /// The primary component that supplies this View, if the projection can
    /// name one.
    ///
    /// A shared View may have several contributors; this is the first owner,
    /// kept for the existing plate/ink and for compatibility. Use
    /// [`Self::contributors`] when every writer matters.
    pub producer: Option<irrigation_core::ids::ComponentId>,
    /// Every distinct component that owns at least one unit of this View, in
    /// identity order.
    pub contributors: Vec<irrigation_core::ids::ComponentId>,
    /// Region routing generation.
    pub routing: u64,
    /// Content version at the current revision.
    pub content_version: Version,
    /// Whether committed content exists for this View.
    pub demanded: bool,
    /// The node holding the View's committed content.
    pub content_node: NodeId,
    /// Whether the View's content is awaiting recomputation.
    pub suspect: bool,
    /// The node that defines this View's content, when it has one.
    pub provider_node: Option<NodeId>,
    /// Units changed at the current revision, across fields.
    pub changed_units: u32,
}

/// What kind of component a node belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentKind {
    /// The installed root.
    Root,
    /// A custom algorithm installed as a general component.
    General,
    /// A component invoked by another.
    Invoked,
}

impl ComponentKind {
    /// Stable wire name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::General => "general",
            Self::Invoked => "invoked",
        }
    }
}

/// One component and the Views it reads and defines.
///
/// This is the runtime's own account of its structure: a component appears
/// because it is registered, not because a projection noticed it, and its edges
/// are the units its traces and definitions actually name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentFacts {
    /// Component identity.
    pub id: irrigation_core::ids::ComponentId,
    /// Fully qualified name.
    pub name: &'static str,
    /// Root, general, or invoked.
    pub kind: ComponentKind,
    /// Views this component reads.
    pub reads: Vec<ViewId>,
    /// Views this component defines.
    pub writes: Vec<ViewId>,
    /// Revision the component last executed at.
    pub executed_at: Revision,
    /// Revision the component was last invalidated at.
    pub suspect_at: Revision,
}

/// What part of the graph to project./// What part of the graph to project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphScope {
    /// Every node.
    All,
    /// Units and content of one View.
    View(ViewId),
    /// Nodes whose scope or identity belongs (transitively) to one component.
    Component(irrigation_core::ids::ComponentId),
    /// One node.
    Node(NodeId),
}

/// A bounded, read-only graph query.
#[derive(Clone, Copy, Debug)]
pub struct GraphQuery {
    /// Subgraph to include.
    pub scope: GraphScope,
    /// Whether expression-site nodes are included.
    pub include_sites: bool,
    /// Maximum nodes returned.
    pub node_limit: usize,
    /// Maximum edges returned.
    pub edge_limit: usize,
}

impl Default for GraphQuery {
    fn default() -> Self {
        Self {
            scope: GraphScope::All,
            include_sites: false,
            node_limit: 800,
            edge_limit: 2_500,
        }
    }
}

/// The logical unit a node carries, if any.
#[derive(Clone, Debug)]
pub struct UnitFacts {
    /// Owning View.
    pub view: ViewId,
    /// Field within the View.
    pub field: irrigation_core::ids::FieldId,
    /// What is observed.
    pub kind: irrigation_core::descriptor::UnitKind,
    /// Entity display, when the unit is entity-scoped.
    pub entity: Option<String>,
}

/// One node's metadata. Values are never included.
#[derive(Clone, Debug)]
pub struct NodeFacts {
    /// Node identity.
    pub id: NodeId,
    /// Human-readable kind.
    pub kind: &'static str,
    /// Debug label recorded at allocation.
    pub label: &'static str,
    /// Identity scope.
    pub scope: irrigation_core::ids::ScopeId,
    /// Owning component, for bodies and outputs.
    pub component: Option<irrigation_core::ids::ComponentId>,
    /// Logical unit, for unit nodes.
    pub unit: Option<UnitFacts>,
    /// Revision the value last changed at.
    pub changed_at: Version,
    /// Revision the value is certified for.
    pub verified_at: Revision,
    /// Revision of the last invalidation.
    pub suspect_at: Revision,
    /// Revision the closure last ran.
    pub executed_at: Revision,
    /// Revision the trace was last validated.
    pub validated_at: Revision,
    /// Routing generation of the carried unit.
    pub routing_at: Revision,
    /// Tasks currently suspended on this node.
    pub waiters: u32,
    /// Payload size in bytes.
    pub value_size: Option<usize>,
}

/// One recorded dependency edge.
#[derive(Clone, Copy, Debug)]
pub struct EdgeFacts {
    /// Observing node.
    pub from: NodeId,
    /// Observed node.
    pub to: NodeId,
    /// Version recorded when the observation was made.
    pub recorded: Version,
    /// The observed node's current version.
    pub current: Version,
}

/// A consistent, bounded projection of the node arena.
#[derive(Clone, Debug)]
pub struct GraphFacts {
    /// Revision the projection was taken at.
    pub revision: Revision,
    /// Included nodes.
    pub nodes: Vec<NodeFacts>,
    /// Included edges.
    pub edges: Vec<EdgeFacts>,
    /// Nodes that matched before the cap.
    pub total_nodes: usize,
    /// Whether the cap excluded matching nodes.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use irrigation_core::descriptor::ViewDescriptor;
    use irrigation_core::ids::ComponentId;
    use irrigation_core::value::{Value, value_of};

    use super::{ObserveLevel, RuntimeEvent, RuntimeObserver};

    #[derive(Default)]
    struct Recorded(Mutex<Vec<RuntimeEvent>>);

    struct TestObserver {
        level: ObserveLevel,
        events: Arc<Recorded>,
    }

    impl RuntimeObserver for TestObserver {
        fn level(&self) -> ObserveLevel {
            self.level
        }

        fn record(&self, event: RuntimeEvent) {
            self.events.0.lock().expect("lock").push(event);
        }
    }

    struct ConstantRoot;

    impl crate::component::RootComponent for ConstantRoot {
        type Output = u32;

        const ID: ComponentId = ComponentId(0x00C2_0001);
        const NAME: &'static str = "tests::ConstantRoot";
        const ARITY: usize = 1;
        const VIEWS: &'static [&'static ViewDescriptor] = &[];

        fn run() -> u32 {
            7
        }

        fn project(output: &u32, _index: usize) -> Value {
            value_of(*output)
        }

        fn reconstruct(parts: &[Value]) -> u32 {
            *parts[0].downcast_ref::<u32>().expect("output")
        }

        fn projection_eq(_index: usize, left: &Value, right: &Value) -> bool {
            left.downcast_ref::<u32>() == right.downcast_ref::<u32>()
        }
    }

    /// Demands a root so the observer has a transaction and a demand to report.
    fn demand(runtime: &crate::Runtime) {
        runtime.install(ConstantRoot).expect("install");
        assert_eq!(runtime.read_outputs::<ConstantRoot>().expect("read"), 7);
    }

    fn observed(level: ObserveLevel) -> (crate::Runtime, Arc<Recorded>) {
        let events = Arc::new(Recorded::default());
        let observer: Arc<dyn RuntimeObserver> = Arc::new(TestObserver {
            level,
            events: Arc::clone(&events),
        });
        (
            crate::Runtime::with_observer(crate::inner::RuntimeConfig::default(), observer),
            events,
        )
    }

    #[test]
    fn observation_disabled_delivers_nothing() {
        let (runtime, events) = observed(ObserveLevel::Off);
        demand(&runtime);
        assert!(
            events.0.lock().expect("lock").is_empty(),
            "an off observer must receive no events"
        );
    }

    #[test]
    fn a_summary_observer_does_not_see_node_events() {
        let (runtime, events) = observed(ObserveLevel::Summary);
        demand(&runtime);
        let recorded = events.0.lock().expect("lock");
        assert!(
            !recorded.is_empty(),
            "a summary observer still sees the demand it caused"
        );
        let levels: Vec<ObserveLevel> = recorded.iter().map(RuntimeEvent::level).collect();
        assert!(
            levels.iter().all(|level| *level <= ObserveLevel::Summary),
            "a summary observer must not see node or task events: {levels:?}"
        );
    }

    #[test]
    fn a_full_observer_sees_the_whole_stream() {
        let (runtime, events) = observed(ObserveLevel::Full);
        demand(&runtime);
        let recorded = events.0.lock().expect("lock");
        assert!(
            recorded
                .iter()
                .any(|event| event.level() == ObserveLevel::Full),
            "a full observer still sees task events"
        );
    }
}
