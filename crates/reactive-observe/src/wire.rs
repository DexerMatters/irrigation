//! Wire types: the JSON a page consumes.
//!
//! These types are presentation, not runtime state. Every field is either an
//! exact runtime fact, a host-rendered display string, or a count the server
//! keeps itself. Nothing here is inferred: an unknown value is `null` or an
//! explicit state, never a guess.

use serde::Serialize;

use reactive_core::ids::{NodeId, Revision, Version, ViewId};
use reactive_runtime::observe::{FieldFacts, GraphFacts, NodeFacts, ViewFacts};
use reactive_runtime::runtime::{Explanation, RuntimeStats};

use crate::journal::RevisionRecord;

/// Wire protocol version. Bumped when a field changes meaning.
pub const PROTOCOL: u32 = 1;

/// Server-side caps, echoed so the page can explain a truncated view.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Limits {
    /// Maximum nodes in a graph projection.
    pub node_limit: usize,
    /// Maximum edges in a graph projection.
    pub edge_limit: usize,
    /// Maximum rows per list/map/scalar render.
    pub rows: usize,
    /// Maximum tree rows.
    pub tree_rows: usize,
    /// Maximum set members.
    pub set_members: usize,
    /// Maximum graph nodes inside a module.
    pub graph_nodes: usize,
    /// Retained revisions.
    pub history: usize,
}

/// The first frame of a session.
#[derive(Clone, Debug, Serialize)]
pub struct Hello {
    /// Wire protocol version.
    pub protocol: u32,
    /// Recording level.
    pub level: &'static str,
    /// Runtime name, when a root is installed.
    pub runtime: Option<String>,
    /// Server caps.
    pub limits: Limits,
    /// Oldest retained revision, when history has been trimmed.
    pub history_from: Option<Revision>,
}

/// A node identity as the page uses it.
#[must_use]
pub fn node_key(node: NodeId) -> String {
    format!("n:{}@{}", node.index(), node.generation())
}

/// A View identity as the page uses it.
#[must_use]
pub fn view_key(view: ViewId) -> String {
    format!("v:{:016x}", view.0)
}

/// One logical unit.
#[derive(Clone, Debug, Serialize)]
pub struct UnitWire {
    /// Owning View key.
    pub view: String,
    /// Field index.
    pub field: u16,
    /// Unit kind.
    pub kind: &'static str,
    /// Entity display, for entity-scoped units.
    pub entity: Option<String>,
    /// Change version, when the unit is installed.
    pub changed_at: Option<Version>,
}

/// Row state, derived by the server from the unit's facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RowState {
    /// The row's unit changed at the selected revision.
    Changed,
    /// The row's unit is certified but older than the selected revision.
    Clean,
    /// The row's unit is suspect at the selected revision.
    Suspect,
    /// The row has no unit; it is part of a whole-value render.
    Display,
    /// The row's unit was reclaimed.
    Removed,
}

/// One tree row.
#[derive(Clone, Debug, Serialize)]
pub struct TreeRowWire {
    /// Stable row key.
    pub key: String,
    /// Nesting depth.
    pub depth: u16,
    /// Kind label.
    pub kind: String,
    /// Display label.
    pub label: String,
    /// Optional value.
    pub value: Option<String>,
    /// Unit, when the row is entity-backed.
    pub unit: Option<UnitWire>,
    /// Server-derived state.
    pub state: RowState,
}

/// One list row.
#[derive(Clone, Debug, Serialize)]
pub struct ListRowWire {
    /// Stable row key.
    pub key: String,
    /// Display index.
    pub index: usize,
    /// Display value.
    pub value: String,
    /// Unit, when the row is entity-backed.
    pub unit: Option<UnitWire>,
    /// Server-derived state.
    pub state: RowState,
}

/// One map entry.
#[derive(Clone, Debug, Serialize)]
pub struct MapRowWire {
    /// Stable row key.
    pub key: String,
    /// Display key.
    pub label: String,
    /// Display value.
    pub value: Option<String>,
    /// Unit, when the entry is entity-backed.
    pub unit: Option<UnitWire>,
    /// Server-derived state.
    pub state: RowState,
}

