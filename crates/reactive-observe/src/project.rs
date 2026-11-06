//! Projection: runtime facts and host renders into wire shapes.
//!
//! This module owns the rule that keeps the page honest: the runtime, not the
//! renderer, decides whether a row corresponds to a unit and what that unit's
//! state is. A renderer supplies structure, labels, and an optional
//! [`crate::render::UnitRef`]; anything it cannot back with a real unit becomes
//! a display row.

use std::collections::HashSet;
use std::sync::Arc;

use reactive_core::descriptor::UnitKey;
use reactive_core::ids::{FieldId, Revision, ViewId};
use reactive_runtime::Runtime;
use reactive_runtime::observe::{FieldFacts, Granularity, ViewFacts};
use reactive_runtime::snapshot::Snapshot;

use crate::render::{
    FieldRender, GraphEdge, GraphNode, ListRow, MapRow, RenderError, RenderModel, ScalarRow,
    SetMember, TreeRow, ValueFormatter, ValuePreview, ViewRenderer,
};
use crate::wire::{
    FieldRenderWire, GraphEdgeWire, GraphNodeWire, ListRowWire, MapRowWire, ModelWire,
    NodeDetailWire, RowState, ScalarRowWire, SetMemberWire, TreeRowWire, UnitWire, ValueWire,
    ViewRenderWire, ViewWire, node_detail_wire, node_wire, view_key, view_wire,
};

/// Server-side caps.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    /// Rows per list, map, or scalar render.
    pub rows: usize,
    /// Tree rows.
    pub tree_rows: usize,
    /// Set members.
    pub set_members: usize,
    /// Graph nodes inside a module.
    pub graph_nodes: usize,
    /// Graph projection nodes.
    pub node_limit: usize,
    /// Graph projection edges.
    pub edge_limit: usize,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            rows: 200,
            tree_rows: 250,
            set_members: 300,
            graph_nodes: 80,
            node_limit: 800,
            edge_limit: 2_500,
        }
    }
}

/// Registered host renderers and formatters.
#[derive(Default)]
pub struct Renderers {
    renderers: Vec<Arc<dyn ViewRenderer>>,
    formatters: Vec<Arc<dyn ValueFormatter>>,
}

impl std::fmt::Debug for Renderers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderers")
            .field("renderers", &self.renderers.len())
            .field("formatters", &self.formatters.len())
            .finish()
    }
}

impl Renderers {
    /// Registers a renderer, replacing any renderer for the same View.
    pub fn insert(&mut self, renderer: Arc<dyn ViewRenderer>) {
        self.renderers
            .retain(|existing| existing.view() != renderer.view());
        self.renderers.push(renderer);
    }

    /// Registers a value formatter, replacing any formatter for the same View.
    pub fn insert_formatter(&mut self, formatter: Arc<dyn ValueFormatter>) {
        self.formatters
            .retain(|existing| existing.view() != formatter.view());
        self.formatters.push(formatter);
    }

    /// Whether a renderer exists for `view`.
    #[must_use]
    pub fn has(&self, view: ViewId) -> bool {
        self.renderers
            .iter()
            .any(|renderer| renderer.view() == view)
    }

    /// Renders a View, when a renderer is registered.
    #[must_use]
    pub fn render(
        &self,
        view: ViewId,
        snapshot: &Snapshot,
    ) -> Option<Result<Vec<FieldRender>, RenderError>> {
        self.renderers
            .iter()
            .find(|renderer| renderer.view() == view)
            .map(|renderer| renderer.render(snapshot))
    }

    /// Formats a View's value, when a formatter is registered.
    #[must_use]
    pub fn format(&self, view: ViewId, snapshot: &Snapshot) -> Option<ValuePreview> {
        self.formatters
            .iter()
            .find(|formatter| formatter.view() == view)
            .map(|formatter| formatter.format(snapshot))
    }
}

