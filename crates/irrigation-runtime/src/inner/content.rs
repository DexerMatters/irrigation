//! The content protocol: one View's committed value and its projections.
//!
//! Part of the kernel's `impl Inner`; the struct and its lock live in the
//! parent module.

use super::*;

impl Inner {
    #[allow(clippy::too_many_arguments)]
    /// Recomputes one field's structure unit from the committed content.
    pub(super) fn refresh_structure(
        state: &mut State,
        region: &mut Region,
        field: FieldId,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        state
            .stats
            .structure_refreshes
            .fetch_add(1, Ordering::Relaxed);
        let descriptor = region.descriptor;
        let Some(structure) = (descriptor.content.structure)(&region.content, field) else {
            return;
        };
        let key = UnitKey::field(descriptor.id, field, UnitKind::Structure);
        let node = match region.units.get(&key) {
            Some(slot) => slot.node,
            None => {
                let node = Self::ensure_unit_node(state, key.clone(), None);
                region.units.insert(any_key(key.clone()), UnitSlot { node });
                node
            }
        };
        let changed = {
            let record = state.node(node);
            record
                .value
                .as_ref()
                .is_none_or(|old| !(structure.eq)(old, &structure.value))
        };
        let value_version = changed.then(|| state.alloc_value_version());
        let record = state.node_mut(node);
        if changed {
            record.install_value(
                Some(structure.value),
                value_version.expect("changed"),
                revision,
            );
            changed_nodes.push(node);
        } else {
            record.certify(revision);
            record.suspect_at = 0;
            record.state = NodeState::Ready;
            record.failure = None;
        }
    }

    /// Publishes one View's content, advancing the content unit's version.
    ///
    /// The definition pipeline — install, retraction, conflict restore —
    /// computes the content field by field and refreshes the fields it moved,
    /// so it takes this primitive. A whole-region replacement takes
    /// [`Self::publish_region_content`] instead.
    ///
    /// Returns whether the content moved: a caller that derives cells from it
    /// (the retraction that refreshes a field's structure) checks the answer
    /// instead of re-deriving the cutoff.
    pub(super) fn commit_content(
        state: &mut State,
        region: &mut Region,
        content: Arc<dyn Any + Send + Sync>,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) -> bool {
        let eq = region.descriptor.content.eq;
        Self::commit_content_with_eq(state, region, content, eq, revision, changed_nodes)
    }

    /// [`commit_content`] with the caller's equality for the cutoff.
    ///
    /// The host content path stages the transaction's own `eq`, so it passes it
    /// through instead of re-deciding the cutoff with the descriptor's.
    pub(super) fn commit_content_with_eq(
        state: &mut State,
        region: &mut Region,
        content: Arc<dyn Any + Send + Sync>,
        eq: fn(&Value, &Value) -> bool,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) -> bool {
        let changed = state
            .node(region.content_node)
            .value
            .as_ref()
            .is_none_or(|old| !eq(old, &content));
        if changed {
            state.stats.content_commits.fetch_add(1, Ordering::Relaxed);
            let value_version = state.alloc_value_version();
            let record = state.node_mut(region.content_node);
            record.install_value(Some(content.clone()), value_version, revision);
            changed_nodes.push(region.content_node);
        }
        region.content = content;
        changed
    }

    /// Publishes content already known to have moved.
    ///
    /// The definition pipeline applies a unit only when that unit's value moved
    /// and the field says the unit moves content. In that case the rebuilt
    /// content is a new value by construction; comparing it with the previous
    /// whole content would repeat the work the unit comparison already did.
    pub(super) fn commit_content_changed(
        state: &mut State,
        region: &mut Region,
        content: Arc<dyn Any + Send + Sync>,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        state.stats.content_commits.fetch_add(1, Ordering::Relaxed);
        let value_version = state.alloc_value_version();
        let record = state.node_mut(region.content_node);
        record.install_value(Some(content.clone()), value_version, revision);
        changed_nodes.push(region.content_node);
        region.content = content;
    }