/// One set member.
#[derive(Clone, Debug, Serialize)]
pub struct SetMemberWire {
    /// Stable row key.
    pub key: String,
    /// Display member.
    pub member: String,
    /// Whether the member is present.
    pub present: bool,
    /// Unit, when the member is entity-backed.
    pub unit: Option<UnitWire>,
    /// Server-derived state.
    pub state: RowState,
}

/// One graph node.
#[derive(Clone, Debug, Serialize)]
pub struct GraphNodeWire {
    /// Stable node identity.
    pub id: String,
    /// Display label.
    pub label: String,
    /// Unit, when the node is entity-backed.
    pub unit: Option<UnitWire>,
    /// Server-derived state.
    pub state: RowState,
}

/// One graph edge.
#[derive(Clone, Debug, Serialize)]
pub struct GraphEdgeWire {
    /// Source identity.
    pub from: String,
    /// Destination identity.
    pub to: String,
    /// Optional label.
    pub label: Option<String>,
}

/// One scalar row.
#[derive(Clone, Debug, Serialize)]
pub struct ScalarRowWire {
    /// Stable row key.
    pub key: String,
    /// Field name.
    pub label: String,
    /// Display value.
    pub value: String,
    /// Unit, when the field is entity-backed.
    pub unit: Option<UnitWire>,
    /// Server-derived state.
    pub state: RowState,
}

/// A display model for one field.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelWire {
    /// An indented outline.
    Tree {
        /// Rows.
        rows: Vec<TreeRowWire>,
    },
    /// Ordered rows.
    List {
        /// Rows.
        rows: Vec<ListRowWire>,
    },
    /// Key/value entries.
    Map {
        /// Rows.
        rows: Vec<MapRowWire>,
    },
    /// Membership.
    Set {
        /// Members.
        members: Vec<SetMemberWire>,
    },
    /// A small network.
    Graph {
        /// Nodes.
        nodes: Vec<GraphNodeWire>,
        /// Edges.
        edges: Vec<GraphEdgeWire>,
    },
    /// Key/value fields.
    Scalar {
        /// Rows.
        rows: Vec<ScalarRowWire>,
    },
    /// A declared aggregate plus a sample.
    Reduced {
        /// Formatted aggregate.
        aggregate: String,
        /// Reduction name.
        reduction: String,
        /// Sampled elements.
        sample: Vec<String>,
    },
    /// No renderer, or a declined render.
    Opaque {
        /// Type or schema hint.
        type_name: String,
        /// One-line summary.
        summary: String,
    },
}

/// One field's render.
#[derive(Clone, Debug, Serialize)]
pub struct FieldRenderWire {
    /// Field index.
    pub field: u16,
    /// Display model.
    pub model: ModelWire,
    /// Elements before capping.
    pub total: usize,
    /// Whether the cap removed elements.
    pub truncated: bool,
}

/// A View's rendered body.
#[derive(Clone, Debug, Serialize)]
pub struct ViewRenderWire {
    /// View key.
    pub view: String,
    /// Content version this render was built from.
    pub content_version: Version,
    /// Revision at projection time.
    pub revision: Revision,
    /// Who produced the display data.
    pub source: &'static str,
    /// Whether a renderer claimed a unit the runtime does not have.
    pub inconsistent: bool,
    /// Field renders.
    pub fields: Vec<FieldRenderWire>,
}

/// One field's metadata.
#[derive(Clone, Debug, Serialize)]
pub struct FieldWire {
    /// Field index.
    pub field: u16,
    /// Declared name.
    pub name: &'static str,
    /// Declared shape.
    pub kind: &'static str,
    /// Element identity.
    pub granularity: &'static str,
    /// Installed units.
    pub units: usize,
    /// Display elements.
    pub elements: Option<usize>,
    /// Content version.
    pub content_version: Version,
    /// Units changed at the selected revision.
    pub changed_units: u32,
    /// How the field is drawn: a host renderer, or the generic drawing built
    /// from the runtime's facts.
    pub render: &'static str,
}

