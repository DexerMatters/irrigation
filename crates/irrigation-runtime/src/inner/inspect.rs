//! Read-only inspection for the observation path.
//!
//! Every method here holds the metadata lock for one pass, reads committed
//! state, and never demands a node: opening the board cannot manufacture the
//! work the board shows. They are gated on the `observe` feature.

use super::*;

/// Adds a row for every entity the content order names that no cell backs.
///
/// The board draws one row per entity and spells each row's value through the content
/// projection, so an entity the content defines must be listed even when nothing has
/// materialised a cell for it; `unit_version_of` -- the single owner -- answers its version
/// from the content revision. Rows came from materialized cells alone, so such an entity was
/// given a position in this order and then dropped.
///
/// Only fields whose order decodes reach here, which is currently `PSeq<u64>`: a **list**. A
/// set, map or graph spells its keys as typed values and `Structure` carries only a type-erased
/// value, so those need a vtable capability rather than a decode.
#[cfg(feature = "observe")]
fn fill_entities_without_cells(
    state: &State,
    view: irrigation_core::ids::ViewId,
    field: irrigation_core::ids::FieldId,
    declared: Option<&irrigation_core::descriptor::FieldDescriptor>,
    order: &[u64],
    revision: Revision,
    rows: &mut Vec<crate::observe::EntityFacts>,
) {
    if declared.is_some_and(|field| field.kind == irrigation_core::descriptor::FieldKind::Scalar) {
        return;
    }
    let seen: std::collections::HashSet<u64> = rows
        .iter()
        .filter_map(|row| {
            row.unit
                .entity
                .as_ref()
                .and_then(|entity| entity.as_any().downcast_ref::<u64>().copied())
        })
        .collect();
    for id in order {
        if seen.contains(id) {
            continue;
        }
        let unit = UnitKey::entity(
            view,
            field,
            UnitKind::Membership,
            irrigation_core::key::any_key(*id),
        );
        let version = crate::state::unit_version_of(state, &unit).unwrap_or(0);
        rows.push(crate::observe::EntityFacts {
            unit,
            version,
            changed: version == revision,
        });
    }
}

impl Inner {
    /// Every component the runtime knows, with the Views it reads and defines.
    ///
    /// Registration is what puts a component here: the root, every installed
    /// general component, and every component that has been invoked. Ownership
    /// is the scope's: a record names its component directly when it belongs to
    /// an invocation, and every node a run creates — its expression sites, the
    /// definitions installed on them — is attributed through the scope that
    /// holds it.
    ///
    /// Reads resolve the same way. A trace names producers, and a producer is
    /// usually an expression site rather than a unit, so the walk descends
    /// through nodes it does not own and collects the units it reaches. When it
    /// reaches another component's body the walk stops: what this component
    /// reads from a callee is the View the callee defines, not everything the
    /// callee happened to look at.
    ///
    /// Timestamps come from the records that run — a component's body and its
    /// output projections — never from the units and sites in its scope, whose
    /// own timestamps belong to whoever published them.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn component_facts(&self) -> Vec<crate::observe::ComponentFacts> {
        use crate::observe::{ComponentFacts, ComponentKind};
        use irrigation_core::ids::ComponentId;
        use std::collections::HashMap;

