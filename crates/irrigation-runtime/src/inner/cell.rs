//! The cell protocol: one logical unit's published value.
//!
//! A cell is a payload, the version that names it, and the revision it changed
//! at. Four writers move that triple — install, retire, open a generation, fill
//! one — and every reader here answers from it. Keeping the protocol in one
//! module is what makes "which writer moved what" a question with a file-sized
//! answer instead of a search through the kernel.

use super::*;

impl Inner {
    /// Returns (creating on first use) a unit's node, with no initializer.
    ///
    /// A caller that brings no projection cannot re-derive a stale cell, so this
    /// is the vacant-cell form of [`Self::unit_node_init`].
    pub(super) fn cell_node(&self, view: ViewId, key: Arc<dyn AnyKey>) -> NodeId {
        let mut state = self.lock();
        let typed = Self::typed_key(view, key);
        Self::ensure_unit_node(&mut state, typed, None)
    }

    /// Returns the unit key a caller addressed, or the View's field-0 value.
    pub(super) fn typed_key(view: ViewId, key: Arc<dyn AnyKey>) -> UnitKey {
        match key.as_any().downcast_ref::<UnitKey>() {
            Some(unit) => unit.clone(),
            None => UnitKey::field(view, irrigation_core::ids::FieldId(0), UnitKind::Value),
        }
    }

    /// Returns the cell node of one unit, creating it vacant on first use.
    pub(crate) fn ensure_unit_node(
        state: &mut State,
        key: UnitKey,
        initial: Option<Value>,
    ) -> NodeId {
        if let Some(node) = state.units.get(&key) {
            return *node;
        }
        // A cell is born at the revision the committed content gave its unit,
        // whether or not the projection is available yet: the payload has been
        // the content's answer since the content changed, and a subscription
        // seeded from the same projection must not read the creation as a
        // change of its own. A unit the content cannot spell is born at the
        // zero `unit_version` answered before the cell existed, and keeps that
        // version until an accessor's projection or a definition moves it.
        // That zero is the version the one derivation already answers for a cell
        // that does not exist yet.
        let changed_at = crate::state::unit_version(state, &key);
        let mut record = NodeRecord::unit("unit", key.clone());
        record.changed_at = changed_at;
        if let Some(value) = initial {
            let value_version = state.alloc_value_version();
            record.install_value(Some(value), value_version, changed_at);
        }
        let node = state.alloc(record);
        state.units.insert(Arc::new(key.clone()), node);
        state.record_cell(&key, node);
        node
    }

    /// Whether a cell has never held an answer.
    ///
    /// A cell created without a projection — a generic read, a receipt, a unit
    /// no provider has defined yet — holds none, and the accessor that owns the
    /// projection is the one that can fill it. A retracted cell holds none
    /// either, but it has a generation of its own: its reader observes the
    /// retraction rather than a re-derived payload.
    pub(super) fn cell_is_unfilled(state: &State, node: NodeId) -> bool {
        let record = state.node(node);
        record.value.is_none() && record.value_version == 0
    }

    /// Whether a materialized cell must be re-derived from its View's content.
    ///
    /// Only a cell a whole-region replacement left for the next read qualifies:
    /// a provider-owned cell is the provider's to publish, and a retracted cell
    /// must stay uncertified, because its reader observes the retraction rather
    /// than a re-derived payload.
    pub(super) fn cell_is_stale(state: &State, unit: &UnitKey, node: NodeId) -> bool {
        state.node(node).content_stale && !state.unit_owned_by_live_provider(unit)
    }

    /// Re-derives one cell a whole-region replacement left for the next read.
    ///
    /// Returns whether the cell is current afterwards. A cell the content
    /// vtable spells is republished from the committed content. A cell only the
    /// accessor that materialized it can project — a rank or adjacency index, a
    /// whole-field payload — stays stale for that accessor: its pre-replacement
    /// payload is not this revision's answer, so the caller must not serve it.
    pub(crate) fn refresh_cell(state: &mut State, unit: &UnitKey, node: NodeId) -> bool {
        if !state.node(node).content_stale {
            return true;
        }
        if state.unit_owned_by_live_provider(unit) || !Self::content_spells(state, unit) {
            return false;
        }
        let initial = state
            .regions
            .get(&unit.view)
            .and_then(|region| content_projection(region, unit));
        Self::republish_cell(state, node, initial);
        true
    }

    /// Whether the content vtable spells one cell.
    ///
    /// A cell is spelled when the vtable answers that unit for that field: a
    /// field that holds one value (a scalar, or a tree's entity-less root
    /// cell), one entity's payload or presence, or one entity's child slot. A
    /// whole-field payload or an index the accessor computes belongs to the
    /// accessor that materialized it.
    pub(super) fn content_spells(state: &State, unit: &UnitKey) -> bool {
        let Some(region) = state.regions.get(&unit.view) else {
            return false;
        };
        let Some(field) = region
            .descriptor
            .fields
            .get(usize::from(unit.field.0))
            .map(|field| field.kind)
        else {
            return false;
        };
        match unit.kind {
            // One entity's payload or presence is a projection of the content.
            UnitKind::Value | UnitKind::Membership if unit.entity.is_some() => true,
            // A field that holds one value projects that value: a scalar
            // field's cell, and a tree's entity-less root cell.
            UnitKind::Value => matches!(field, FieldKind::Scalar | FieldKind::Tree),
            // A structure cell is the field's projection when it has no
            // entity, and a family's child slot when it has one; the vtable
            // spells the child slot, and the field-level projection is
            // refreshed with the content instead of here.
            UnitKind::Structure => unit.entity.is_some(),
            // An index the accessor computes from order, a protocol
            // certificate, and the content unit itself are not projections.
            UnitKind::Membership
            | UnitKind::Rank
            | UnitKind::Adjacency
            | UnitKind::IncomingAdjacency
            | UnitKind::Content => false,
        }
    }