/// One View's metadata.
#[derive(Clone, Debug, Serialize)]
pub struct ViewWire {
    /// View key.
    pub view: String,
    /// Fully qualified name.
    pub name: &'static str,
    /// Source or derived.
    pub kind: &'static str,
    /// Declared fields.
    pub fields: Vec<FieldWire>,
    /// Supplying component, when the projection can name one.
    pub producer: Option<String>,
    /// Region routing generation.
    pub routing: u64,
    /// Content version.
    pub content_version: Version,
    /// Whether committed content exists.
    pub demanded: bool,
    /// Units changed at the selected revision.
    pub changed_units: u32,
    /// Whether the host exposes a text editor for this View.
    pub editable: bool,
    /// Header state.
    pub state: &'static str,
}

/// Exact runtime counters.
#[derive(Clone, Debug, Serialize)]
pub struct StatsWire {
    /// Allocated nodes.
    pub nodes: usize,
    /// Nodes created over the lifetime.
    pub nodes_created: u64,
    /// Published revisions.
    pub revisions: u64,
    /// Successful commits.
    pub executions: u64,
    /// Successful trace validations.
    pub validations: u64,
    /// Validation attempts.
    pub validation_attempts: u64,
    /// Validation rejections.
    pub validation_failures: u64,
    /// Failed or cancelled attempts.
    pub execution_failures: u64,
    /// Equal-result commits.
    pub cutoffs: u64,
    /// Suspended attempts.
    pub suspensions: u64,
    /// Evaluating demand tasks.
    pub demands: u64,
    /// Fired callbacks.
    pub notifications: u64,
    /// Invalidation visits.
    pub invalidations: u64,
    /// Queued tasks.
    pub queued: usize,
    /// Running tasks.
    pub running: usize,
    /// Suspended tasks.
    pub waiting: usize,
}

impl StatsWire {
    /// Projects runtime counters.
    #[must_use]
    pub fn from_stats(stats: &RuntimeStats) -> Self {
        Self {
            nodes: stats.nodes,
            nodes_created: stats.nodes_created,
            revisions: stats.revisions,
            executions: stats.executions,
            validations: stats.validations,
            validation_attempts: stats.validation_attempts,
            validation_failures: stats.validation_failures,
            execution_failures: stats.execution_failures,
            cutoffs: stats.cutoffs,
            suspensions: stats.suspensions,
            demands: stats.demands,
            notifications: stats.notifications,
            invalidations: stats.invalidations,
            queued: stats.tasks.queued,
            running: stats.tasks.running,
            waiting: stats.tasks.waiting,
        }
    }
}

/// The workspace frame: every View's metadata, no bodies.
#[derive(Clone, Debug, Serialize)]
pub struct WorkspaceWire {
    /// Sequence this frame was taken at.
    pub seq: u64,
    /// Revision this frame describes.
    pub revision: Revision,
    /// Runtime name, when a root is installed.
    pub runtime: Option<String>,
    /// Recording level.
    pub level: &'static str,
    /// Views.
    pub views: Vec<ViewWire>,
    /// Every component, with the Views it reads and defines.
    pub components: Vec<ComponentWire>,
    /// Counters.
    pub stats: StatsWire,
    /// Events lost by the recorder.
    pub dropped: u64,
    /// The most recent transaction that published nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_noop: Option<String>,
    /// Revision that transaction was evaluated at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_noop_revision: Option<Revision>,
}

/// One component and the Views on either side of it.
#[derive(Clone, Debug, Serialize)]
pub struct ComponentWire {
    /// Stable component key.
    pub component: String,
    /// Fully qualified name.
    pub name: &'static str,
    /// `root`, `general`, or `invoked`.
    pub kind: &'static str,
    /// Views this component reads.
    pub reads: Vec<String>,
    /// Views this component defines.
    pub writes: Vec<String>,
    /// Revision the component last executed at.
    pub executed_at: Revision,
    /// Revision the component was last invalidated at.
    pub suspect_at: Revision,
}