        let state = self.lock();
        let mut components: HashMap<ComponentId, ComponentFacts> = HashMap::new();
        let mut entry = |id: ComponentId, name: &'static str, kind: ComponentKind| {
            components.entry(id).or_insert(ComponentFacts {
                id,
                name,
                kind,
                reads: Vec::new(),
                writes: Vec::new(),
                executed_at: 0,
                suspect_at: 0,
            });
        };
        if let Some(root) = &state.root {
            entry(root.component, root.name, ComponentKind::Root);
        }
        for general in &state.generals {
            entry(general.id(), general.name(), ComponentKind::General);
        }
        for (id, name) in &state.component_names {
            entry(*id, name, ComponentKind::Invoked);
        }

        // Who owns each node: the record says so for an invocation's body and
        // projections, and the scope says so for everything the run created.
        let mut owner: Vec<Option<ComponentId>> = Vec::with_capacity(state.nodes.len());
        for record in state.nodes.iter() {
            let id = if record.freed {
                None
            } else {
                record.component.or_else(|| {
                    record
                        .general
                        .and_then(|index| state.generals.get(index))
                        .map(|general| general.id())
                        .or_else(|| component_of(&state, record.scope))
                })
            };
            owner.push(id);
        }

        let root_body = state.root.as_ref().map(|root| root.body);
        let root_outputs: &[NodeId] = state
            .root
            .as_ref()
            .map_or(&[], |root| root.outputs.as_slice());
        let runs = |node: NodeId, record: &crate::state::NodeRecord| -> bool {
            record.component.is_some()
                || Some(node) == root_body
                || Some(node) == state.general_root
                || root_outputs.contains(&node)
        };

        // Pass one: what each component defines, and when it last ran.
        for (index, record) in state.nodes.iter().enumerate() {
            if record.freed {
                continue;
            }
            let Some(id) = owner[index] else {
                continue;
            };
            let Some(facts) = components.get_mut(&id) else {
                continue;
            };
            // A unit holds a value and a region content node holds the host's
            // content; neither is a component's definition.
            if !matches!(record.kind, NodeKind::Unit | NodeKind::RegionContent) {
                for view in record
                    .definitions
                    .iter()
                    .map(|definition| definition.unit.view)
                    .chain(record.content_writes.iter().copied())
                {
                    if !facts.writes.contains(&view) {
                        facts.writes.push(view);
                    }
                }
            }
            let node = NodeId::new(index as u32, record.generation);
            if runs(node, record) {
                facts.executed_at = facts.executed_at.max(record.executed_at);
                facts.suspect_at = facts.suspect_at.max(record.suspect_at);
            }
        }

        // Pass two: what each component reads. The walk needs the writes above,
        // because a callee boundary stands for the Views the callee defines.
        let mut seen = vec![0u32; state.nodes.len()];
        let mut stamp = 0u32;
        let mut reached: Vec<ViewId> = Vec::new();
        for (index, record) in state.nodes.iter().enumerate() {
            if record.freed {
                continue;
            }
            let Some(id) = owner[index] else {
                continue;
            };
            if record.trace.is_empty() {
                continue;
            }
            stamp = stamp.wrapping_add(1);
            reached.clear();
            let mut stack: Vec<NodeId> = record.trace.iter().map(|dep| dep.node).collect();
            while let Some(dep) = stack.pop() {
                let slot = dep.index();
                if slot >= seen.len() || seen[slot] == stamp {
                    continue;
                }
                seen[slot] = stamp;
                let other = state.node(dep);
                if other.freed {
                    continue;
                }
                if let Some(unit) = &other.unit {
                    reached.push(unit.view);
                    continue;
                }
                // Crossing into a callee is a boundary, not a read. What this
                // component reads from another one is the value it returns — the
                // output projection — so the Views that callee defines are what
                // the edge stands for. Entering through a callee's *body* is a
                // transitive dependency the board does not draw: it would give
                // the root an edge from every stage below it.
                let callee = other
                    .component
                    .or_else(|| component_of(&state, other.scope));
                match callee {
                    Some(callee) if callee != id => {
                        if matches!(other.kind, NodeKind::InvocationOutput)
                            && let Some(facts) = components.get(&callee)
                        {
                            reached.extend(facts.writes.iter().copied());
                        }
                    }
                    _ => stack.extend(other.trace.iter().map(|dep| dep.node)),
                }
            }
            let Some(facts) = components.get_mut(&id) else {
                continue;
            };
            for view in reached.drain(..) {
                // A component that publishes a View does not read it back.
                if !facts.writes.contains(&view) && !facts.reads.contains(&view) {
                    facts.reads.push(view);
                }
            }
        }

        let mut facts: Vec<ComponentFacts> = components.into_values().collect();
        for component in &mut facts {
            component.reads.sort_unstable_by_key(|view| view.0);
            component.writes.sort_unstable_by_key(|view| view.0);
        }
        facts.sort_by_key(|component| (component.name, component.id.0));
        facts
    }
    /// Returns several units' change versions under one lock.
    ///
    /// A unit installed by a host edit lives in the global table, one installed
    /// by a provider lives in the region, and one the content defines has no
    /// cell yet; all three are units the runtime serves, so all three answer.
    /// The version is the one owner's — `state::unit_version` — so a host that
    /// seeds its watermark from here agrees with the runtime's own
    /// subscriptions.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn unit_versions(&self, units: &[UnitKey]) -> Vec<Option<Version>> {
        let state = self.lock();
        units
            .iter()
            .map(|unit| crate::state::unit_version_of(&state, unit))
            .collect()
    }
    /// One field's entities in the View's own order, for a generic drawing.
    ///
    /// A list follows its sequence, a set and a map follow their key table
    /// (sorted by the key's display text, so the board is stable across
    /// revisions), and a scalar field answers with its single value unit. A tree
    /// field answers empty: its nodes live in slot units, which a tree-aware
    /// reader resolves from the structure unit rather than from this call.
    ///
    /// **Coverage limit, stated so a consumer is not misled:** the rows come from
    /// materialized cells, plus -- for a **list** -- the identities the committed content
    /// spells without a cell. A set, map or graph whose entities the content spells before
    /// any cell exists lists none of them, because enumerating a typed key table needs a
    /// `ContentVtable` capability that does not exist yet. A whole-field `define_all` on a
    /// list is the only shape known to exercise the content path today.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn field_entities(
        &self,
        view: irrigation_core::ids::ViewId,
        field: irrigation_core::ids::FieldId,
    ) -> Vec<crate::observe::EntityFacts> {
        use irrigation_core::descriptor::UnitKind;
        let state = self.lock();
        let revision = state.revision;
        let Some(region) = state.regions.get(&view) else {
            return Vec::new();
        };
        let declared = region.descriptor.fields.get(field.0 as usize);
        if declared.is_none() {
            return Vec::new();
        }
        // The structure projection already knows the field's order.
        let order: Option<Vec<u64>> = (region.descriptor.content.structure)(&region.content, field)
            .and_then(|structure| {
                structure
                    .value
                    .downcast_ref::<irrigation_store::PSeq<u64>>()
                    .map(|sequence| sequence.iter().copied().collect())
            });
        let mut rows: Vec<crate::observe::EntityFacts> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut push = |unit: irrigation_core::descriptor::UnitKey, node: NodeId| {
            let key = unit
                .entity
                .as_ref()
                .map(|entity| entity.key_display())
                .unwrap_or_default();
            let token = format!("{}:{}", unit.kind as u8, key);
            if !seen.insert(token) {
                return;
            }
            rows.push(crate::observe::EntityFacts {
                unit,
                version: state.node(node).changed_at,
                changed: state.node(node).changed_at == revision,
            });
        };
        let edited = state
            .units
            .iter()
            .filter_map(|(key, node)| key.as_any().downcast_ref::<UnitKey>().map(|u| (u, *node)));
        let owned = region.units.iter().filter_map(|(key, slot)| {
            key.as_any()
                .downcast_ref::<UnitKey>()
                .map(|u| (u, slot.node))
        });
        for (unit, node) in edited.chain(owned) {
            if unit.view != view || unit.field != field {
                continue;
            }
            // The identity unit is what a row stands for; the value unit is
            // fetched by the projection when it needs the text. Only a unit with
            // an entity is an identity: a field's protocol certificate is a
            // membership unit without one.
            let wanted = match declared.map(|field| field.kind) {
                Some(irrigation_core::descriptor::FieldKind::Scalar) => {
                    unit.kind == UnitKind::Value
                }
                _ => unit.kind == UnitKind::Membership && unit.entity.is_some(),
            };
            if wanted {
                // A scalar field has one unit and no entity; a collection's
                // identity is per entity.
                push(unit.clone(), node);
            }
        }
        if let Some(order) = &order {
            fill_entities_without_cells(&state, view, field, declared, order, revision, &mut rows);
        }
        if let Some(order) = order {
            // A list answers in sequence order: the ids are the entities.
            rows.sort_by_key(|row| {
                let id = row
                    .unit
                    .entity
                    .as_ref()
                    .and_then(|entity| entity.as_any().downcast_ref::<u64>().copied());
                id.and_then(|id| order.iter().position(|existing| *existing == id))
                    .unwrap_or(usize::MAX)
            });
        } else if declared
            .is_some_and(|field| field.kind != irrigation_core::descriptor::FieldKind::Scalar)
        {
            rows.sort_by_key(|row| {
                row.unit
                    .entity
                    .as_ref()
                    .map(|entity| entity.key_display())
                    .unwrap_or_default()
            });
        }
        rows
    }
    /// Every installed unit of one View, with the revision it last changed at.
    ///
    /// A generic drawing reads identities and versions from here — including a
    /// tree's slot units, which are not attached to a declared field.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn view_units(
        &self,
        view: irrigation_core::ids::ViewId,
    ) -> Vec<(irrigation_core::descriptor::UnitKey, Version)> {
        use irrigation_core::descriptor::UnitKind;
        let state = self.lock();
        let Some(region) = state.regions.get(&view) else {
            return Vec::new();
        };
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut rows: Vec<(UnitKey, Version)> = Vec::new();
        let edited = state
            .units
            .iter()
            .filter_map(|(key, node)| key.as_any().downcast_ref::<UnitKey>().map(|u| (u, *node)));
        let owned = region.units.iter().filter_map(|(key, slot)| {
            key.as_any()
                .downcast_ref::<UnitKey>()
                .map(|u| (u, slot.node))
        });
        for (unit, node) in edited.chain(owned) {
            if unit.view != view
                || unit.kind != UnitKind::Value && unit.kind != UnitKind::Membership
            {
                continue;
            }
            let token = format!(
                "{}:{}:{}",
                unit.field.0,
                unit.kind as u8,
                unit.entity
                    .as_ref()
                    .map(|entity| entity.key_display())
                    .unwrap_or_default()
            );
            if !seen.insert(token) {
                continue;
            }
            rows.push((unit.clone(), state.node(node).changed_at));
        }
        rows
    }
    /// A View's descriptor, for a projection that draws it generically.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn descriptor(
        &self,
        view: irrigation_core::ids::ViewId,
    ) -> Option<&'static ViewDescriptor> {
        self.lock()
            .regions
            .get(&view)
            .map(|region| region.descriptor)
    }
    /// A View's content, untyped.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn raw_content(
        &self,
        view: irrigation_core::ids::ViewId,
    ) -> Option<irrigation_core::value::Value> {
        self.lock()
            .regions
            .get(&view)
            .map(|region| Arc::clone(&region.content))
    }
    /// A unit's type-erased value, for a display hook with no typed snapshot.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn unit_value(
        &self,
        unit: &irrigation_core::descriptor::UnitKey,
    ) -> Option<irrigation_core::value::Value> {
        let state = self.lock();
        let region = state.regions.get(&unit.view)?;
        let node = region.units.get(unit)?.node;
        state.node(node).value.clone()
    }
    /// Projects every registered View's metadata at one revision.
    ///
    /// Values are never cloned; element counts come from the content vtable.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn view_facts(&self) -> Vec<crate::observe::ViewFacts> {
        let state = self.lock();
        let revision = state.revision;
        // One pass over the edited-unit table, grouped by the View and field it
        // names. The per-field projection used to walk every unit in the runtime
        // once per field, which made a keystroke cost a scan per plate.
        let mut edited: FxHashMap<(ViewId, FieldId), Vec<NodeId>> = FxHashMap::default();
        for (key, node) in state.units.iter() {
            let Some(unit) = key.as_any().downcast_ref::<UnitKey>() else {
                continue;
            };
            edited
                .entry((unit.view, unit.field))
                .or_default()
                .push(*node);
        }
        let mut views: Vec<&crate::state::Region> = state.regions.values().collect();
        views.sort_by_key(|region| region.descriptor.id.0);
        views
            .into_iter()
            .map(|region| {
                let descriptor = region.descriptor;
                let content_version = state.node(region.content_node).changed_at;
                let mut changed_units = 0;
                let fields = descriptor
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(index, field)| {
                        let id = irrigation_core::ids::FieldId(index as u16);
                        // Edited units live in the global table, provided ones
                        // in the region; both count toward the field.
                        let mut units = 0;
                        let mut changed = 0;
                        let mut seen: std::collections::HashSet<irrigation_core::ids::NodeId> =
                            std::collections::HashSet::new();
                        let edited_nodes = edited
                            .get(&(descriptor.id, id))
                            .map_or(&[][..], |nodes| nodes.as_slice());
                        let provided = region.units.iter().filter_map(|(key, slot)| {
                            key.as_any()
                                .downcast_ref::<UnitKey>()
                                .filter(|unit| unit.field == id)
                                .map(|_| slot.node)
                        });
                        // `units` is the number of *entities* a field holds, not
                        // the number of unit rows (an entity has several).
                        let mut entities: std::collections::HashSet<String> =
                            std::collections::HashSet::new();
                        for node in edited_nodes.iter().copied().chain(provided) {
                            if !seen.insert(node) {
                                continue;
                            }
                            let record = &state.nodes[node.index()];
                            match record.unit.as_ref() {
                                Some(unit) if unit.entity.is_some() => {
                                    let key = unit
                                        .entity
                                        .as_ref()
                                        .map(|entity| entity.key_display())
                                        .unwrap_or_default();
                                    entities.insert(key);
                                }
                                Some(unit)
                                    if unit.kind
                                        == irrigation_core::descriptor::UnitKind::Value =>
                                {
                                    units += 1;
                                }
                                _ => {}
                            }
                            if record.changed_at == revision {
                                changed += 1;
                            }
                        }
                        if !entities.is_empty() {
                            units = entities.len();
                        }
                        changed_units += changed;
                        crate::observe::FieldFacts {
                            field: id,
                            name: field.name,
                            kind: field.kind,
                            granularity: granularity_of(&state, descriptor, id),
                            units,
                            elements: (descriptor.content.len)(&region.content, id),
                            content_version,
                            changed_units: changed,
                        }
                    })
                    .collect();
                // Every distinct component that owns a unit here. A shared
                // View can have several; `producer` stays the first owner for
                // the existing plate/ink, but the page and the JSON must be
                // able to name all of them.
                let contributors = {
                    let mut ids: Vec<ComponentId> = region
                        .owners
                        .iter()
                        .filter_map(|(_, provider)| {
                            let record = state.node(provider.node);
                            record
                                .component
                                .or_else(|| component_of(&state, record.scope))
                        })
                        .collect();
                    if let Some(node) = state.producers.get(&descriptor.id).copied() {
                        let record = state.node(node);
                        if let Some(id) = record
                            .component
                            .or_else(|| component_of(&state, record.scope))
                        {
                            ids.push(id);
                        }
                    }
                    ids.sort_unstable();
                    ids.dedup();
                    ids
                };
                // `producer` and `provider_node` describe one owner, chosen
                // deterministically: the smallest contributing component id, which
                // is `contributors.first()`.
                let producer_component = contributors.first().copied();
                let provider_node = region
                    .owners
                    .iter()
                    .find_map(|(_, provider)| {
                        let record = state.node(provider.node);
                        let component = record
                            .component
                            .or_else(|| component_of(&state, record.scope));
                        (component == producer_component).then_some(provider.node)
                    })
                    .or_else(|| state.producers.get(&descriptor.id).copied());
                crate::observe::ViewFacts {
                    view: descriptor.id,
                    name: descriptor.name,
                    kind: descriptor.kind,
                    fields,
                    // The producing *component*, not the node: the page names
                    // the component beside the View. An installed definition
                    // names its provider; a registered producer (a general
                    // component, the root) is the fallback.
                    // The first *contributor*, not the first entry of a hash table:
                    // a shared View has several owners and `iter().next()` gave
                    // whichever the hash order happened to yield, so the same board
                    // could name a different producer on consecutive calls.
                    // `contributors` is sorted, so its head is stable and this is
                    // consistent with it.
                    producer: producer_component,
                    contributors,
                    routing: region.routing_version,
                    content_version,
                    demanded: descriptor.kind == irrigation_core::descriptor::ViewKind::Source
                        || !region.owners.is_empty(),
                    content_node: region.content_node,
                    provider_node,
                    suspect: {
                        // A definition's dependents are invalidated, not the
                        // unit node itself. The provider of any installed unit
                        // stands in for "this View's data may be stale".
                        let owner = region
                            .owners
                            .iter()
                            .next()
                            .map_or(region.content_node, |(_, provider)| provider.node);
                        let record = state.node(owner);
                        record.suspect_at > record.verified_at
                    },
                    changed_units,
                }
            })
            .collect()
    }
    /// Projects the node arena with a cap.
    ///
    /// Holds the metadata lock for one pass and never clones a value.
    #[cfg(feature = "observe")]
    #[must_use]
    pub fn probe(&self, query: &crate::observe::GraphQuery) -> crate::observe::GraphFacts {
        use crate::observe::{GraphFacts, NodeFacts, UnitFacts};
        let state = self.lock();
        let revision = state.revision;
        let mut nodes = Vec::new();
        let mut matched = 0usize;
        let mut truncated = false;
        for (index, record) in state.nodes.iter().enumerate() {
            if record.freed {
                continue;
            }
            let id = NodeId::new(index as u32, record.generation);
            if !scope_matches(&state, query.scope, id, record) {
                continue;
            }
            if matches!(record.kind, NodeKind::Site) && !query.include_sites {
                continue;
            }
            matched += 1;
            if nodes.len() >= query.node_limit {
                truncated = true;
                continue;
            }
            nodes.push(NodeFacts {
                id,
                kind: kind_name(record.kind),
                label: record.label,
                scope: record.scope,
                component: record.component,
                unit: record.unit.as_ref().map(|unit| UnitFacts {
                    view: unit.view,
                    field: unit.field,
                    kind: unit.kind,
                    entity: unit.entity.as_ref().map(|entity| entity.key_display()),
                }),
                changed_at: record.changed_at,
                verified_at: record.verified_at,
                suspect_at: record.suspect_at,
                executed_at: record.executed_at,
                validated_at: record.validated_at,
                routing_at: record.routing_at,
                waiters: record.waiters.len() as u32,
                value_size: record
                    .value
                    .as_ref()
                    .map(|value| std::mem::size_of_val(value.as_ref())),
            });
        }
        let included: std::collections::HashSet<NodeId> =
            nodes.iter().map(|node| node.id).collect();
        let mut edges = Vec::new();
        'outer: for node in &nodes {
            let record = &state.nodes[node.id.index()];
            for dep in &record.trace {
                if !included.contains(&dep.node) {
                    continue;
                }
                if edges.len() >= query.edge_limit {
                    break 'outer;
                }
                edges.push(crate::observe::EdgeFacts {
                    from: node.id,
                    to: dep.node,
                    recorded: dep.version,
                    // `dep.version` is the node's value version, so the current
                    // side must be one too. `changed_at` is a *revision* -- a
                    // different counter, advanced by every commit -- so the two
                    // were never comparable, and the drift label this feeds was
                    // noise. `explain` answers the same question with
                    // `value_version`.
                    current: state.nodes[dep.node.index()].value_version,
                });
            }
        }
        GraphFacts {
            revision,
            nodes,
            edges,
            total_nodes: matched,
            truncated,
        }
    }
}