    /// Publishes a whole-region content replacement and the structure it derives.
    ///
    /// A host `set_content` and a general component's are the same transition:
    /// both replace every field's content at once. A structure unit is computed
    /// from content but its readers observed the unit, not the content, so
    /// leaving it to lazily re-initialize would keep serving the old projection
    /// for good.
    ///
    /// Only materialized units are refreshed: a unit nobody read has nothing to
    /// correct, and a provider-owned structure is the provider's to publish.
    /// Returns whether the View is registered, so the caller can take the raw
    /// path for a namespace that has no region yet.
    pub(crate) fn publish_region_content(
        state: &mut State,
        view: ViewId,
        content: Arc<dyn Any + Send + Sync>,
        eq: fn(&Value, &Value) -> bool,
        revision: Revision,
        delta: CellDelta<'_>,
        changed_nodes: &mut Vec<NodeId>,
    ) -> bool {
        Self::with_taken_region(state, view, |state, region| {
            let descriptor = region.descriptor;
            let normalization_safe = descriptor.fields.iter().all(|field| {
                matches!(
                    field.kind,
                    irrigation_core::descriptor::FieldKind::Scalar
                        | irrigation_core::descriptor::FieldKind::Map
                        | irrigation_core::descriptor::FieldKind::List
                        | irrigation_core::descriptor::FieldKind::Set
                )
            });
            // One changed content-moving unit is a proof that this transaction
            // moved that unit. With several units, an add/remove pair can end at
            // the committed content, so the equality remains the normalization
            // point. Graph/tree/reduced families keep equality: their topology
            // and edit scripts can cancel across units.
            let units: Vec<UnitKey> = delta.installed.iter().cloned().collect();
            let unit_equality =
                (descriptor.content.changed_units_equal)(&region.content, &content, &units);
            let content_moved = match unit_equality {
                Some(true) => false,
                Some(false) => true,
                None => {
                    normalization_safe
                        && units.len() == 1
                        && units
                            .iter()
                            .any(|unit| (descriptor.content.moves_content)(unit))
                }
            };
            let changed = if content_moved {
                state
                    .stats
                    .source_content_applies
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Self::commit_content_changed(state, region, content, revision, changed_nodes);
                true
            } else if matches!(unit_equality, Some(true)) {
                // The changed units project equal content; no whole equality
                // is needed either.
                false
            } else {
                state
                    .stats
                    .source_equality_checks
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Self::commit_content_with_eq(state, region, content, eq, revision, changed_nodes)
            };
            if changed {
                if delta.installed.is_empty() {
                    Self::refresh_structures(state, region, revision, changed_nodes);
                } else {
                    let fields: Vec<FieldId> = delta
                        .installed
                        .iter()
                        .filter(|unit| unit.kind != UnitKind::Value || unit.entity.is_none())
                        .map(|unit| unit.field)
                        .collect();
                    if !fields.is_empty() {
                        Self::refresh_structures_of(
                            state,
                            region,
                            Some(&fields),
                            revision,
                            changed_nodes,
                        );
                    }
                }
            }
            if changed {
                Self::invalidate_cells(state, region, delta, revision, changed_nodes);
            }
        })
        .is_some()
    }

    /// Republishes every materialized structure projection from the region's
    /// committed content.
    ///
    /// A structure unit is derived from content but its readers observed the
    /// unit, not the content, so a content replacement that skipped this would
    /// leave them serving a projection the content no longer supports. Only
    /// materialized units are refreshed — a unit nobody read initializes from
    /// the new content — and a provider-owned structure is the provider's to
    /// publish.
    pub(super) fn refresh_structures(
        state: &mut State,
        region: &mut Region,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        Self::refresh_structures_of(state, region, None, revision, changed_nodes);
    }

    pub(super) fn refresh_structures_of(
        state: &mut State,
        region: &mut Region,
        fields: Option<&[FieldId]>,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        let descriptor = region.descriptor;
        let indices: Vec<usize> = match fields {
            Some(fields) => {
                let mut indices: Vec<usize> = fields.iter().map(|field| field.0 as usize).collect();
                indices.sort_unstable();
                indices.dedup();
                indices
            }
            None => (0..descriptor.fields.len()).collect(),
        };
        for index in indices {
            let field = FieldId(index as u16);
            let structure = UnitKey::field(descriptor.id, field, UnitKind::Structure);
            let materialized =
                region.units.get(&structure).is_some() || state.units.get(&structure).is_some();
            if !materialized || Self::structure_is_published(region, field) {
                continue;
            }
            Self::refresh_structure(state, region, field, revision, changed_nodes);
        }
    }

    /// The materialized cells one whole-region replacement owns.
    ///
    /// A replacement with no cell write beside it names no delta: it replaced
    /// the content every cell is a projection of, and the runtime has no
    /// type-erased equality to tell which cell's payload survived. So the cell
    /// is opened for a new generation — its readers re-execute, and the next
    /// read re-derives it from the committed content, where a projection the
    /// content vtable cannot spell (a rank or adjacency index) is re-derived
    /// through the accessor that materialized it.
    ///
    /// Reading one of these cells in the replacing group is read-your-own-write
    /// for the same reason, and [`InstalledPorts`] answers that question with
    /// this one predicate.
    ///
    /// Ownership is answered from `region`, not from `state.regions`: the
    /// publisher holds the region taken out of the map, so asking the map would
    /// find no View and report every cell unowned.
    ///
    /// A structure unit is the field's own projection and was just republished
    /// with the content's equality, so it is not stale; a provider-owned cell is
    /// the provider's to publish.
    pub(super) fn replacement_owns_cell(
        state: &State,
        region: &Region,
        unit: &UnitKey,
        node: NodeId,
    ) -> bool {
        // An entity's cells project the content, and so does a whole-field
        // value cell. An entity-less membership unit is the field's keyed/whole
        // protocol — a certificate the definition pipeline publishes, never a
        // projection — and a structure unit was just republished above.
        (unit.entity.is_some() || unit.kind == UnitKind::Value)
            && region.owners.get(&owner_key(unit)).is_none()
            && node_is_live(state, node)
    }