/// Projects one component for the page.
#[must_use]
pub fn component_wire(component: &reactive_runtime::observe::ComponentFacts) -> ComponentWire {
    ComponentWire {
        component: format!("c:{}", component.id.0),
        name: component.name,
        kind: component.kind.as_str(),
        reads: component.reads.iter().map(|view| view_key(*view)).collect(),
        writes: component
            .writes
            .iter()
            .map(|view| view_key(*view))
            .collect(),
        executed_at: component.executed_at,
        suspect_at: component.suspect_at,
    }
}

/// One node's metadata./// One node's metadata.
#[derive(Clone, Debug, Serialize)]
pub struct NodeWire {
    /// Node key.
    pub key: String,
    /// Kind.
    pub kind: &'static str,
    /// Recorded label.
    pub label: &'static str,
    /// Scope identity, opaque to the page.
    pub scope: String,
    /// Owning component, when known.
    pub component: Option<String>,
    /// Logical unit, when the node carries one.
    pub unit: Option<UnitWire>,
    /// Change version.
    pub changed_at: Version,
    /// Verification revision.
    pub verified_at: Revision,
    /// Last invalidation revision.
    pub suspect_at: Revision,
    /// Last execution revision.
    pub executed_at: Revision,
    /// Last validation revision.
    pub validated_at: Revision,
    /// Routing generation.
    pub routing_at: Revision,
    /// Suspended tasks.
    pub waiters: u32,
    /// Payload size.
    pub value_size: Option<usize>,
    /// Server-derived state at the projection revision.
    pub state: RowState,
}

/// One recorded dependency.
#[derive(Clone, Debug, Serialize)]
pub struct DepWire {
    /// Observed node key.
    pub node: String,
    /// Observed node label.
    pub label: &'static str,
    /// Recorded version.
    pub recorded: Version,
    /// Current version.
    pub current: Version,
    /// Whether the observation moved.
    pub changed: bool,
    /// Whether the observed node is itself awaiting validation.
    pub suspect: bool,
}

/// A node's explanation.
#[derive(Clone, Debug, Serialize)]
pub struct NodeDetailWire {
    /// Node facts.
    pub node: NodeWire,
    /// Recorded dependencies.
    pub dependencies: Vec<DepWire>,
    /// Value preview, when a formatter exists.
    pub value: Option<ValueWire>,
    /// Whether the node was reclaimed between probe and detail.
    pub reclaimed: bool,
}

/// A host-formatted value preview.
#[derive(Clone, Debug, Serialize)]
pub struct ValueWire {
    /// Type or schema hint.
    pub kind: String,
    /// One-line form.
    pub summary: String,
    /// Longer form.
    pub detail: Option<String>,
    /// Bytes.
    pub bytes: usize,
    /// Whether truncated.
    pub truncated: bool,
    /// Who formatted it.
    pub source: &'static str,
}

/// One revision's exact reaction.
#[derive(Clone, Debug, Serialize)]
pub struct RevisionWire {
    /// Revision number.
    pub revision: Revision,
    /// Transaction label.
    pub label: String,
    /// Staged writes.
    pub staged: u32,
    /// Units whose value changed.
    pub changed: u32,
    /// Nodes newly marked suspect.
    pub invalidated: u64,
    /// Whether a revision was published.
    pub published: bool,
    /// Evaluating demand tasks.
    pub demands: u32,
    /// Distinct demand causes.
    pub demand_causes: Vec<String>,
    /// Successful commits.
    pub executed: u32,
    /// Successful validations.
    pub validated: u32,
    /// Equal-result commits.
    pub cutoffs: u32,
    /// Failed attempts.
    pub failed: u32,
    /// Fired callbacks.
    pub notifications: u32,
    /// Commit duration.
    pub duration_ns: u64,
    /// Views with changed units.
    pub changed_views: Vec<String>,
    /// Views redefined to an equal value.
    pub cutoff_views: Vec<String>,
    /// Nodes that executed.
    pub executed_nodes: Vec<String>,
    /// Nodes that committed an equal value.
    pub cutoff_nodes: Vec<String>,
    /// Nodes that were invalidated first.
    pub invalidated_seeds: Vec<String>,
    /// Whether activity sets were capped.
    pub activity_capped: bool,
    /// Whether the recorder dropped events for this revision.
    pub incomplete: bool,
}

