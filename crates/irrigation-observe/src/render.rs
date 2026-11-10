//! Display models built from runtime facts.
//!
//! These are internal: the projection in [`crate::generic`] builds them from a
//! View's descriptor, entities, and unit versions, then [`crate::project`]
//! resolves each row's claim against the runtime and writes the wire model. No
//! host code touches a type in this module.
//!
//! A row may name the unit it corresponds to ([`UnitRef`]); the projection
//! checks that unit against the owning region and attaches change flags itself.
//! A row without a verifiable unit is a display row, and the page says so.

use std::fmt::Debug;
use std::sync::Arc;

use irrigation_core::descriptor::UnitKind;
use irrigation_core::ids::{FieldId, ViewId};
use irrigation_core::key::AnyKey;

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
    /// A claim copied from a unit key the runtime itself produced.
    #[must_use]
    pub fn from_key(key: &irrigation_core::descriptor::UnitKey) -> Self {
        Self {
            view: key.view,
            field: key.field,
            kind: key.kind,
            entity: key.entity.clone(),
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
        /// The aggregate, spelled by the View's describe hook.
        aggregate: String,
        /// The field the aggregate belongs to.
        reduction: String,
        /// Sampled elements, explicitly a sample.
        sample: Vec<String>,
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
        }
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
    /// Builds a render with an explicit pre-cap total.
    #[cfg(test)]
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
    #[cfg(test)]
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
        }
        self.truncated = total > self.model.len() || self.truncated;
        self.total = total;
        self
    }
}
