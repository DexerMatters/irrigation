//! Drawing a View from the runtime's own facts.
//!
//! This is the only renderer. The runtime knows each field's shape, its
//! entities in their own order, and the revision every unit moved at; the View
//! descriptor spells element values and names tree variants
//! (`ContentVtable::describe`, `ContentVtable::variant_label`,
//! `ContentVtable::summary`).
//!
//! The module turns those facts into [`FieldRender`] values, so the rest of the
//! pipeline — claims, change marks, caps, truncation — is unchanged. Two rules
//! keep it honest:
//!
//! * a row cites a unit only when that unit really exists, so nothing is ever
//!   reported as removed or inconsistent;
//! * a value is read through the content vtable, so a field published as one
//!   value (a derived list, a whole-value View, a reduction) still draws what
//!   it holds.
//!
//! Naming comes from the type's own `Debug`: an AST rule carries its name in
//! the tree, and a scope-graph node carries its label in the graph payload.

use std::collections::HashSet;
use std::sync::Arc;

use irrigation_core::descriptor::{FieldKind, UnitKey, UnitKind, ViewDescriptor};
use irrigation_core::ids::{FieldId, ViewId};
use irrigation_core::key::{AnyKey, any_key};
use irrigation_runtime::Runtime;
use irrigation_runtime::observe::{EntityFacts, FieldFacts, ViewFacts};
use irrigation_store::PSeq;

use crate::project::Caps;
use crate::render::{
    FieldRender, GraphEdge, GraphNode, ListRow, MapRow, RenderModel, ScalarRow, SetMember, TreeRow,
    UnitRef,
};

/// The generic drawing of every field of one View.
#[must_use]
pub fn render(runtime: &Runtime, view: ViewId, facts: &ViewFacts, caps: Caps) -> Vec<FieldRender> {
    let Some(descriptor) = runtime.descriptor(view) else {
        return Vec::new();
    };
    let content = runtime.raw_content(view);
    let installed = installed(view, runtime);
    // A tree View's declared slots below the root (tag, field, children,
    // parent, membership) are observation units, not rows a reader needs as
    // separate tabs. The tree drawing is the View.
    let tree_only = facts
        .fields
        .first()
        .is_some_and(|field| field.kind == FieldKind::Tree);
    facts
        .fields
        .iter()
        .filter(move |field| !tree_only || field.field.0 == 0)
        .map(|field| {
            let entities = runtime.field_entities(descriptor.id, field.field);
            let context = Context {
                descriptor,
                content: content.as_ref(),
                installed: &installed,
            };
            let mut model = match field.kind {
                FieldKind::Scalar => context.scalar(field, &entities),
                FieldKind::List => context.list(field, &entities, caps),
                FieldKind::Set => context.set(&entities, caps),
                FieldKind::Map => context.map(field, &entities, caps),
                FieldKind::Graph => context.graph(field, &entities, caps),
                FieldKind::Tree => context.tree(caps),
            };
            // A family with a whole-field summary (a reduction's aggregate)
            // shows that summary instead of pretending its items are the whole
            // story. The value is spelled through the same `describe` hook.
            if field.kind == FieldKind::List
                && let Some(aggregate) = context.summary(field.field)
            {
                let sample = match &model {
                    RenderModel::List(rows) => rows
                        .iter()
                        .take(caps.rows.min(8))
                        .map(|row| row.value.clone())
                        .collect(),
                    _ => Vec::new(),
                };
                model = RenderModel::Reduced {
                    aggregate,
                    reduction: field.name.to_owned(),
                    sample,
                };
            }
            // The field's element count is the pre-cap truth; the model may
            // already have stopped at its cap while it was built. A reduction's
            // aggregate is complete even when its sample is empty.
            let reduced = matches!(model, RenderModel::Reduced { .. });
            let total = if reduced {
                model.len()
            } else {
                field.elements.unwrap_or_default().max(model.len())
            };
            let truncated = !reduced && (trim(&mut model, caps) || total > model.len());
            if reduced {
                let _ = trim(&mut model, caps);
            }
            FieldRender {
                field: field.field,
                model,
                total,
                truncated,
            }
        })
        .collect()
}

