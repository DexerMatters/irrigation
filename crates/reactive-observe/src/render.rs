//! Host-rendered View bodies.
//!
//! The runtime stores typed user data; only the host knows that a `Syntax`
//! field is an AST or that a `Tokens` field is a token stream. A renderer turns
//! a committed [`Snapshot`] into bounded display rows. It runs on a render
//! worker, outside the runtime lock, and it never calls the runtime.
//!
//! Runtime truth is not the renderer's to declare. Rows may name the unit they
//! correspond to ([`UnitRef`]); the server checks that unit against the owning
//! region and attaches change flags itself. A row without a verifiable unit is
//! a display row, and the page says so.

use std::any::Any;
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::Arc;

use reactive_core::descriptor::UnitKind;
use reactive_core::ids::{FieldId, ViewId};
use reactive_core::key::{AnyKey, any_key};
use reactive_runtime::snapshot::Snapshot;

/// A row's claim that it corresponds to one logical unit.
#[derive(Clone)]
pub struct UnitRef {
    /// Owning View.
    pub view: ViewId,
    /// Field within the View.
    pub field: FieldId,
    /// What is observed.
    pub kind: UnitKind,
    /// Entity identity, for entity-scoped units.
    pub entity: Option<Arc<dyn AnyKey>>,
}

impl Debug for UnitRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnitRef")
            .field("view", &self.view)
            .field("field", &self.field)
            .field("kind", &self.kind)
            .field("entity", &self.entity.as_ref().map(|key| key.key_display()))
            .finish()
    }
}

impl UnitRef {
    /// A unit for one entity: a list item, map key, tree node, or member.
    #[must_use]
    pub fn entity<K>(view: ViewId, field: FieldId, kind: UnitKind, key: K) -> Self
    where
        K: Any + Hash + Eq + Send + Sync + Debug + 'static,
    {
        Self {
            view,
            field,
            kind,
            entity: Some(any_key(key)),
        }
    }

    /// A claim copied from a unit key the runtime itself produced.
    #[must_use]
    pub fn from_key(key: &reactive_core::descriptor::UnitKey) -> Self {
        Self {
            view: key.view,
            field: key.field,
            kind: key.kind,
            entity: key.entity.clone(),
        }
    }

    /// A whole-field unit.
    #[must_use]
    pub fn field(view: ViewId, field: FieldId, kind: UnitKind) -> Self {
        Self {
            view,
            field,
            kind,
            entity: None,
        }
    }
}

/// One row of a tree module.
#[derive(Clone, Debug)]
pub struct TreeRow {
    /// Stable row identity within this render.
    pub key: String,
    /// Nesting depth, zero at the root.
    pub depth: u16,
    /// Kind label (`Let`, `Call`, `Module`).
    pub kind: String,
    /// Display label, usually the node's name or tag.
    pub label: String,
    /// Optional short value shown next to the label.
    pub value: Option<String>,
    /// The unit this row displays, when it has one.
    pub unit: Option<UnitRef>,
}

/// One row of a list module.
#[derive(Clone, Debug)]
pub struct ListRow {
    /// Zero-based display index.
    pub index: usize,
    /// Display value.
    pub value: String,
    /// The unit this row displays, when it has one.
    pub unit: Option<UnitRef>,
}

/// One entry of a map module.
#[derive(Clone, Debug)]
pub struct MapRow {
    /// Display key.
    pub key: String,
    /// Display value; `None` renders as absent.
    pub value: Option<String>,
    /// The unit this entry displays, when it has one.
    pub unit: Option<UnitRef>,
}

/// One member of a set module.
#[derive(Clone, Debug)]
pub struct SetMember {
    /// Display member.
    pub member: String,
    /// Whether the member is present.
    pub present: bool,
    /// The membership unit, when it has one.
    pub unit: Option<UnitRef>,
}

/// One node of a graph module.
#[derive(Clone, Debug)]
pub struct GraphNode {
    /// Stable node identity.
    pub id: String,
    /// Display label.
    pub label: String,
    /// The unit this node displays, when it has one.
    pub unit: Option<UnitRef>,
}

/// One edge of a graph module.
#[derive(Clone, Debug)]
pub struct GraphEdge {
    /// Source node identity.
    pub from: String,
    /// Destination node identity.
    pub to: String,
    /// Optional edge label.
    pub label: Option<String>,
}

