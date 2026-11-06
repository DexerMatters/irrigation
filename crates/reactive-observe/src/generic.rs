//! Drawing a View from the runtime's own facts.
//!
//! A host that registers no renderer still gets a plate with rows: the runtime
//! knows each field's shape, its entities in their own order, and the revision
//! every unit moved at, and each View descriptor can spell its element values
//! and name a tree node's variant (`ContentVtable::describe`,
//! `ContentVtable::variant_label`).
//!
//! This module turns those facts into the same [`FieldRender`] a host renderer
//! would produce, so the rest of the pipeline — claims, change marks, caps,
//! truncation — is unchanged. Two rules keep it honest:
//!
//! * a row cites a unit only when that unit really exists, so nothing is ever
//!   reported as removed or inconsistent;
//! * a value is read through the content vtable, so a field published as one
//!   value (a derived list, a whole-value View) still draws its elements.
//!
//! What a generic drawing does not have is a host's naming: a `JsonPair` is
//! `Rule`, not `Pair`. That is the refinement `renderer::<V>` still buys.

use std::collections::HashSet;
use std::sync::Arc;

use reactive_core::descriptor::{FieldKind, UnitKey, UnitKind, ViewDescriptor};
use reactive_core::ids::{FieldId, ViewId};
use reactive_core::key::{AnyKey, any_key};
use reactive_runtime::Runtime;
use reactive_runtime::observe::{EntityFacts, FieldFacts, ViewFacts};
use reactive_store::PSeq;

use crate::project::Caps;
use crate::render::{
    FieldRender, GraphNode, ListRow, MapRow, RenderModel, ScalarRow, SetMember, TreeRow, UnitRef,
};

/// The generic drawing of every field of one View.
#[must_use]
pub fn render(runtime: &Runtime, view: ViewId, facts: &ViewFacts, caps: Caps) -> Vec<FieldRender> {
    let Some(descriptor) = runtime.descriptor(view) else {
        return Vec::new();
    };
    let content = runtime.raw_content(view);
    let installed = installed(view, runtime);
    facts
        .fields
        .iter()
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
            let total = model.len();
            let truncated = trim(&mut model, caps);
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
                    unit: self.claim(&entity.unit),
                }
            })
            .collect();
        RenderModel::Map(rows)
    }

    fn graph(&self, facts: &FieldFacts, entities: &[EntityFacts], caps: Caps) -> RenderModel {
        // A graph's topology lives in a separate structure, so the generic
        // drawing shows the node set it can name and leaves edges to a renderer.
        let mut nodes: Vec<GraphNode> = Vec::new();
        for entity in entities.iter().take(caps.graph_nodes) {
            let Some(key) = entity.unit.entity.as_ref() else {
                continue;
            };
            let id = key.key_display();
            if nodes.iter().any(|node| node.id == id) {
                continue;
            }
            let unit = self.value_unit(facts.field, key);
            nodes.push(GraphNode {
                label: self.spell(&unit).unwrap_or_else(|| id.clone()),
                id,
                unit: self.claim(&entity.unit),
            });
        }
        RenderModel::Graph {
            nodes,
            edges: Vec::new(),
        }
    }

    fn tree(&self, caps: Caps) -> RenderModel {
        use reactive_collections::tree::{TreeStorage, TreeValue};

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
                    .first()
                    .and_then(|value| match value {
                        TreeValue::Text(text) => Some(text.clone()),
                        TreeValue::Int(value) => Some(value.to_string()),
                        TreeValue::UInt(value) => Some(value.to_string()),
                        TreeValue::Bool(value) => Some(value.to_string()),
                        TreeValue::Float(value) => Some(value.to_string()),
                        _ => None,
                    })
                    .unwrap_or_default(),
                value: None,
                unit: self.claim(&reactive_collections::tree::units::tag(
                    self.descriptor.id,
                    node,
                )),
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
        RenderModel::Opaque { .. } => {}
    }
    true
}