/// Unit tokens that exist on this View, so a claim is only made for a real one.
fn installed(view: ViewId, runtime: &Runtime) -> HashSet<String> {
    runtime
        .view_units(view)
        .into_iter()
        .map(|(unit, _)| token(&unit))
        .collect()
}

fn token(unit: &UnitKey) -> String {
    format!(
        "{}:{}:{}",
        unit.field.0,
        unit.kind as u8,
        unit.entity
            .as_ref()
            .map_or_else(String::new, |entity| entity.key_display())
    )
}

struct Context<'a> {
    descriptor: &'static ViewDescriptor,
    content: Option<&'a Arc<dyn std::any::Any + Send + Sync>>,
    installed: &'a HashSet<String>,
}

impl Context<'_> {
    /// A claim, but only when the runtime really has that unit.
    fn claim(&self, unit: &UnitKey) -> Option<UnitRef> {
        self.installed
            .contains(&token(unit))
            .then(|| UnitRef::from_key(unit))
    }

    /// Reads one unit's value out of the content and spells it.
    fn spell(&self, unit: &UnitKey) -> Option<String> {
        let descriptor = self.descriptor;
        let content = self.content?;
        let value = (descriptor.content.get)(content, unit)?;
        (descriptor.content.describe)(&value)
    }

    /// Spells a field's whole-field summary, when its family publishes one.
    fn summary(&self, field: FieldId) -> Option<String> {
        let content = self.content?;
        let value = (self.descriptor.content.summary)(content, field)?;
        (self.descriptor.content.describe)(&value)
    }

    /// Reads a graph field's topology, the one structure the runtime keeps.
    fn topology(&self, field: FieldId) -> Option<irrigation_collections::GraphTopology> {
        let content = self.content?;
        let structure = (self.descriptor.content.structure)(content, field)?;
        structure
            .value
            .downcast_ref::<irrigation_collections::GraphTopology>()
            .cloned()
    }

    fn value_unit(&self, field: FieldId, entity: &Arc<dyn AnyKey>) -> UnitKey {
        UnitKey::entity(
            self.descriptor.id,
            field,
            UnitKind::Value,
            Arc::clone(entity),
        )
    }

    fn scalar(&self, facts: &FieldFacts, entities: &[EntityFacts]) -> RenderModel {
        let unit = UnitKey::field(self.descriptor.id, facts.field, UnitKind::Value);
        let rows = if entities.is_empty() {
            // A whole-value field still names itself; the content is one value.
            vec![ScalarRow {
                name: facts.name.to_owned(),
                value: self.spell(&unit).unwrap_or_default(),
                unit: self.claim(&unit),
            }]
        } else {
            entities
                .iter()
                .map(|entity| ScalarRow {
                    name: facts.name.to_owned(),
                    value: self.spell(&entity.unit).unwrap_or_default(),
                    unit: self.claim(&entity.unit),
                })
                .collect()
        };
        RenderModel::Scalar(rows)
    }

    fn list(&self, facts: &FieldFacts, entities: &[EntityFacts], caps: Caps) -> RenderModel {
        let sequence = self.sequence(facts.field);
        let rows = if entities.is_empty() {
            // Published as one value: the ids come from the field's structure.
            sequence
                .iter()
                .enumerate()
                .take(caps.rows)
                .map(|(index, id)| {
                    let unit = UnitKey::entity(
                        self.descriptor.id,
                        facts.field,
                        UnitKind::Value,
                        any_key(*id),
                    );
                    ListRow {
                        index,
                        value: self.spell(&unit).unwrap_or_default(),
                        unit: self.claim(&unit),
                    }
                })
                .collect()
        } else {
            entities
                .iter()
                .take(caps.rows)
                .enumerate()
                .map(|(index, entity)| {
                    let unit = entity.unit.entity.as_ref().map_or_else(
                        || UnitKey::field(self.descriptor.id, facts.field, UnitKind::Value),
                        |entity| self.value_unit(facts.field, entity),
                    );
                    ListRow {
                        index,
                        value: self.spell(&unit).unwrap_or_default(),
                        unit: self.claim(&unit),
                    }
                })
                .collect()
        };
        RenderModel::List(rows)
    }

    fn set(&self, entities: &[EntityFacts], caps: Caps) -> RenderModel {
        let members = entities
            .iter()
            .take(caps.set_members)
            .map(|entity| SetMember {
                member: entity
                    .unit
                    .entity
                    .as_ref()
                    .map_or_else(String::new, |key| key.key_display()),
                present: true,
                unit: self.claim(&entity.unit),
            })
            .collect();
        RenderModel::Set(members)
    }

    fn map(&self, facts: &FieldFacts, entities: &[EntityFacts], caps: Caps) -> RenderModel {
        let rows = entities
            .iter()
            .take(caps.rows)
            .enumerate()
            .map(|(index, entity)| {
                let key = entity
                    .unit
                    .entity
                    .clone()
                    .unwrap_or_else(|| any_key(index as u64));
                let unit = self.value_unit(facts.field, &key);
                MapRow {
                    key: key.key_display(),
                    value: self.spell(&unit),
                    // A map row shows the value at the key. Membership says the
                    // key exists; the value unit says what is there, so an edit
                    // to one entry marks that entry and leaves the others alone.
                    unit: self.claim(&unit),
                }
            })
            .collect();
        RenderModel::Map(rows)
    }

    fn graph(&self, facts: &FieldFacts, entities: &[EntityFacts], caps: Caps) -> RenderModel {
        // The structure unit holds the topology. Use it to keep node entities
        // (the field's table also carries edge membership) and to wire the
        // edges, so a generic drawing looks like a graph and not a key list.
        let topology = self.topology(facts.field);
        let node_ids: HashSet<u64> = topology
            .as_ref()
            .map(|topology| topology.nodes.iter().copied().collect())
            .unwrap_or_default();
        let mut nodes: Vec<GraphNode> = Vec::new();
        let mut ids: HashSet<String> = HashSet::new();
        for entity in entities {
            let Some(key) = entity.unit.entity.as_ref() else {
                continue;
            };
            let Some(node) = key
                .as_any()
                .downcast_ref::<irrigation_collections::NodeKey>()
            else {
                continue;
            };
            if topology.is_some() && !node_ids.contains(&node.0) {
                continue;
            }
            if nodes.len() >= caps.graph_nodes {
                break;
            }
            let id = node.0.to_string();
            if !ids.insert(id.clone()) {
                continue;
            }
            let unit = self.value_unit(facts.field, key);
            nodes.push(GraphNode {
                label: self.spell(&unit).unwrap_or_else(|| id.clone()),
                id,
                // A node row shows the payload; a weight edit must mark the
                // node, not its membership cell.
                unit: self.claim(&unit),
            });
        }
        let edges = topology
            .map(|topology| {
                topology
                    .edges
                    .iter()
                    .filter(|(_, (from, to))| {
                        ids.contains(&from.to_string()) && ids.contains(&to.to_string())
                    })
                    .map(|(edge, (from, to))| GraphEdge {
                        from: from.to_string(),
                        to: to.to_string(),
                        // The edge payload is the host's own label; spell it
                        // through the View's describe hook like any other value.
                        label: self.spell(&UnitKey::entity(
                            self.descriptor.id,
                            facts.field,
                            irrigation_core::descriptor::UnitKind::Value,
                            any_key(irrigation_collections::EdgeKey(*edge)),
                        )),
                    })
                    .collect()
            })
            .unwrap_or_default();
        RenderModel::Graph { nodes, edges }
    }

    fn tree(&self, caps: Caps) -> RenderModel {
        use irrigation_collections::tree::{TreeStorage, TreeValue};

        let Some(content) = self
            .content
            .and_then(|content| content.downcast_ref::<TreeStorage>())
        else {
            return RenderModel::Tree(Vec::new());
        };
        let mut rows = Vec::new();
        let mut queue: Vec<(_, u16)> = content.root.into_iter().map(|root| (root, 0)).collect();
        let mut cursor = 0;
        while cursor < queue.len() && rows.len() < caps.tree_rows {
            let (node, depth) = queue[cursor];
            cursor += 1;
            let Some(entry) = content.node(node) else {
                continue;
            };
            // The row draws the first leaf payload when the node has one, so
            // that payload's unit is the row's evidence. A structural node with
            // no payload draws its kind, and the tag unit is that evidence.
            let unit = if entry.fields.is_empty() {
                irrigation_collections::tree::units::tag(self.descriptor.id, node)
            } else {
                // The label prefers a text payload; cite that slot so a lexeme
                // edit marks the row, and fall back to the identity slot.
                let slot = entry
                    .fields
                    .iter()
                    .position(|value| matches!(value, TreeValue::Text(_)))
                    .unwrap_or(0);
                irrigation_collections::tree::units::field(
                    self.descriptor.id,
                    node,
                    u16::try_from(slot).unwrap_or(0),
                )
            };
            rows.push(TreeRow {
                key: format!("{node:?}"),
                depth,
                kind: (self.descriptor.content.variant_label)(
                    u32::from(entry.type_index),
                    entry.variant,
                )
                .unwrap_or("node")
                .to_owned(),
                label: entry
                    .fields
                    .iter()
                    // A node that carries its own spelling draws that, not the
                    // numeric identity beside it.
                    .find_map(|value| match value {
                        TreeValue::Text(text) => Some(text.clone()),
                        _ => None,
                    })
                    .or_else(|| {
                        entry.fields.first().and_then(|value| match value {
                            TreeValue::Int(value) => Some(value.to_string()),
                            TreeValue::UInt(value) => Some(value.to_string()),
                            TreeValue::Bool(value) => Some(value.to_string()),
                            TreeValue::Float(value) => Some(value.to_string()),
                            _ => None,
                        })
                    })
                    .unwrap_or_default(),
                value: None,
                unit: self.claim(&unit),
            });
            for child in &entry.children {
                for child in child.ids() {
                    queue.push((child, depth + 1));
                }
            }
        }
        RenderModel::Tree(rows)
    }

    /// A list field's ids, from the structure the content vtable projects.
    fn sequence(&self, field: FieldId) -> Vec<u64> {
        let Some(content) = self.content else {
            return Vec::new();
        };
        (self.descriptor.content.structure)(content, field)
            .and_then(|structure| {
                structure
                    .value
                    .downcast_ref::<PSeq<u64>>()
                    .map(|sequence| sequence.iter().copied().collect())
            })
            .unwrap_or_default()
    }
}

/// Applies the cap that belongs to the model, reporting whether it cut anything.
fn trim(model: &mut RenderModel, caps: Caps) -> bool {
    let limit = match model {
        RenderModel::Tree(_) => caps.tree_rows,
        RenderModel::Set(_) => caps.set_members,
        RenderModel::Graph { .. } => caps.graph_nodes,
        _ => caps.rows,
    };
    if model.len() <= limit {
        return false;
    }
    match model {
        RenderModel::Tree(rows) => rows.truncate(limit),
        RenderModel::List(rows) => rows.truncate(limit),
        RenderModel::Map(rows) => rows.truncate(limit),
        RenderModel::Set(members) => members.truncate(limit),
        RenderModel::Graph { nodes, .. } => nodes.truncate(limit),
        RenderModel::Scalar(rows) => rows.truncate(limit),
        RenderModel::Reduced { sample, .. } => sample.truncate(limit),
    }
    true
}