/// Element granularity for one field, derived from the region it lives in.
///
/// The field kind alone is not enough: a source tree stages one whole content
/// value, while a derived tree can stage a unit per node. Only the installed
/// units can answer honestly.
#[cfg(feature = "observe")]
fn granularity_of(
    state: &State,
    descriptor: &'static ViewDescriptor,
    field: irrigation_core::ids::FieldId,
) -> crate::observe::Granularity {
    use crate::observe::Granularity;
    let region = match state.regions.get(&descriptor.id) {
        Some(region) => region,
        None => return Granularity::Unknown,
    };
    let mut entities = 0usize;
    let mut units = 0usize;
    // An edit stages a unit in the global table, while a provider stages it in
    // the region; both are installed units and both answer the question.
    let edited = state
        .units
        .iter()
        .filter_map(|(key, _)| key.as_any().downcast_ref::<UnitKey>())
        .filter(|unit| unit.view == descriptor.id && unit.field == field);
    let owned = region
        .units
        .iter()
        .filter_map(|(key, _)| key.as_any().downcast_ref::<UnitKey>())
        .filter(|unit| unit.field == field);
    for unit in edited.chain(owned) {
        units += 1;
        if unit.entity.is_some() {
            entities += 1;
        }
    }
    if entities > 0 {
        return Granularity::PerEntity;
    }
    // A tree keeps its node units in the tag/field/children slots, not in the
    // root field. If the region has entity units at all, rows can be marked.
    if descriptor
        .fields
        .get(field.0 as usize)
        .is_some_and(|field| field.kind == irrigation_core::descriptor::FieldKind::Tree)
    {
        let tree_entities = region
            .units
            .iter()
            .filter_map(|(key, _)| key.as_any().downcast_ref::<UnitKey>())
            .filter(|unit| unit.entity.is_some())
            .count();
        if tree_entities > 0 {
            return Granularity::PerEntity;
        }
    }
    match descriptor
        .fields
        .get(field.0 as usize)
        .map(|field| field.kind)
    {
        Some(irrigation_core::descriptor::FieldKind::Scalar) if units > 0 => Granularity::PerEntity,
        Some(
            irrigation_core::descriptor::FieldKind::List
            | irrigation_core::descriptor::FieldKind::Set
            | irrigation_core::descriptor::FieldKind::Map
            | irrigation_core::descriptor::FieldKind::Graph
            | irrigation_core::descriptor::FieldKind::Tree,
        ) => Granularity::WholeValue,
        Some(_) => Granularity::Unknown,
        None => Granularity::Unknown,
    }
}