/// The workspace view list, with renderer presence filled in.
#[must_use]
pub fn workspace_views(
    facts: &[ViewFacts],
    revision: Revision,
    current: Option<&crate::journal::RevisionRecord>,
    renderers: &Renderers,
    editable: &dyn Fn(reactive_core::ids::ViewId) -> bool,
) -> Vec<ViewWire> {
    facts
        .iter()
        .map(|view| {
            let mut wire = view_wire(view, current, revision, editable(view.view));
            let render = if renderers.has(view.view) {
                "host"
            } else {
                "generic"
            };
            for field in &mut wire.fields {
                field.render = render;
            }
            wire
        })
        .collect()
}

/// One row's unit claim, owned for validation.
struct Claim {
    view: ViewId,
    field: FieldId,
    kind: reactive_core::descriptor::UnitKind,
    entity: Option<Arc<dyn reactive_core::key::AnyKey>>,
}

impl Claim {
    fn of(unit: &crate::render::UnitRef) -> Self {
        Self {
            view: unit.view,
            field: unit.field,
            kind: unit.kind,
            entity: unit.entity.clone(),
        }
    }

    fn key(&self) -> UnitKey {
        UnitKey {
            view: self.view,
            field: self.field,
            kind: self.kind,
            entity: self.entity.clone(),
        }
    }
}

/// The resolved unit and state for one row.
type RowFacts = (Option<UnitWire>, RowState);

/// Resolves every row's unit claim against the runtime in one lock.
fn resolve(
    runtime: &Runtime,
    claims: &[Option<Claim>],
    revision: Revision,
) -> (Vec<RowFacts>, bool) {
    let keys: Vec<UnitKey> = claims.iter().flatten().map(Claim::key).collect();
    let mut versions = runtime.unit_versions(&keys).into_iter();
    let mut inconsistent = false;
    let rows = claims
        .iter()
        .map(|claim| {
            let Some(claim) = claim else {
                return (None, RowState::Display);
            };
            match versions.next().flatten() {
                Some(version) => (
                    Some(UnitWire {
                        view: view_key(claim.view),
                        field: claim.field.0,
                        kind: crate::wire::unit_kind(claim.kind),
                        entity: claim.entity.as_ref().map(|key| key.key_display()),
                        changed_at: Some(version),
                    }),
                    if version == revision {
                        RowState::Changed
                    } else {
                        RowState::Clean
                    },
                ),
                None => {
                    inconsistent = true;
                    (None, RowState::Removed)
                }
            }
        })
        .collect();
    (rows, inconsistent)
}

/// A field render plus whether any row's unit claim was rejected.
#[derive(Debug)]
pub struct ProjectedField {
    /// Wire field render.
    pub wire: FieldRenderWire,
    /// Whether a claim could not be backed by the runtime.
    pub inconsistent: bool,
}