/// A runtime event, as the page sees it.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventWire {
    /// A transaction reached its decision.
    Transaction {
        /// Revision.
        revision: Revision,
        /// Label.
        label: String,
        /// Staged writes.
        staged: u32,
        /// Changed units.
        changed: u32,
        /// Published?
        published: bool,
        /// Invalidation count.
        invalidated: u64,
        /// Views with a changed unit.
        views: Vec<String>,
        /// Duration.
        duration_ns: u64,
    },
    /// A reverse-dependency walk ran.
    Invalidated {
        /// Revision.
        revision: Revision,
        /// Exact count.
        count: u64,
        /// Capped seeds.
        seeds: Vec<String>,
    },
    /// A demand started.
    Demand {
        /// Task id.
        task: u64,
        /// Cause.
        cause: &'static str,
        /// Subscription id, when the cause is a refresh.
        subscription: Option<u64>,
    },
    /// A demand finished.
    DemandDone {
        /// Task id.
        task: u64,
        /// Success.
        ok: bool,
        /// Duration.
        duration_ns: u64,
    },
    /// A node was claimed.
    Claim {
        /// Node key.
        node: String,
        /// Task id.
        task: u64,
    },
    /// A node validated.
    Validated {
        /// Node key.
        node: String,
        /// Observations checked.
        deps: u32,
    },
    /// A node committed.
    Committed {
        /// Node key.
        node: String,
        /// Owning View, when the node carries a unit.
        view: Option<String>,
        /// Whether the value changed.
        changed: bool,
        /// Duration.
        duration_ns: u64,
    },
    /// A node failed.
    Failed {
        /// Node key.
        node: String,
        /// Error kind.
        error: &'static str,
    },
    /// A node was reused.
    Reused {
        /// Node key.
        node: String,
    },
    /// A callback fired.
    Notified {
        /// View key.
        view: String,
        /// Previous version.
        previous: Version,
        /// Current version.
        current: Version,
    },
    /// A task began on a worker.
    Running {
        /// Task id.
        task: u64,
    },
    /// A task suspended.
    Waiting {
        /// Task id.
        task: u64,
        /// Awaited node.
        node: String,
    },
    /// A task was queued again.
    Woken {
        /// Task id.
        task: u64,
        /// Completed node.
        node: String,
    },
    /// Reclamation ran.
    Collected {
        /// Nodes reclaimed.
        nodes: u32,
        /// Scopes reclaimed.
        scopes: u32,
    },
    /// The catalog changed.
    Catalog,
}

/// One frame of events.
#[derive(Clone, Debug, Serialize)]
pub struct EventsWire {
    /// Sequence of this frame.
    pub seq: u64,
    /// Events in order.
    pub events: Vec<EventWire>,
}

/// Header state for a View, from its facts and the selected revision's activity.
#[must_use]
pub fn view_state(
    facts: &ViewFacts,
    record: Option<&RevisionRecord>,
    revision: Revision,
) -> &'static str {
    if facts.kind == reactive_core::descriptor::ViewKind::Derived && !facts.demanded {
        return "not_demanded";
    }
    // A keyed View keeps its content (the identity sequence) across a value-only
    // edit, so the version alone is not enough: the units that carry the data say
    // what moved, and they are the ones a reader sees.
    if facts.content_version == revision
        || facts.changed_units > 0
        || record.is_some_and(|record| record.changed_views.contains(&facts.view))
    {
        return "changed";
    }
    if let Some(record) = record
        && (record.cutoff_views.contains(&facts.view)
            || record.activity.cutoff.contains(&facts.content_node))
    {
        return "cutoff";
    }
    if let Some(record) = record
        && let Some(provider) = facts.provider_node
        && record.activity.executed.contains(&provider)
    {
        // The producer ran in this revision and the content did not move.
        return "cutoff";
    }
    if facts.suspect {
        return "suspect";
    }
    "clean"
}