/// Whether a node belongs to the queried scope.
#[cfg(feature = "observe")]
fn scope_matches(
    state: &State,
    scope: crate::observe::GraphScope,
    id: NodeId,
    record: &NodeRecord,
) -> bool {
    use crate::observe::GraphScope;
    match scope {
        GraphScope::All => true,
        GraphScope::View(view) => {
            record.unit.as_ref().is_some_and(|unit| unit.view == view)
                || matches!(record.kind, NodeKind::RegionContent)
                    && state
                        .regions
                        .get(&view)
                        .is_some_and(|region| region.content_node == id)
        }
        GraphScope::Component(component) => {
            record.component == Some(component)
                || component_of(state, record.scope) == Some(component)
        }
        GraphScope::Node(node) => node == id,
    }
}

/// Stable kind names for the wire.
#[cfg(feature = "observe")]
fn kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Site => "site",
        NodeKind::Unit => "unit",
        NodeKind::InvocationBody => "body",
        NodeKind::InvocationOutput => "output",
        NodeKind::Operator => "operator",
        NodeKind::RegionContent => "content",
    }
}

#[cfg(all(test, feature = "observe"))]
mod entity_row_tests {
    use super::*;

    fn row_ids(rows: &[crate::observe::EntityFacts]) -> Vec<u64> {
        let mut ids: Vec<u64> = rows
            .iter()
            .filter_map(|row| {
                row.unit
                    .entity
                    .as_ref()
                    .and_then(|entity| entity.as_any().downcast_ref::<u64>().copied())
            })
            .collect();
        ids.sort_unstable();
        ids
    }