/// Projects one field render into the wire shape.
///
/// A whole-value field never keeps per-element units, even if the renderer
/// supplied them: the runtime has no identity to attach.
#[must_use]
pub fn field_render_wire(
    runtime: &Runtime,
    facts: &FieldFacts,
    render: FieldRender,
    revision: Revision,
    caps: Caps,
) -> ProjectedField {
    let FieldRender {
        field,
        model,
        total,
        truncated,
    } = render;
    // A whole-value field has no per-element identity, so a renderer's unit
    // claims are simply not applicable: drop them silently rather than calling
    // the render inconsistent. A claim on a per-entity field that the runtime
    // cannot back is a real inconsistency and is reported.
    let allow_units = facts.granularity == Granularity::PerEntity;

    let claim = |unit: &Option<crate::render::UnitRef>| -> Option<Claim> {
        if allow_units {
            unit.as_ref().map(Claim::of)
        } else {
            None
        }
    };

    let (model, allowed) = match model {
        RenderModel::Tree(rows) => {
            let rows: Vec<TreeRow> = rows.into_iter().take(caps.tree_rows).collect();
            let claims: Vec<Option<Claim>> = rows.iter().map(|row| claim(&row.unit)).collect();
            let (resolved, inconsistent) = resolve(runtime, &claims, revision);
            let rows = rows
                .into_iter()
                .zip(resolved)
                .map(|(row, (unit, state))| TreeRowWire {
                    key: row.key,
                    depth: row.depth,
                    kind: row.kind,
                    label: row.label,
                    value: row.value,
                    unit,
                    state,
                })
                .collect();
            (ModelWire::Tree { rows }, Some(inconsistent))
        }
        RenderModel::List(rows) => {
            let rows: Vec<ListRow> = rows.into_iter().take(caps.rows).collect();
            let claims: Vec<Option<Claim>> = rows.iter().map(|row| claim(&row.unit)).collect();
            let (resolved, inconsistent) = resolve(runtime, &claims, revision);
            let rows = rows
                .into_iter()
                .zip(resolved)
                .map(|(row, (unit, state))| ListRowWire {
                    key: format!("i{}", row.index),
                    index: row.index,
                    value: row.value,
                    unit,
                    state,
                })
                .collect();
            (ModelWire::List { rows }, Some(inconsistent))
        }
        RenderModel::Map(rows) => {
            let rows: Vec<MapRow> = rows.into_iter().take(caps.rows).collect();
            let claims: Vec<Option<Claim>> = rows.iter().map(|row| claim(&row.unit)).collect();
            let (resolved, inconsistent) = resolve(runtime, &claims, revision);
            let rows = rows
                .into_iter()
                .zip(resolved)
                .map(|(row, (unit, state))| MapRowWire {
                    key: row.key.clone(),
                    label: row.key,
                    value: row.value,
                    unit,
                    state,
                })
                .collect();
            (ModelWire::Map { rows }, Some(inconsistent))
        }
        RenderModel::Set(members) => {
            let members: Vec<SetMember> = members.into_iter().take(caps.set_members).collect();
            let claims: Vec<Option<Claim>> = members.iter().map(|row| claim(&row.unit)).collect();
            let (resolved, inconsistent) = resolve(runtime, &claims, revision);
            let members = members
                .into_iter()
                .zip(resolved)
                .map(|(row, (unit, state))| SetMemberWire {
                    key: row.member.clone(),
                    member: row.member,
                    present: row.present,
                    unit,
                    state,
                })
                .collect();
            (ModelWire::Set { members }, Some(inconsistent))
        }
        RenderModel::Scalar(rows) => {
            let rows: Vec<ScalarRow> = rows.into_iter().take(caps.rows).collect();
            let claims: Vec<Option<Claim>> = rows.iter().map(|row| claim(&row.unit)).collect();
            let (resolved, inconsistent) = resolve(runtime, &claims, revision);
            let rows = rows
                .into_iter()
                .zip(resolved)
                .map(|(row, (unit, state))| ScalarRowWire {
                    key: row.name.clone(),
                    label: row.name,
                    value: row.value,
                    unit,
                    state,
                })
                .collect();
            (ModelWire::Scalar { rows }, Some(inconsistent))
        }
        RenderModel::Graph { nodes, edges } => {
            let nodes: Vec<GraphNode> = nodes.into_iter().take(caps.graph_nodes).collect();
            let claims: Vec<Option<Claim>> = nodes.iter().map(|row| claim(&row.unit)).collect();
            let (resolved, inconsistent) = resolve(runtime, &claims, revision);
            let kept: HashSet<String> = nodes.iter().map(|node| node.id.clone()).collect();
            let nodes: Vec<GraphNodeWire> = nodes
                .into_iter()
                .zip(resolved)
                .map(|(node, (unit, state))| GraphNodeWire {
                    id: node.id,
                    label: node.label,
                    unit,
                    state,
                })
                .collect();
            let edges: Vec<GraphEdgeWire> = edges
                .into_iter()
                .filter(|edge: &GraphEdge| kept.contains(&edge.from) && kept.contains(&edge.to))
                .map(|edge| GraphEdgeWire {
                    from: edge.from,
                    to: edge.to,
                    label: edge.label,
                })
                .collect();
            (ModelWire::Graph { nodes, edges }, Some(inconsistent))
        }
        RenderModel::Reduced {
            aggregate,
            reduction,
            sample,
        } => (
            ModelWire::Reduced {
                aggregate,
                reduction,
                sample: sample.into_iter().take(caps.rows.min(50)).collect(),
            },
            None,
        ),
        RenderModel::Opaque { kind, summary } => (
            ModelWire::Opaque {
                type_name: kind,
                summary,
            },
            None,
        ),
    };

    let rendered = match &model {
        ModelWire::Tree { rows } => rows.len(),
        ModelWire::List { rows } => rows.len(),
        ModelWire::Map { rows } => rows.len(),
        ModelWire::Set { members } => members.len(),
        ModelWire::Graph { nodes, .. } => nodes.len(),
        ModelWire::Scalar { rows } => rows.len(),
        ModelWire::Reduced { sample, .. } => sample.len(),
        ModelWire::Opaque { .. } => 0,
    };

    ProjectedField {
        wire: FieldRenderWire {
            field: field.0,
            model,
            total: total.max(rendered),
            truncated: truncated || total > rendered,
        },
        inconsistent: allowed == Some(true),
    }
}