/// One scalar or record field row.
#[derive(Clone, Debug)]
pub struct ScalarRow {
    /// Field name.
    pub name: String,
    /// Display value.
    pub value: String,
    /// The unit this row displays, when it has one.
    pub unit: Option<UnitRef>,
}

/// A bounded display model for one field.
#[derive(Clone, Debug)]
pub enum RenderModel {
    /// An indented outline.
    Tree(Vec<TreeRow>),
    /// Ordered rows.
    List(Vec<ListRow>),
    /// Key/value entries.
    Map(Vec<MapRow>),
    /// Membership.
    Set(Vec<SetMember>),
    /// A small network.
    Graph {
        /// Nodes.
        nodes: Vec<GraphNode>,
        /// Edges, as node identity pairs.
        edges: Vec<GraphEdge>,
    },
    /// Key/value fields.
    Scalar(Vec<ScalarRow>),
    /// A declared aggregate plus a bounded sample.
    Reduced {
        /// The aggregate, formatted by the host.
        aggregate: String,
        /// The reduction's name.
        reduction: String,
        /// Sampled elements, explicitly a sample.
        sample: Vec<String>,
    },
    /// No renderer, or a renderer that declined.
    Opaque {
        /// Concrete type or schema hint.
        kind: String,
        /// One-line summary.
        summary: String,
    },
}

impl RenderModel {
    /// Number of rows, nodes, or members in this model.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Tree(rows) => rows.len(),
            Self::List(rows) => rows.len(),
            Self::Map(rows) => rows.len(),
            Self::Set(members) => members.len(),
            Self::Graph { nodes, .. } => nodes.len(),
            Self::Scalar(rows) => rows.len(),
            Self::Reduced { sample, .. } => sample.len(),
            Self::Opaque { .. } => 0,
        }
    }

    /// Whether the model has nothing to draw.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One field's render, with its pre-cap total.
#[derive(Clone, Debug)]
pub struct FieldRender {
    /// Field this model belongs to.
    pub field: FieldId,
    /// The display model, after capping.
    pub model: RenderModel,
    /// Elements before capping.
    pub total: usize,
    /// Whether the cap removed elements.
    pub truncated: bool,
}

impl FieldRender {
    /// Builds a render whose total is the model's own length.
    #[must_use]
    pub fn new(field: FieldId, model: RenderModel) -> Self {
        Self {
            field,
            total: model.len(),
            model,
            truncated: false,
        }
    }

    /// Builds a render with an explicit pre-cap total.
    #[must_use]
    pub fn capped(field: FieldId, model: RenderModel, total: usize) -> Self {
        Self {
            field,
            truncated: total > model.len(),
            total,
            model,
        }
    }

    /// Applies a row cap, keeping an exact total.
    #[must_use]
    pub fn with_cap(mut self, cap: usize) -> Self {
        let total = self.total.max(self.model.len());
        match &mut self.model {
            RenderModel::Tree(rows) => rows.truncate(cap),
            RenderModel::List(rows) => rows.truncate(cap),
            RenderModel::Map(rows) => rows.truncate(cap),
            RenderModel::Set(members) => members.truncate(cap),
            RenderModel::Scalar(rows) => rows.truncate(cap),
            RenderModel::Reduced { sample, .. } => sample.truncate(cap.min(50)),
            RenderModel::Graph { nodes, edges } => {
                nodes.truncate(cap);
                let kept: std::collections::HashSet<&str> =
                    nodes.iter().map(|node| node.id.as_str()).collect();
                edges.retain(|edge| kept.contains(edge.from.as_str()));
                self.total = nodes.len();
            }
            RenderModel::Opaque { .. } => {}
        }
        self.truncated = total > self.model.len() || self.truncated;
        self.total = total;
        self
    }
}

/// Why a render failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderError(pub String);

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RenderError {}

/// A host renderer for one View.
pub trait ViewRenderer: Send + Sync + 'static {
    /// The View this renderer draws.
    fn view(&self) -> ViewId;

    /// Draws the View from a committed snapshot.
    ///
    /// Runs on a render worker: it must be bounded, pure, and must not call the
    /// runtime.
    fn render(&self, snapshot: &Snapshot) -> Result<Vec<FieldRender>, RenderError>;
}