    /// Whether the replacement content already projects this cell's payload.
    ///
    /// The runtime has no type-erased equality for a cell payload, so it asks
    /// the one equality it does have: installing the payload into the new
    /// content must leave that content unchanged. Presence is answered directly
    /// by `contains`, because a membership payload is a bool and a family's
    /// `define` reads a membership as a value definition. A family whose
    /// `define` cannot realize a unit — a tree, which is likewise replaced whole
    /// — answers "unchanged", the same trust the stated delta itself carries for
    /// that family.
    pub(super) fn cell_matches_content(
        state: &State,
        region: &Region,
        unit: &UnitKey,
        node: NodeId,
    ) -> bool {
        if unit.kind == UnitKind::Membership {
            let present = (region.descriptor.content.contains)(&region.content, unit);
            return membership_of(state, node) == Some(present);
        }
        let Some(payload) = state.node(node).value.clone() else {
            return false;
        };
        let next = (region.descriptor.content.define)(&region.content, unit, payload);
        (region.descriptor.content.eq)(&region.content, &next)
    }

    /// Opens the generation of every cell a transaction installed whose payload
    /// the content that transaction committed does not project.
    ///
    /// The publish decision for each staged write is measured once, against the
    /// state the transaction found. Two editors in one transaction each clone
    /// that state, and only the last replacement reaches the content, so a
    /// write can still publish a value the committed content contradicts. The
    /// content is the revision's answer: such a cell is opened for a new
    /// generation — its readers re-execute and the next read re-derives it —
    /// instead of serving the staged value for good.
    ///
    /// The pass runs after the transaction's whole publish loop, because a cell
    /// write staged after the content replacement is installed after the
    /// replacement too. Only payload and presence cells are checked, and a
    /// retraction's cells are never in `written`: it already owns their
    /// generation, and reopening one would let a read certify an absence the
    /// retraction must leave uncertified.
    pub(crate) fn reconcile_written_cells(
        state: &mut State,
        view: ViewId,
        written: &FxHashSet<UnitKey>,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        let _ = Self::with_taken_region(state, view, |state, region| {
            for unit in written {
                if !matches!(unit.kind, UnitKind::Value | UnitKind::Membership) {
                    continue;
                }
                let Some(node) = current_unit_node(state, unit) else {
                    continue;
                };
                if Self::cell_matches_content(state, region, unit, node) {
                    continue;
                }
                let value_version = state.alloc_value_version();
                state
                    .node_mut(node)
                    .begin_generation(value_version, revision);
                changed_nodes.push(node);
            }
        });
    }

    /// Opens a new generation of every materialized cell a replacement owns.
    ///
    /// `delta` says what the same commit writes itself. A commit that stages
    /// cells beside the replacement claims their sum is the whole content delta,
    /// so each remaining cell is checked: one that still projects its payload is
    /// left alone, and one that does not is the replacement's. A commit that
    /// stages no cell names no delta, so the replacement owns every cell it did
    /// not install.
    pub(super) fn invalidate_cells(
        state: &mut State,
        region: &Region,
        delta: CellDelta<'_>,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        let view = region.descriptor.id;
        let Some(cells) = state.cells.get(&view) else {
            return;
        };
        let stale: Vec<(UnitKey, NodeId)> = cells
            .iter()
            .filter_map(|(key, node)| {
                let unit = key.as_any().downcast_ref::<UnitKey>()?;
                let owns = Self::replacement_owns_cell(state, region, unit, *node)
                    && !delta.installed.contains(unit);
                owns.then(|| (unit.clone(), *node))
            })
            .collect();
        for (unit, node) in stale {
            // A stated delta owns the value cells its transaction did not write —
            // one staged cell cannot stand in for a content that moves another.
            // Presence is the exception: every editor stages the memberships it
            // changes, and a family answers presence in its own way (a tree's
            // node set is not its membership field), so a presence cell the
            // transaction did not write did not move.
            let kept = if delta.stated {
                unit.kind == UnitKind::Membership
                    || Self::cell_matches_content(state, region, &unit, node)
            } else {
                unit.kind == UnitKind::Membership
                    && Self::cell_matches_content(state, region, &unit, node)
            };
            if kept {
                continue;
            }
            // The cell's payload belongs to this revision now, even though it
            // is installed by the next read: a change-version observer of the
            // cell must be told about the replacement that moved it, at the
            // revision that moved it.
            let value_version = state.alloc_value_version();
            state
                .node_mut(node)
                .begin_generation(value_version, revision);
            changed_nodes.push(node);
        }
    }

    /// Refreshes one field's structure projection unless a provider publishes
    /// that unit.
    ///
    /// A field's structure is the projection of its content, so every path that
    /// moves content has to republish a materialized structure cell — the
    /// replacement path and the retraction path alike. The cell is not always
    /// the runtime's to write: a keyed publisher stages the structure unit
    /// itself, and a provider-owned cell is the provider's to publish on every
    /// path. Both callers ask [`Self::structure_is_published`], because a rule
    /// answered twice drifts.
    pub(super) fn refresh_unowned_structure(
        state: &mut State,
        region: &mut Region,
        field: FieldId,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        if Self::structure_is_published(region, field) {
            return;
        }
        Self::refresh_structure(state, region, field, revision, changed_nodes);
    }
}