/// Renders a View and projects it, or reports why it could not.
#[must_use]
pub fn view_render_wire(
    runtime: &Runtime,
    facts: &ViewFacts,
    renderers: &Renderers,
    caps: Caps,
) -> Option<ViewRenderWire> {
    // A host renderer draws with the host's own knowledge; without one the
    // runtime's facts still describe the View, so the plate is drawn from those
    // and says so.
    let (rendered, source) = match renderers.render(facts.view, &runtime.inspect_snapshot()) {
        Some(rendered) => (rendered, "host_renderer"),
        None => (
            Ok(crate::generic::render(runtime, facts.view, facts, caps)),
            "generic",
        ),
    };
    let revision = runtime.revision();
    let fields = match rendered {
        Ok(fields) => fields,
        Err(error) => {
            return Some(ViewRenderWire {
                view: view_key(facts.view),
                content_version: facts.content_version,
                revision,
                source: "render_failed",
                inconsistent: false,
                fields: vec![FieldRenderWire {
                    field: 0,
                    model: ModelWire::Opaque {
                        type_name: String::from("render error"),
                        summary: error.0,
                    },
                    total: 0,
                    truncated: false,
                }],
            });
        }
    };
    let mut inconsistent = false;
    let mut wire = Vec::with_capacity(fields.len());
    for render in fields {
        let Some(field_facts) = facts
            .fields
            .iter()
            .find(|field| field.field == render.field)
        else {
            continue;
        };
        let projected = field_render_wire(runtime, field_facts, render, revision, caps);
        inconsistent |= projected.inconsistent;
        wire.push(projected.wire);
    }
    Some(ViewRenderWire {
        view: view_key(facts.view),
        content_version: facts.content_version,
        revision,
        source,
        inconsistent,
        fields: wire,
    })
}

/// Projects one node's detail, with a formatted value when a formatter exists.
#[must_use]
pub fn node_detail(
    runtime: &Runtime,
    node: reactive_core::ids::NodeId,
    renderers: &Renderers,
) -> Option<NodeDetailWire> {
    let explanation = runtime.try_explain_node(node)?;
    let revision = runtime.revision();
    let mut detail = node_detail_wire(&explanation, revision);
    let facts = runtime
        .probe(&reactive_runtime::observe::GraphQuery {
            scope: reactive_runtime::observe::GraphScope::Node(node),
            include_sites: true,
            node_limit: 1,
            edge_limit: 0,
        })
        .nodes
        .into_iter()
        .next()?;
    detail.node = node_wire(&facts, revision);
    if let Some(unit) = &facts.unit {
        let snapshot = runtime.inspect_snapshot();
        detail.value = Some(match renderers.format(unit.view, &snapshot) {
            Some(preview) => ValueWire {
                kind: preview.kind,
                summary: preview.summary,
                detail: preview.detail,
                bytes: preview.bytes,
                truncated: preview.truncated,
                source: "host_formatter",
            },
            None => ValueWire {
                kind: format!("unit {}", crate::wire::unit_kind(unit.kind)),
                summary: format!("opaque value, {} B", facts.value_size.unwrap_or_default()),
                detail: None,
                bytes: facts.value_size.unwrap_or_default(),
                truncated: false,
                source: "opaque",
            },
        });
    }
    Some(detail)
}