struct SingleField<V, F> {
    render: F,
    marker: std::marker::PhantomData<fn() -> V>,
}

impl<V, F> ViewRenderer for SingleField<V, F>
where
    V: reactive_runtime::snapshot::ViewSchema,
    F: Fn(&Snapshot) -> RenderModel + Send + Sync + 'static,
{
    fn view(&self) -> ViewId {
        V::view_id()
    }

    fn render(&self, snapshot: &Snapshot) -> Result<Vec<FieldRender>, RenderError> {
        Ok(vec![FieldRender::new(FieldId(0), (self.render)(snapshot))])
    }
}

/// Wraps a single-field render closure for `V`.
#[must_use]
pub fn single_field<V, F>(render: F) -> Arc<dyn ViewRenderer>
where
    V: reactive_runtime::snapshot::ViewSchema,
    F: Fn(&Snapshot) -> RenderModel + Send + Sync + 'static,
{
    Arc::new(SingleField::<V, F> {
        render,
        marker: std::marker::PhantomData,
    })
}

struct MultiField<V, F> {
    render: F,
    marker: std::marker::PhantomData<fn() -> V>,
}

impl<V, F> ViewRenderer for MultiField<V, F>
where
    V: reactive_runtime::snapshot::ViewSchema,
    F: Fn(&Snapshot) -> Vec<FieldRender> + Send + Sync + 'static,
{
    fn view(&self) -> ViewId {
        V::view_id()
    }

    fn render(&self, snapshot: &Snapshot) -> Result<Vec<FieldRender>, RenderError> {
        Ok((self.render)(snapshot))
    }
}

/// Wraps a multi-field render closure for `V`.
#[must_use]
pub fn multi_field<V, F>(render: F) -> Arc<dyn ViewRenderer>
where
    V: reactive_runtime::snapshot::ViewSchema,
    F: Fn(&Snapshot) -> Vec<FieldRender> + Send + Sync + 'static,
{
    Arc::new(MultiField::<V, F> {
        render,
        marker: std::marker::PhantomData,
    })
}

/// A bounded preview of one value, for the inspector.
#[derive(Clone, Debug)]
pub struct ValuePreview {
    /// Concrete or schema type hint.
    pub kind: String,
    /// Short one-line form.
    pub summary: String,
    /// Longer form, when useful.
    pub detail: Option<String>,
    /// Payload size in bytes.
    pub bytes: usize,
    /// Whether the preview was truncated.
    pub truncated: bool,
}

impl ValuePreview {
    /// An opaque preview: type, size, and nothing invented.
    #[must_use]
    pub fn opaque(kind: impl Into<String>, bytes: usize) -> Self {
        Self {
            kind: kind.into(),
            summary: format!("opaque value, {bytes} B"),
            detail: None,
            bytes,
            truncated: false,
        }
    }
}

/// A host formatter for one View's selected value.
pub trait ValueFormatter: Send + Sync + 'static {
    /// The View this formatter previews.
    fn view(&self) -> ViewId;

    /// Formats the current content of the View.
    fn format(&self, snapshot: &Snapshot) -> ValuePreview;
}

struct ClosureFormatter<V, F> {
    format: F,
    marker: std::marker::PhantomData<fn() -> V>,
}

impl<V, F> ValueFormatter for ClosureFormatter<V, F>
where
    V: reactive_runtime::snapshot::ViewSchema,
    F: Fn(&Snapshot) -> ValuePreview + Send + Sync + 'static,
{
    fn view(&self) -> ViewId {
        V::view_id()
    }

    fn format(&self, snapshot: &Snapshot) -> ValuePreview {
        (self.format)(snapshot)
    }
}

/// Wraps a value-preview closure for `V`.
#[must_use]
pub fn formatter<V, F>(format: F) -> Arc<dyn ValueFormatter>
where
    V: reactive_runtime::snapshot::ViewSchema,
    F: Fn(&Snapshot) -> ValuePreview + Send + Sync + 'static,
{
    Arc::new(ClosureFormatter::<V, F> {
        format,
        marker: std::marker::PhantomData,
    })
}