    /// An entity the content order names but no cell backs still becomes a row.
    ///
    /// This is the branch the board needs: it spells each row's value through the content, so the
    /// row list must not be limited to cells that happen to exist. The function is unit-tested here
    /// because no integration fixture produces the shape -- a seeded list creates a cell per item,
    /// and a seeded map cannot be ordered.
    #[test]
    fn content_named_entities_become_rows() {
        let state = State::default();
        let declared = irrigation_core::descriptor::FieldDescriptor {
            name: "items",
            kind: irrigation_core::descriptor::FieldKind::List,
        };
        let mut rows = Vec::new();
        fill_entities_without_cells(
            &state,
            irrigation_core::ids::ViewId(7),
            irrigation_core::ids::FieldId(0),
            Some(&declared),
            &[3, 1, 2],
            5,
            &mut rows,
        );
        assert_eq!(row_ids(&rows), vec![1, 2, 3], "every named id is a row");
        assert!(
            rows.iter().all(|row| row.version == 0),
            "an entity with no cell takes the content fallback, which is zero on an empty state"
        );
    }

    /// A scalar field has one unit and no per-entity rows, so the order must not add any.
    #[test]
    fn a_scalar_field_adds_no_entity_rows() {
        let state = State::default();
        let declared = irrigation_core::descriptor::FieldDescriptor {
            name: "value",
            kind: irrigation_core::descriptor::FieldKind::Scalar,
        };
        let mut rows = Vec::new();
        fill_entities_without_cells(
            &state,
            irrigation_core::ids::ViewId(7),
            irrigation_core::ids::FieldId(0),
            Some(&declared),
            &[1, 2],
            5,
            &mut rows,
        );
        assert!(rows.is_empty(), "a scalar field has no entities");
    }
}