#[cfg(test)]
mod tests {
    use crate::render::{
        FieldRender, GraphEdge, GraphNode, ListRow, MapRow, RenderModel, ScalarRow, SetMember,
        TreeRow,
    };

    fn rows(count: usize) -> Vec<ListRow> {
        (0..count)
            .map(|index| ListRow {
                index,
                value: index.to_string(),
                unit: None,
            })
            .collect()
    }

    /// G13: each field kind has its own drawing, and the tag is what the page
    /// switches on. A shared "rows" tag would let the wrong renderer draw.
    #[test]
    fn every_model_kind_keeps_its_own_tag() {
        let models = [
            RenderModel::Tree(vec![TreeRow {
                key: String::from("n0"),
                depth: 0,
                kind: String::from("Program"),
                label: String::from("program"),
                value: None,
                unit: None,
            }]),
            RenderModel::List(rows(2)),
            RenderModel::Map(vec![MapRow {
                key: String::from("a"),
                value: Some(String::from("1")),
                unit: None,
            }]),
            RenderModel::Set(vec![SetMember {
                member: String::from("m"),
                present: true,
                unit: None,
            }]),
            RenderModel::Scalar(vec![ScalarRow {
                name: String::from("s"),
                value: String::from("1"),
                unit: None,
            }]),
            RenderModel::Graph {
                nodes: vec![GraphNode {
                    id: String::from("0"),
                    label: String::from("zero"),
                    unit: None,
                }],
                edges: vec![GraphEdge {
                    from: String::from("0"),
                    to: String::from("0"),
                    label: None,
                }],
            },
            RenderModel::Reduced {
                aggregate: String::from("3"),
                reduction: String::from("sum"),
                sample: vec![String::from("1")],
            },
            RenderModel::Opaque {
                kind: String::from("Widget"),
                summary: String::from("12 B"),
            },
        ];
        let tags: Vec<&str> = models
            .iter()
            .map(|model| match model {
                RenderModel::Tree(_) => "tree",
                RenderModel::List(_) => "list",
                RenderModel::Map(_) => "map",
                RenderModel::Set(_) => "set",
                RenderModel::Scalar(_) => "scalar",
                RenderModel::Graph { .. } => "graph",
                RenderModel::Reduced { .. } => "reduced",
                RenderModel::Opaque { .. } => "opaque",
            })
            .collect();
        assert_eq!(
            tags,
            vec![
                "tree", "list", "map", "set", "scalar", "graph", "reduced", "opaque"
            ]
        );
    }

    /// G4: a render that exceeds its cap says so, and keeps the exact total.
    #[test]
    fn capped_renders_report_the_real_total() {
        let over = FieldRender::capped(
            reactive_core::ids::FieldId(0),
            RenderModel::List(rows(10)),
            10,
        )
        .with_cap(3);
        assert_eq!(over.total, 10);
        assert!(over.truncated);
        assert_eq!(over.model.len(), 3);
        let small = FieldRender::capped(
            reactive_core::ids::FieldId(0),
            RenderModel::List(rows(2)),
            2,
        )
        .with_cap(3);
        assert!(!small.truncated);
        assert_eq!(small.model.len(), 2);
    }
}