/// Projects View facts.
#[must_use]
pub fn view_wire(
    facts: &ViewFacts,
    record: Option<&RevisionRecord>,
    revision: Revision,
    editable: bool,
) -> ViewWire {
    ViewWire {
        view: view_key(facts.view),
        name: facts.name,
        kind: match facts.kind {
            reactive_core::descriptor::ViewKind::Source => "source",
            reactive_core::descriptor::ViewKind::Derived => "derived",
        },
        fields: facts
            .fields
            .iter()
            .map(|field| field_wire(field, facts.content_version, record, facts.view))
            .collect(),
        producer: facts
            .producer
            .map(|component| format!("c:{:016x}", component.0)),
        routing: facts.routing,
        content_version: facts.content_version,
        demanded: facts.demanded,
        changed_units: facts.changed_units,
        editable,
        state: view_state(facts, record, revision),
    }
}

fn field_wire(
    field: &FieldFacts,
    content_version: Version,
    record: Option<&RevisionRecord>,
    view: ViewId,
) -> FieldWire {
    let changed_units = record.map_or(field.changed_units, |record| {
        // For an older selected revision the journal's activity is the truth;
        // the region can only describe "now".
        u32::try_from(
            record
                .activity
                .changed
                .iter()
                .filter(|node| record.view_of(**node) == Some(view))
                .count(),
        )
        .unwrap_or(u32::MAX)
    });
    FieldWire {
        field: field.field.0,
        name: field.name,
        kind: field_kind(field.kind),
        granularity: field.granularity.as_str(),
        units: field.units,
        elements: field.elements,
        content_version,
        changed_units,
        render: "generic",
    }
}

/// Stable field-kind names for the wire.
#[must_use]
pub fn field_kind(kind: reactive_core::descriptor::FieldKind) -> &'static str {
    use reactive_core::descriptor::FieldKind;
    match kind {
        FieldKind::Scalar => "scalar",
        FieldKind::List => "list",
        FieldKind::Set => "set",
        FieldKind::Map => "map",
        FieldKind::Graph => "graph",
        FieldKind::Tree => "tree",
    }
}

/// Stable unit-kind names for the wire.
#[must_use]
pub fn unit_kind(kind: reactive_core::descriptor::UnitKind) -> &'static str {
    use reactive_core::descriptor::UnitKind;
    match kind {
        UnitKind::Value => "value",
        UnitKind::Membership => "membership",
        UnitKind::Structure => "structure",
        UnitKind::Rank => "rank",
        UnitKind::Adjacency => "adjacency",
        UnitKind::IncomingAdjacency => "incoming_adjacency",
        UnitKind::Content => "content",
    }
}

/// Node state names for the wire.
#[must_use]
pub fn node_state(facts: &NodeFacts, revision: Revision) -> RowState {
    if facts.suspect_at > facts.verified_at {
        RowState::Suspect
    } else if facts.changed_at == revision {
        RowState::Changed
    } else {
        RowState::Clean
    }
}

/// Projects node facts.
#[must_use]
pub fn node_wire(facts: &NodeFacts, revision: Revision) -> NodeWire {
    NodeWire {
        key: node_key(facts.id),
        kind: facts.kind,
        label: facts.label,
        scope: format!("s:{}", facts.scope.0),
        component: facts
            .component
            .map(|component| format!("c:{:016x}", component.0)),
        unit: facts.unit.as_ref().map(|unit| UnitWire {
            view: view_key(unit.view),
            field: unit.field.0,
            kind: unit_kind(unit.kind),
            entity: unit.entity.clone(),
            changed_at: Some(facts.changed_at),
        }),
        changed_at: facts.changed_at,
        verified_at: facts.verified_at,
        suspect_at: facts.suspect_at,
        executed_at: facts.executed_at,
        validated_at: facts.validated_at,
        routing_at: facts.routing_at,
        waiters: facts.waiters,
        value_size: facts.value_size,
        state: node_state(facts, revision),
    }
}