    /// The revision the committed content already answers one draft's value at.
    ///
    /// The content is the cutoff authority on the definition path exactly as it
    /// is on the edit path: a definition that restates what the content projects
    /// is the cell's birth, not a generation of its own. The value has been the
    /// content's answer since the content changed, so a subscription seeded from
    /// that content must not read the definition as a change. `None` means the
    /// content does not hold the draft, and installing it moves the unit.
    pub(super) fn content_answer(
        state: &State,
        region: &Region,
        unit: &UnitKey,
        value: &Value,
        eq: fn(&Value, &Value) -> bool,
    ) -> Option<Revision> {
        let holds = if unit.kind == UnitKind::Membership {
            let present = (region.descriptor.content.contains)(&region.content, unit);
            eq(&value_of(present), value)
        } else {
            (region.descriptor.content.get)(&region.content, unit)
                .is_some_and(|current| eq(&current, value))
        };
        holds.then(|| state.node(region.content_node).changed_at)
    }

    /// Writes a re-derived cell's payload into the generation it belongs to.
    ///
    /// The replacement that opened this generation already allocated its version
    /// and stamped the revision it changed at, and a read that can now project
    /// the payload is installing that change rather than making one. The stamp
    /// is therefore kept, and only the payload observer's identity moves: a
    /// reader that recorded the unfilled generation recorded the absence, so it
    /// belongs to a version this fill retires.
    pub(super) fn republish_cell(state: &mut State, node: NodeId, initial: Option<Value>) {
        let verified_at = state.revision;
        let changed_at = state.node(node).changed_at;
        let value_version = state.alloc_value_version();
        let dependents = state.node(node).dependents.clone();
        state
            .node_mut(node)
            .publish_fill(initial, value_version, changed_at, verified_at);
        // The payload is new to the graph even though its stamp is not: a
        // dependent that observed the unfilled cell, or the generation it
        // replaced, holds a result this payload contradicts, and a cell it
        // already certified for this revision would not revisit. Mark the
        // readers instead, so the next demand re-executes them.
        if !dependents.is_empty() {
            invalidate(state, dependents, verified_at);
        }
    }

    /// Writes one unit's value as a committed change at `revision`.
    pub(super) fn commit_unit_value(
        state: &mut State,
        node: NodeId,
        value: Value,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        let value_version = state.alloc_value_version();
        let record = state.node_mut(node);
        record.install_value(Some(value), value_version, revision);
        record.routing_at = revision;
        changed_nodes.push(node);
    }

    /// Retires one cell's payload as a change of its own generation.
    ///
    /// Retraction is the inverse of installation, so the payload, the version,
    /// and the stamp move together like any other change. The cell is left
    /// uncertified on purpose — a retracted unit is not a value the runtime
    /// serves, and an observation of it must fail validation instead of pinning
    /// an absence.
    ///
    /// The cell itself is the seed, never its readers: the pass this feeds has
    /// to see those readers as the results it is about, and exempting them
    /// would let a demand carry out a value this retraction contradicts.
    pub(super) fn retire_cell(
        state: &mut State,
        node: NodeId,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        let value_version = state.alloc_value_version();
        let validity = state.alloc_validity();
        // `routing_generation` "advances when the unit's provider changes,
        // including a handover that leaves the value equal". A source View has no
        // provider, so no handover there is possible and the stamp must not move:
        // moving it told a routing observer that the provider had changed when
        // only the value had, and left the entity's payload and membership cells
        // disagreeing about whether anything had.
        let routes = state
            .node(node)
            .unit
            .as_ref()
            .is_none_or(|unit| state.producers.contains_key(&unit.view));
        let record = state.node_mut(node);
        record.retire_value(value_version, revision);
        if routes {
            record.routing_at = revision;
        }
        record.verified_at = 0;
        record.verified_validity = 0;
        record.suspect_at = revision;
        record.suspect_validity = validity;
        changed_nodes.push(node);
    }

    /// Restores one unit's committed value and its membership cell.
    ///
    /// Conflict resolution has to undo a later provider's install without
    /// running the definition pipeline; the unit node and the entity's
    /// membership both belong to the restored value.
    pub(super) fn restore_definition(
        state: &mut State,
        region: &mut Region,
        unit: &UnitKey,
        value: Value,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        if unit.entity.is_some() {
            let membership = membership_key(unit);
            let present = if unit.kind == UnitKind::Membership {
                value.downcast_ref::<bool>().copied().unwrap_or(true)
            } else {
                true
            };
            if let Some(slot) = region.units.get(&membership) {
                let node = slot.node;
                Self::commit_unit_value(state, node, value_of(present), revision, changed_nodes);
            }
        }
        if unit.kind != UnitKind::Membership
            && let Some(slot) = region.units.get(unit)
        {
            let node = slot.node;
            Self::commit_unit_value(state, node, value, revision, changed_nodes);
        }
    }
}