/// Projects an explanation.
#[must_use]
pub fn node_detail_wire(explanation: &Explanation, revision: Revision) -> NodeDetailWire {
    NodeDetailWire {
        node: NodeWire {
            key: node_key(explanation.node),
            kind: "node",
            label: explanation.label,
            scope: format!("s:{}", explanation.scope.0),
            component: None,
            unit: None,
            changed_at: explanation.value_changed_at,
            verified_at: explanation.verified_at,
            suspect_at: explanation.suspect_at,
            executed_at: explanation.executed_at,
            validated_at: explanation.validated_at,
            routing_at: 0,
            waiters: u32::try_from(explanation.waiters).unwrap_or(u32::MAX),
            value_size: None,
            state: if explanation.suspect_at > explanation.verified_at {
                RowState::Suspect
            } else if explanation.value_changed_at == revision {
                RowState::Changed
            } else {
                RowState::Clean
            },
        },
        dependencies: explanation
            .dependencies
            .iter()
            .map(|dep| DepWire {
                node: node_key(dep.node),
                label: dep.label,
                recorded: dep.recorded,
                current: dep.current,
                changed: dep.changed,
                suspect: dep.suspect,
            })
            .collect(),
        value: None,
        reclaimed: false,
    }
}

/// Projects a revision record.
#[must_use]
pub fn revision_wire(record: &RevisionRecord) -> RevisionWire {
    RevisionWire {
        revision: record.revision,
        label: record.label.clone(),
        staged: record.staged,
        changed: record.changed,
        invalidated: record.invalidated,
        published: record.published,
        demands: record.demands,
        demand_causes: record
            .demand_causes
            .iter()
            .map(|cause| (*cause).to_owned())
            .collect(),
        executed: record.activity.executed.len() as u32,
        validated: record.activity.validated.len() as u32,
        cutoffs: record.activity.cutoff.len() as u32,
        failed: record.activity.failed.len() as u32,
        notifications: record.notifications,
        duration_ns: record.duration_ns,
        changed_views: record
            .changed_views
            .iter()
            .map(|view| view_key(*view))
            .collect(),
        cutoff_views: record
            .cutoff_views
            .iter()
            .map(|view| view_key(*view))
            .collect(),
        executed_nodes: record
            .activity
            .executed
            .iter()
            .map(|node| node_key(*node))
            .collect(),
        cutoff_nodes: record
            .activity
            .cutoff
            .iter()
            .map(|node| node_key(*node))
            .collect(),
        invalidated_seeds: record
            .invalidated_seeds
            .iter()
            .map(|node| node_key(*node))
            .collect(),
        activity_capped: record.activity.capped,
        incomplete: record.incomplete,
    }
}

/// Projects a projection revision into the wire form used by `/snapshot`.
#[must_use]
pub fn graph_wire(facts: &GraphFacts) -> GraphWire {
    GraphWire {
        revision: facts.revision,
        nodes: facts
            .nodes
            .iter()
            .map(|node| node_wire(node, facts.revision))
            .collect(),
        edges: facts
            .edges
            .iter()
            .map(|edge| GraphEdgeWire {
                from: node_key(edge.from),
                to: node_key(edge.to),
                label: if edge.recorded == edge.current {
                    None
                } else {
                    Some(format!("{} -> {}", edge.recorded, edge.current))
                },
            })
            .collect(),
        total_nodes: facts.total_nodes,
        truncated: facts.truncated,
    }
}

/// A bounded node-link projection, for the Runtime mode.
#[derive(Clone, Debug, Serialize)]
pub struct GraphWire {
    /// Revision.
    pub revision: Revision,
    /// Nodes.
    pub nodes: Vec<NodeWire>,
    /// Edges.
    pub edges: Vec<GraphEdgeWire>,
    /// Matching nodes before the cap.
    pub total_nodes: usize,
    /// Whether the cap excluded nodes.
    pub truncated: bool,
}
