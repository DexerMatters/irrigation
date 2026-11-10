//! The definition pipeline: one write group's drafts, retractions, and owners.
//!
//! Part of the kernel's `impl Inner`; the struct and its lock live in the
//! parent module.

use super::*;

impl Inner {
    /// Checks one unit of a write group against the group's authority.
    ///
    /// Installation and retraction share it: a unit is writable only from a
    /// registered View, from a View the provider declared, and from the provider
    /// component its route plan names.
    pub(super) fn validate_unit(
        state: &State,
        provider: NodeId,
        unit: &UnitKey,
        enforce_views: bool,
    ) -> Result<()> {
        if !state.regions.contains_key(&unit.view) {
            return Err(ComputeError::DefinitionDenied {
                unit: format!("{unit:?}"),
                reason: String::from("view is not registered"),
            });
        }
        if enforce_views {
            let declared = state
                .declared_views
                .get(&state.node(provider).scope)
                .is_some_and(|views| views.contains(&unit.view));
            if !declared {
                return Err(ComputeError::DefinitionDenied {
                    unit: format!("{unit:?}"),
                    reason: String::from(
                        "the provider did not declare this View (call `ensure_view` first)",
                    ),
                });
            }
        }
        if let Some(planned) = state.route_plans.get(unit) {
            let provider_component = component_of(state, state.node(provider).scope);
            if provider_component != Some(*planned) {
                return Err(ComputeError::DefinitionDenied {
                    unit: format!("{unit:?}"),
                    reason: String::from("the unit is planned for a different provider component"),
                });
            }
        }
        Ok(())
    }

    /// Checks one unit a write group retracts against the group's authority.
    ///
    /// Retraction mirrors installation: the units a group removes answer to the
    /// same authority as the units it installs, and another live provider's unit
    /// is a conflict rather than a silent removal.
    pub(super) fn validate_retraction(
        state: &State,
        provider: NodeId,
        unit: &UnitKey,
    ) -> Result<()> {
        let owner = state
            .regions
            .get(&unit.view)
            .and_then(|region| region.owners.get(&owner_key(unit)));
        if let Some(owner) = owner
            && !retraction_may_take_over(state, provider, owner, unit)
        {
            return Err(ComputeError::DefinitionDenied {
                unit: format!("{unit:?}"),
                reason: String::from("the unit is owned by another live provider"),
            });
        }
        Ok(())
    }

    /// Checks every draft and explicit retraction of one write group before any
    /// of it is installed.
    ///
    /// The install itself calls this again; the commit calls it first so View
    /// content only joins a write group that can no longer be rejected.
    pub(super) fn validate_definitions(
        state: &State,
        provider: NodeId,
        drafts: &[DefinitionDraft],
        retractions: &[UnitKey],
        enforce_views: bool,
    ) -> Result<()> {
        for draft in drafts {
            Self::validate_unit(state, provider, &draft.unit, enforce_views)?;
            if draft_states_absence(draft) {
                // A `Membership = false` draft is the presence spelling of the
                // entity's retraction, so it answers to the retraction's
                // authority: another live provider's entity is a conflict, not a
                // silent removal.
                Self::validate_retraction(state, provider, &payload_key(&draft.unit))?;
            }
        }
        for unit in retractions {
            Self::validate_unit(state, provider, unit, enforce_views)?;
            Self::validate_retraction(state, provider, unit)?;
        }
        let restaged: FxHashSet<UnitKey> = drafts.iter().map(|draft| draft.unit.clone()).collect();
        if restaged.len() != drafts.len() {
            let mut seen: std::collections::HashMap<
                UnitKey,
                Option<&'static std::panic::Location<'static>>,
            > = std::collections::HashMap::new();
            for draft in drafts {
                if let Some(first_location) = seen.get(&draft.unit) {
                    return Err(ComputeError::DuplicateDefinition {
                        unit: format!("{:?}", draft.unit),
                        first: describe_provider(draft.label, *first_location),
                        second: describe_provider(draft.label, draft.location),
                    });
                }
                seen.insert(draft.unit.clone(), draft.location);
            }
        }
        Ok(())
    }

    /// Installs one write group into its View regions.
    ///
    /// The caller has already run [`Self::validate_definitions`] on the same
    /// group; every region is known to exist and every draft is known to be the
    /// only one for its unit.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn install_definitions(
        state: &mut State,
        provider: NodeId,
        drafts: Vec<DefinitionDraft>,
        definition_mode: (Vec<UnitKey>, bool),
        revision: Revision,
        enforce_views: bool,
        decisions: &[RouteDecision],
        changed_nodes: &mut Vec<NodeId>,
    ) {
        debug_assert!(
            Self::validate_definitions(state, provider, &drafts, &definition_mode.0, enforce_views)
                .is_ok(),
            "a write group is validated before it is installed"
        );
        let (retractions, restate_all) = definition_mode;
        let retracted: FxHashSet<UnitKey> = retractions.into_iter().collect();
        // Presence is decided once per write group, from its final drafts: a
        // `Membership = false` draft states that the entity is absent, so the
        // group retracts it instead of installing a presence cell. The decision
        // is normalised to the owner key every retraction names, which releases
        // the provider's records for both cells and takes the drafts out of the
        // install below.
        let withdrawn = withdrawn_entities(&drafts);
        // A retraction may name either of an entity's cells, and the record it
        // releases is the ownership pair. Comparing full keys would let a
        // presence-only retraction leave the payload's definition installed.
        let released: FxHashSet<UnitKey> = retracted
            .iter()
            .map(owner_key)
            .chain(withdrawn.iter().cloned())
            .collect();
        // Cloned on purpose, and it is the largest per-commit allocation in the
        // pipeline for a provider with a big output (135,280 records for the
        // syntax fixture's AST view), so taking it looks like an easy win. It is
        // kept because this set is the only record of what the provider owns and
        // the commit can unwind -- a panicking host equality in the content
        // vtable is re-thrown -- which would leave the provider owning nothing
        // until it runs again. That window is not reachable through the public
        // API today (a read of one of its units demands the producer and gets the
        // cached failure), so the clone is a precaution rather than a proven
        // requirement; taking it measured 41 s -> 39.8 s on the fixture suite,
        // which is not worth trading the invariant for.
        let old: Vec<DefinitionRecord> = state.node(provider).definitions.clone();
        let restaged: FxHashSet<UnitKey> = drafts
            .iter()
            .filter(|draft| !draft_withdrawn(draft, &withdrawn))
            .map(|draft| draft.unit.clone())
            .collect();

        // A field-level `Value` draft publishes its field as one value, not as a
        // delta against entities: the entity cells this provider staged for that
        // field and did not restate belong to the value that was replaced.
        //
        // Two rules retire an entity unit this run does not restate, and each
        // owns one kind of provider:
        //
        // * A lowered component restates its whole output every run, so
        //   `restate_all` retires whatever it stopped stating.
        // * A custom component writes deltas, so `superseded` is the rule that
        //   makes a whole-field draft replace the entity units of its field:
        //   a delta publisher that stops listing an id would otherwise leave
        //   that id's membership and payload cells serving it forever.
        //
        // They overlap on a lowered component that stages a whole-field draft,
        // where either alone is enough. The scan below is therefore only worth
        // its cost for the delta case: `restate_all` already covers the rest, and
        // the record set it would walk is every definition the provider owns.
        let superseded: FxHashSet<UnitKey> = if restate_all {
            FxHashSet::default()
        } else {
            let replaced: FxHashSet<(ViewId, FieldId)> = drafts
                .iter()
                .filter(|draft| draft.unit.entity.is_none() && draft.unit.kind == UnitKind::Value)
                .map(|draft| (draft.unit.view, draft.unit.field))
                .collect();
            old.iter()
                .filter(|record| {
                    record.unit.entity.is_some()
                        && replaced.contains(&(record.unit.view, record.unit.field))
                        && !restaged.contains(&record.unit)
                })
                .map(|record| record.unit.clone())
                .collect()
        };

        // Retractions of one View batch their content publish, and the fields
        // they move are refreshed once by the install pass that follows.
        let mut stale_entities: FxHashMap<ViewId, Vec<UnitKey>> = FxHashMap::default();
        let mut retracted_dirty: FxHashMap<ViewId, Vec<FieldId>> = FxHashMap::default();
        // Retractions first: a unit this provider no longer produces must
        // disappear atomically with the new value. A unit is removed only when
        // it is retracted explicitly, superseded by a whole-field replacement,
        // or the run restates everything; absence alone keeps the committed
        // value.
        for record in &old {
            if restaged.contains(&record.unit) {
                continue;
            }
            if withdrawn.contains(&owner_key(&record.unit)) {
                // The group's own presence decision clears this pair, after the
                // drafts it does install are rewritten into the content.
                continue;
            }
            if !restate_all
                && !released.contains(&owner_key(&record.unit))
                && !superseded.contains(&record.unit)
            {
                continue;
            }
            let view = record.unit.view;
            if record.unit.entity.is_some() {
                stale_entities
                    .entry(view)
                    .or_default()
                    .push(record.unit.clone());
                continue;
            }
            let outcome = Self::with_taken_region(state, view, |state, region| {
                Self::retract_unit(
                    state,
                    region,
                    provider,
                    &record.unit,
                    revision,
                    changed_nodes,
                    false,
                );
            });
            debug_assert!(outcome.is_some(), "a validated View has its region");
        }

        // Explicit retractions that are not part of this provider's old record
        // set: a branch site that was not taken last revision can retract the
        // unit it used to own without waiting for the old branch node to be
        // reclaimed.
        let mut explicit_entities: FxHashMap<ViewId, Vec<UnitKey>> = FxHashMap::default();
        for unit in &retracted {
            if restaged.contains(unit) || superseded.contains(unit) {
                continue;
            }
            let view = unit.view;
            if unit.entity.is_some() {
                explicit_entities
                    .entry(view)
                    .or_default()
                    .push(unit.clone());
                continue;
            }
            let outcome = Self::with_taken_region(state, view, |state, region| {
                Self::retract_unit(state, region, provider, unit, revision, changed_nodes, true);
            });
            debug_assert!(outcome.is_some(), "a View that owns a unit has its region");
        }

        // The two batches differ only in authority: a stale record is only
        // released, an explicit retraction may take a sibling site's unit over.
        for (batches, force) in [(stale_entities, false), (explicit_entities, true)] {
            for (view, units) in batches {
                let outcome = Self::with_taken_region(state, view, |state, region| {
                    let mut dirty = retracted_dirty.remove(&view).unwrap_or_default();
                    Self::retract_entities(
                        state,
                        region,
                        provider,
                        &units,
                        force,
                        revision,
                        changed_nodes,
                        &mut dirty,
                    );
                    retracted_dirty.insert(view, dirty);
                });
                debug_assert!(outcome.is_some(), "a validated View has its region");
            }
        }
        let mut by_view: FxHashMap<ViewId, Vec<DefinitionDraft>> = FxHashMap::default();
        for draft in drafts {
            by_view.entry(draft.unit.view).or_default().push(draft);
        }
        let mut installed: Vec<DefinitionRecord> = old
            .into_iter()
            .filter(|record| {
                !restaged.contains(&record.unit)
                    && !released.contains(&owner_key(&record.unit))
                    && !superseded.contains(&record.unit)
                    && !restate_all
            })
            .collect();
        for (view, drafts) in by_view {
            let applied = Self::with_taken_region(state, view, |state, region| {
                Self::apply_definitions(
                    state,
                    region,
                    provider,
                    &drafts,
                    &withdrawn,
                    revision,
                    decisions,
                    changed_nodes,
                    &mut installed,
                    retracted_dirty.remove(&view).unwrap_or_default(),
                );
            });
            debug_assert!(applied.is_some(), "a validated View has its region");
        }
        // A View whose retractions moved a field but which installs nothing this
        // run never reaches `apply_definitions`, so its projection is refreshed
        // here — still once per field.
        for (view, fields) in retracted_dirty {
            if fields.is_empty() {
                continue;
            }
            let outcome = Self::with_taken_region(state, view, |state, region| {
                for field in fields {
                    Self::refresh_unowned_structure(state, region, field, revision, changed_nodes);
                }
            });
            debug_assert!(outcome.is_some(), "a validated View has its region");
        }
        // Retained definitions are still this provider's output for the
        // revision: certify them so readers and reuse checks see them as final.
        // Only restaged units are written this run.
        for record in &installed {
            if restaged.contains(&record.unit) {
                continue;
            }
            if let Some(node) = current_unit_node(state, &record.unit) {
                let node_record = state.node_mut(node);
                if node_record.value.is_some() && node_record.verified_at < revision {
                    node_record.certify(revision);
                }
            }
        }
        state.node_mut(provider).definitions = installed;
    }

    /// Moves one field's keyed/whole protocol and its certificate together.
    ///
    /// The bit and the cell must land in the same revision, in the same commit:
    /// a reader that validates between them would take the branch of the old
    /// mode against the new content. The certificate is written only when the
    /// bit actually moves, so a keyed publication that restages `Value` then
    /// `Structure` on every run does not churn the readers it wakes.
    pub(super) fn set_field_protocol(
        state: &mut State,
        view: ViewId,
        field: FieldId,
        keyed: bool,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) {
        let moved = state.keyed_fields.contains(&(view, field)) != keyed;
        if keyed {
            state.keyed_fields.insert((view, field));
        } else {
            state.keyed_fields.remove(&(view, field));
        }
        if !moved {
            return;
        }
        // A field is keyed only after this ran with `keyed`, so a missing
        // certificate means the field has never been keyed: the new mode is a
        // transition no reader can have observed yet.
        let key = UnitKey::field(view, field, UnitKind::Membership);
        let node = match state.units.get(&key).copied() {
            Some(node) => node,
            None => {
                let node = state.alloc(NodeRecord::unit("field protocol", key.clone()));
                state.units.insert(Arc::new(key), node);
                node
            }
        };
        let value_version = state.alloc_value_version();
        let record = state.node_mut(node);
        record.install_value(Some(value_of(keyed)), value_version, revision);
        record.routing_at = revision;
        changed_nodes.push(node);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_definitions(
        state: &mut State,
        region: &mut Region,
        provider: NodeId,
        drafts: &[DefinitionDraft],
        withdrawn: &FxHashSet<UnitKey>,
        revision: Revision,
        decisions: &[RouteDecision],
        changed_nodes: &mut Vec<NodeId>,
        installed: &mut Vec<DefinitionRecord>,
        retracted_dirty: Vec<FieldId>,
    ) {
        let descriptor = region.descriptor;
        let mut content_drafts: Vec<(UnitKey, Value)> = Vec::new();
        let mut content_changed = false;
        // Fields the group's own retractions moved before the installs ran. They
        // are seeded here so this View's single flush republishes every moved
        // projection once, from the group's final content.
        let mut dirty_fields: Vec<FieldId> = retracted_dirty;
        // Fields the group's presence decisions move. They are flushed under
        // their own rule, because a presence decision answers after the drafts.
        let mut presence_fields: Vec<FieldId> = Vec::new();
        let mut protocols: Vec<(FieldId, bool)> = Vec::new();
        for draft in drafts {
            // The group's presence decision owns this entity's cells: a
            // presence draft is not an install, and a payload staged beside it
            // states a value the decision marks absent.
            if draft_withdrawn(draft, withdrawn) {
                continue;
            }
            // The protocol a field ends the commit with is the one its last
            // declaring draft names; `define_keyed` stages the whole-field
            // value first and the structure after it.
            if let Some(keyed) = draft_protocol(&draft.unit, descriptor.fields) {
                match protocols
                    .iter_mut()
                    .find(|(field, _)| *field == draft.unit.field)
                {
                    Some(entry) => entry.1 = keyed,
                    None => protocols.push((draft.unit.field, keyed)),
                }
            }
            let owner = owner_key(&draft.unit);
            let previous_owner = region.owners.get(&owner).map(|existing| existing.node);
            if let Some(existing) = region.owners.get(&owner) {
                if existing.node != provider {
                    // A route selector is host code, so the choice was made
                    // before the lock was taken; it applies only while the
                    // collision it was made for is still the current one.
                    match route_winner(decisions, &draft.unit, existing.node, provider) {
                        Some(winner) if winner == existing.node => continue,
                        Some(_) => {
                            // The selected provider keeps the unit and the other
                            // claim is rejected or transferred now.
                            let previous = existing.node;
                            state.node_mut(previous).definitions.retain(|definition| {
                                definition.unit != draft.unit || definition.provider != previous
                            });
                            force_recompute(state, &[previous], revision);
                        }
                        None => {
                            // Resolve at the end of the revision: if the earlier
                            // owner is still reachable then both providers are
                            // live and the revision fails; if it retired during
                            // the revision, this was a handover.
                            state.pending_conflicts.push(crate::state::PendingConflict {
                                unit: draft.unit.clone(),
                                first: existing.node,
                                second: provider,
                                second_location: draft.location,
                                first_owner: existing.clone(),
                            });
                        }
                    }
                }
            }
            // Whether this is the cell's first appearance. A definition that
            // only restates the committed content is that appearance, not a
            // generation of its own.
            let fresh_cell = state.units.get(&draft.unit).is_none();
            let value_node = match region.units.get(&draft.unit) {
                Some(slot) => slot.node,
                None => {
                    let node = Self::ensure_unit_node(state, draft.unit.clone(), None);
                    region.units.insert(
                        any_key(draft.unit.clone()),
                        UnitSlot {
                            node,
                            present: false,
                        },
                    );
                    node
                }
            };
            let born_at = fresh_cell
                .then(|| Self::content_answer(state, region, &draft.unit, &draft.value, draft.eq))
                .flatten();
            let changed = born_at.is_none()
                && state
                    .node(value_node)
                    .value
                    .as_ref()
                    .is_none_or(|old| !(draft.eq)(old, &draft.value));
            let write = changed || born_at.is_some();
            let value_version = write.then(|| state.alloc_value_version());
            {
                let record = state.node_mut(value_node);
                if let Some(value_version) = value_version {
                    // A cell born from the content keeps the revision that gave
                    // it the answer, so no subscription reads the definition as
                    // a change. A real change is stamped with this revision.
                    let changed_at = born_at.unwrap_or(revision);
                    record.install_value(Some(Arc::clone(&draft.value)), value_version, changed_at);
                    if changed {
                        changed_nodes.push(value_node);
                    }
                } else {
                    record.certify(revision);
                    record.suspect_at = 0;
                    record.state = NodeState::Ready;
                    record.failure = None;
                }
                if previous_owner != Some(provider) {
                    record.routing_at = revision;
                }
            }
            if draft.unit.entity.is_some() {
                let membership = membership_key(&draft.unit);
                let member_node = match region.units.get(&membership) {
                    Some(slot) => slot.node,
                    None => {
                        let node = Self::ensure_unit_node(state, membership.clone(), None);
                        region.units.insert(
                            any_key(membership.clone()),
                            UnitSlot {
                                node,
                                present: false,
                            },
                        );
                        node
                    }
                };
                // A membership draft states presence itself; every other
                // entity unit installs the entity, so its membership is true. A
                // false presence never reaches here: the group's retraction
                // decided it before any cell installed.
                let present = draft.unit.kind != UnitKind::Membership
                    || draft.value.downcast_ref::<bool>().copied().unwrap_or(true);
                // Presence is one question with two spellings: the cell, or the
                // committed content while the cell holds no answer. A payload
                // draft whose entity the content already holds materializes the
                // presence at the content's revision — the answer is not new,
                // only its cell is, and a subscription seeded from that content
                // must not read the definition as a change.
                let recorded = membership_of(state, member_node);
                let present_in_content = recorded.is_none()
                    && (descriptor.content.contains)(&region.content, &membership);
                if draft.unit.kind != UnitKind::Membership && recorded != Some(true) {
                    let value_version = state.alloc_value_version();
                    let changed_at = if present_in_content {
                        state.node(region.content_node).changed_at
                    } else {
                        revision
                    };
                    let dependents = state.node(member_node).dependents.clone();
                    let record = state.node_mut(member_node);
                    record.install_value(Some(value_of(true)), value_version, changed_at);
                    record.routing_at = revision;
                    if present_in_content {
                        // The revision is the content's, not this one, but the
                        // payload is new to the graph: a reader that observed
                        // the vacant cell — the documented negative read — holds
                        // an absence this presence contradicts, and being
                        // certified for the revision would keep it. Mark those
                        // readers like `republish_cell` does; the cell itself is
                        // not seeded, because a subscription must still read
                        // this materialisation as no change.
                        if !dependents.is_empty() {
                            invalidate(state, dependents, revision);
                        }
                    } else {
                        changed_nodes.push(member_node);
                    }
                }
                if let Some(slot) = region.units.get_mut(&membership) {
                    slot.present = present;
                }
                if changed && !dirty_fields.contains(&draft.unit.field) {
                    dirty_fields.push(draft.unit.field);
                }
            }
            // An equal restatement is already represented by the committed
            // content; applying it again is pure whole-content work. Only a
            // unit whose value actually moved can move the content, and a
            // cell born from the content is already in it.
            if changed && (descriptor.content.moves_content)(&draft.unit) {
                state
                    .stats
                    .content_applies
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                content_changed = true;
                content_drafts.push((draft.unit.clone(), Arc::clone(&draft.value)));
                // A per-entity payload update cannot move keys/order/topology.
                // A whole-field payload or membership/structure unit can.
                if (draft.unit.kind != UnitKind::Value || draft.unit.entity.is_none())
                    && !dirty_fields.contains(&draft.unit.field)
                {
                    dirty_fields.push(draft.unit.field);
                }
            } else if changed {
                state
                    .stats
                    .content_projections
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            } else {
                state
                    .stats
                    .content_restatements
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            if previous_owner != Some(provider) {
                region.routing_version = region.routing_version.wrapping_add(1);
            }
            region.owners.insert(
                Arc::new(owner),
                ProviderRef {
                    node: provider,
                    label: draft.label,
                    location: draft.location,
                },
            );
            installed.push(DefinitionRecord {
                unit: draft.unit.clone(),
                provider,
                value: Arc::clone(&draft.value),
            });
        }
        if content_changed {
            let content = (descriptor.content.define_many)(&region.content, &content_drafts);
            let units: Vec<UnitKey> = content_drafts
                .iter()
                .map(|(unit, _)| unit.clone())
                .collect();
            let equal = (descriptor.content.changed_units_equal)(&region.content, &content, &units);
            if equal != Some(true) {
                state
                    .stats
                    .content_batches
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Self::commit_content_changed(state, region, content, revision, changed_nodes);
            }
        }
        for (field, keyed) in protocols {
            Self::set_field_protocol(state, descriptor.id, field, keyed, revision, changed_nodes);
        }
        // The group's presence decisions land after its content rewrite: a
        // `Membership = false` draft states that the entity is absent, and
        // absence implies the payload is absent. Both cells go through the one
        // retraction primitive, so no draft of the same group can leave a
        // payload certified behind an absence it stated itself.
        let withdrawn_units: Vec<UnitKey> = withdrawn
            .iter()
            .filter(|owner| owner.view == descriptor.id)
            .map(payload_key)
            .collect();
        if !withdrawn_units.is_empty() {
            Self::retract_entities(
                state,
                region,
                provider,
                &withdrawn_units,
                true,
                revision,
                changed_nodes,
                &mut presence_fields,
            );
        }
        // One flush for the whole group, and the projection each moved field
        // gets is decided by who answered last:
        //
        // * A field the group *publishes* a structure for has already been
        //   answered by that draft, so refreshing it again would write the cell
        //   a second time and could only produce the value it already holds.
        // * A field the group's presence decisions moved has no such answer:
        //   those drafts were computed from the content *before* the retraction,
        //   so the retraction is the group's last word and the projection must
        //   follow it.
        let published: FxHashSet<UnitKey> = drafts
            .iter()
            .filter(|draft| draft.unit.kind == UnitKind::Structure)
            .map(|draft| draft.unit.clone())
            .collect();
        for field in dirty_fields {
            let structure = UnitKey::field(descriptor.id, field, UnitKind::Structure);
            if published.contains(&structure) {
                continue;
            }
            Self::refresh_structure(state, region, field, revision, changed_nodes);
        }
        for field in presence_fields {
            Self::refresh_structure(state, region, field, revision, changed_nodes);
        }
    }

    /// Retracts one entity: presence false and payload absent.
    ///
    /// A member's cells answer one question, so every retraction lands here —
    /// a provider releasing a definition, and an editor staging
    /// `Membership = false` without a payload write beside it. The cells
    /// resolve through the unit table as well as the region, because a
    /// host-installed entity has no region slot and its retraction must still
    /// clear both.
    ///
    /// The entity is named by either of its cells: a retraction that stages the
    /// presence and one that stages the payload describe the same removal, so
    /// the key is normalised to the payload before anything is cleared. Retraction
    /// is the inverse of installation, which means the payload is *gone* and not
    /// the removed value left behind a cleared certification — the next install
    /// of the entity must be able to take it over, and a direct read of the unit
    /// must answer absent.
    ///
    /// Returns whether the entity was present.
    pub(crate) fn retract_entity(
        state: &mut State,
        unit: &UnitKey,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
    ) -> bool {
        let unit = &payload_key(unit);
        let membership = membership_key(unit);
        let removed = if let Some(node) = current_unit_node(state, &membership)
            && membership_of(state, node) == Some(true)
        {
            let value_version = state.alloc_value_version();
            let record = state.node_mut(node);
            record.install_value(Some(value_of(false)), value_version, revision);
            changed_nodes.push(node);
            true
        } else {
            false
        };
        // The payload belongs to the same generation as the presence: both cells
        // are this entity's, one retraction clears both, and the version and the
        // stamp move with the payload so no consumer keeps an observation of a
        // value the entity no longer has.
        //
        // The generation closes even when the cell holds no payload: a
        // replacement opened it with a version and a stamp of its own, and the
        // removal is what that generation changed for. Leaving it would keep the
        // payload's change version naming the replacement, so a payload
        // subscriber would never hear the removal. A cell that never held an
        // answer has no generation to close, and a cell another retraction in
        // this commit already retired keeps its cleared state.
        if let Some(node) = current_unit_node(state, unit) {
            let record = state.node(node);
            let closes_generation = record.value.is_some() || record.value_version != 0;
            let already_retired =
                record.value.is_none() && record.changed_at == revision && !record.content_stale;
            if closes_generation && !already_retired {
                Self::retire_cell(state, node, revision, changed_nodes);
            }
        }
        removed
    }

    pub(super) fn retract_unit(
        state: &mut State,
        region: &mut Region,
        provider: NodeId,
        unit: &UnitKey,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
        force: bool,
    ) {
        let descriptor = region.descriptor;
        let owner = owner_key(unit);

        if let Some(existing) = region.owners.get(&owner) {
            // Retirement only releases ownership this provider still holds. An
            // explicit retraction may take a stale unit over, or a sibling
            // definition site of its own component; every other live provider's
            // unit was rejected by validation before the install.
            let may_take_over = force && retraction_may_take_over(state, provider, existing, unit);
            if existing.node != provider && !may_take_over {
                return;
            }
        }
        // A branch site can retire after another site replaced the field in the
        // same revision. The cells it defined are still its own to clear, but
        // the content now belongs to the replacement.
        let owns_content = Self::retraction_owns_content(state, region, provider, unit);
        if region.owners.get(&owner).is_some() && owns_content {
            region.routing_version = region.routing_version.wrapping_add(1);
        }
        region.owners.remove(&owner);
        let mut content = Arc::clone(&region.content);
        if owns_content {
            if unit.entity.is_none() && unit.kind == UnitKind::Structure {
                // Retraction mirrors installation: a retracted structure is a
                // field that is no longer keyed, and its readers must hear about
                // it. A whole-field value retracts into the whole protocol the
                // value installed, so its bit already reads false and nothing
                // moves.
                Self::set_field_protocol(
                    state,
                    unit.view,
                    unit.field,
                    false,
                    revision,
                    changed_nodes,
                );
            }
            content = (descriptor.content.retract)(&content, unit);
            if unit.entity.is_none() {
                // A whole-field definition was retracted: the field reverts to
                // its empty value, and the unit node takes that value so readers
                // and snapshots agree. The two whole-field units retract from
                // different spellings: a structure unit is the family's
                // projection of the field, while a value unit is the storage
                // the definition installed.
                let value = match unit.kind {
                    UnitKind::Structure => (descriptor.content.structure)(&content, unit.field)
                        .map(|structure| structure.value),
                    UnitKind::Value => (descriptor.content.field_storage)(&content, unit.field),
                    _ => None,
                };
                if let Some(slot) = region.units.get(unit) {
                    let node = slot.node;
                    let value_version = state.alloc_value_version();
                    let record = state.node_mut(node);
                    record.install_value(value, value_version, revision);
                    record.routing_at = revision;
                    changed_nodes.push(node);
                }
            }
        } else if unit.entity.is_none() && unit.kind == UnitKind::Value {
            // The replacement site publishes this field's protocol now, but no
            // provider refreshes a whole-field payload: the value this site
            // installed is nobody's answer any more, and the content vtable
            // cannot re-derive it. The structure half is *not* retired — the
            // incoming publisher refreshed that cell as its own, and it is the
            // field's projection under the protocol it just published.
            if let Some(node) = current_unit_node(state, unit)
                && state.node(node).value.is_some()
            {
                Self::retire_cell(state, node, revision, changed_nodes);
            }
        }
        let removed_membership = if unit.entity.is_some() {
            let membership = membership_key(unit);
            if let Some(slot) = region.units.get_mut(&membership) {
                slot.present = false;
            }
            Self::retract_entity(state, unit, revision, changed_nodes)
        } else {
            false
        };
        if !owns_content {
            return;
        }
        let content_moved = Self::commit_content(state, region, content, revision, changed_nodes);
        // The structure follows the content, not the presence cell: a
        // definition-path retraction can remove an entity the content spells
        // while no membership cell ever held `true` (a content-only key), and
        // the key set then moves without `removed_membership` saying so.
        if removed_membership
            || content_moved
            || (unit.entity.is_none()
                && (descriptor.content.structure)(&region.content, unit.field).is_some())
        {
            Self::refresh_unowned_structure(state, region, unit.field, revision, changed_nodes);
        }
    }

    /// Retracts a batch of one View's entity units with a single publish.
    ///
    /// [`Self::retract_unit`] is the single-unit primitive and publishes the
    /// field's content itself. A write group that retracts a whole field — a
    /// provider whose definition set shrank, a presence decision covering
    /// thousands of entities, a region deleted from a document — calls that
    /// primitive once per unit, and each call rebuilds and republishes the
    /// field's content: the group becomes quadratic in what it retracts.
    ///
    /// The per-unit half is unchanged and still the single owner of each
    /// entity's cells, ownership record and routing bump. Only the content
    /// publish is hoisted out of the loop, and the fields it moved are returned
    /// so the caller can refresh each projection exactly once, from the group's
    /// final content, instead of once per retracted unit.
    ///
    /// Entity units only. A whole-field unit retracts the field's *protocol* and
    /// reinstalls the unit's own storage, which is one unit by definition and
    /// stays on the single-unit path.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn retract_entities(
        state: &mut State,
        region: &mut Region,
        provider: NodeId,
        units: &[UnitKey],
        force: bool,
        revision: Revision,
        changed_nodes: &mut Vec<NodeId>,
        dirty_fields: &mut Vec<FieldId>,
    ) {
        let descriptor = region.descriptor;
        let mut content = Arc::clone(&region.content);
        let mut moved = false;
        for unit in units {
            let unit = &payload_key(unit);
            let owner = owner_key(unit);
            if let Some(existing) = region.owners.get(&owner) {
                // The same authority the single-unit primitive applies: this
                // provider's own unit, a sibling site of its own component, or a
                // stale record it may take over.
                let may_take_over =
                    force && retraction_may_take_over(state, provider, existing, unit);
                if existing.node != provider && !may_take_over {
                    continue;
                }
            }
            let owns_content = Self::retraction_owns_content(state, region, provider, unit);
            if owns_content {
                if region.owners.get(&owner).is_some() {
                    region.routing_version = region.routing_version.wrapping_add(1);
                }
                content = (descriptor.content.retract)(&content, unit);
                moved = true;
                if !dirty_fields.contains(&unit.field) {
                    dirty_fields.push(unit.field);
                }
            }
            region.owners.remove(&owner);
            let membership = membership_key(unit);
            if let Some(slot) = region.units.get_mut(&membership) {
                slot.present = false;
            }
            Self::retract_entity(state, unit, revision, changed_nodes);
        }
        if moved {
            // The batch knows it *attempted* a content retraction per unit, not
            // that the content moved: a unit the field never held leaves the
            // value equal. One comparison per group is what the per-unit path
            // paid once per unit, so the primitive that decides belongs here.
            Self::commit_content(state, region, content, revision, changed_nodes);
        }
    }

    /// Whether a field's structure unit is a provider's output.
    pub(super) fn structure_is_published(region: &Region, field: FieldId) -> bool {
        let structure = UnitKey::field(region.descriptor.id, field, UnitKind::Structure);
        region.owners.get(&owner_key(&structure)).is_some()
    }

    /// Whether retracting `unit` still describes its field's committed content.
    ///
    /// A field's content belongs to whichever protocol unit a site published: a
    /// `Structure` install publishes the keyed one, an entity-less `Value`
    /// install the whole one, and entity cells belong to the keyed one. A branch
    /// site can retire after another site replaced the field in the same
    /// revision, and the obsolete protocol's cells then retire as records, not
    /// as content.
    pub(super) fn retraction_owns_content(
        state: &State,
        region: &Region,
        provider: NodeId,
        unit: &UnitKey,
    ) -> bool {
        if unit.entity.is_some() {
            // Entity cells are keyed content; an entity-less value published in
            // the same field already replaced them.
            return state.keyed_fields.contains(&(unit.view, unit.field))
                || region
                    .owners
                    .get(&UnitKey::field(unit.view, unit.field, UnitKind::Value))
                    .is_none();
        }
        let other = match unit.kind {
            UnitKind::Structure => UnitKind::Value,
            UnitKind::Value => UnitKind::Structure,
            _ => return true,
        };
        let key = UnitKey::field(unit.view, unit.field, other);
        region
            .owners
            .get(&key)
            .is_none_or(|owner| owner.node == provider)
    }

    /// Resolves the definition conflicts the demand's commits staged.
    ///
    /// The conflicts were staged after the demand's last commit, so a restore
    /// here can invalidate a node this revision already finalized. That is the
    /// drain's situation exactly, and it is reported the same way: the caller
    /// must replay instead of carrying the result out.
    pub(crate) fn resolve_conflicts(&self, revision: Revision) -> Result<ConflictOutcome> {
        let selector = self.route_selector.lock().clone();
        let (conflicts, collisions) = {
            let mut state = self.lock();
            let conflicts = std::mem::take(&mut state.pending_conflicts);
            let collisions: Vec<(UnitKey, NodeId, NodeId)> = conflicts
                .iter()
                .map(|conflict| (conflict.unit.clone(), conflict.first, conflict.second))
                .collect();
            (conflicts, collisions)
        };
        if conflicts.is_empty() {
            return Ok(ConflictOutcome::Done);
        }
        // The selector is host code and runs before the lock is taken; a
        // decision is used only while the conflict it was made for is current.
        let decisions = decide_routes(selector.as_ref(), &collisions);
        let mut state = self.lock();
        let mut overtaken = false;
        for conflict in conflicts {
            // The earlier provider is still live when it still claims this
            // entity. A provider that retired during the revision released its
            // claim, which makes this a handover rather than a conflict. This
            // test works for root demands and for call-only demands alike, and
            // it asks about the entity: a provider that staged the member's
            // presence and the late one that staged its payload own the same
            // entity, so the kinds must compare by `owner_key`.
            let owner_of_unit = owner_key(&conflict.unit);
            let first_live = state
                .node(conflict.first)
                .definitions
                .iter()
                .any(|definition| owner_key(&definition.unit) == owner_of_unit);
            if !first_live {
                continue;
            }
            // A provider that was not certified for this revision has not run
            // in the current graph. A new branch site may take the unit over;
            // an authoritative provider that ran this revision still wins and
            // produces the duplicate diagnostic.
            let winner = route_winner(&decisions, &conflict.unit, conflict.first, conflict.second);
            if let Some(winner) = winner {
                if winner == conflict.second {
                    // The later provider keeps the unit; drop the earlier
                    // provider's stale definition record without touching the
                    // committed content.
                    //
                    let first = state.node_mut(conflict.first);
                    first.definitions.retain(|definition| {
                        owner_key(&definition.unit) != owner_of_unit
                            || definition.provider != conflict.first
                    });
                    // The unit already serves the later provider's value, so
                    // this arm restores nothing — but retiring the earlier
                    // provider re-runs both, and a value this revision had
                    // already published from one of them is a result the demand
                    // would carry out after discarding it. No commit follows to
                    // revalidate it, so it reports the overtake the same way the
                    // restore arm's own invalidation does.
                    overtaken |= force_recompute_tracked(
                        &mut state,
                        &[conflict.first, conflict.second],
                        revision,
                    );
                    continue;
                }
                // The earlier provider keeps the route; retract the late one
                // and restore the selected provider's committed post-state.
                overtaken |= Self::restore_conflict(&mut state, &conflict, revision);
                force_recompute(&mut state, &[conflict.first, conflict.second], revision);
                continue;
            }
            let unit = format!("{:?}", conflict.unit);
            let first =
                describe_provider(conflict.first_owner.label, conflict.first_owner.location);
            let second =
                describe_provider(state.node(conflict.second).label, conflict.second_location);
            // The revision is rejected without losing the committed
            // definition: the late provider's install is undone, so the View
            // keeps the earlier provider's value and every node that serves it.
            // The duplicate is the outcome the host needs, so it wins over the
            // overtaken decision a replay would otherwise report.
            let _ = Self::restore_conflict(&mut state, &conflict, revision);
            return Err(ComputeError::DuplicateDefinition {
                unit,
                first,
                second,
            });
        }
        Ok(if overtaken {
            ConflictOutcome::Overtaken
        } else {
            ConflictOutcome::Done
        })
    }

    /// Undoes a later provider's install of a contested unit.
    ///
    /// The earlier provider's committed value, content, owner entry, and nodes
    /// are restored through the same primitives as a commit. The restore is an
    /// install after the demand's last commit, so its invalidation is tracked:
    /// the returned flag is `invalidate_tracked`'s, and it says a node this
    /// revision had already finalized was marked.
    pub(super) fn restore_conflict(
        state: &mut State,
        conflict: &crate::state::PendingConflict,
        revision: Revision,
    ) -> bool {
        // The earlier provider's record is the one that owns this entity, and
        // it may spell the unit with the other kind: a set publishes a member's
        // presence while a map publishes its payload. The restore therefore
        // replays that record's own unit and value rather than synthesising the
        // late provider's spelling as a payload of the earlier owner.
        let owner_of_unit = owner_key(&conflict.unit);
        let restored = state
            .node(conflict.first)
            .definitions
            .iter()
            .find(|definition| owner_key(&definition.unit) == owner_of_unit)
            .map(|definition| (definition.unit.clone(), definition.value.clone()));
        let seeds = Self::with_taken_region(state, conflict.unit.view, |state, region| {
            let mut changed = Vec::new();
            Self::retract_unit(
                state,
                region,
                conflict.second,
                &conflict.unit,
                revision,
                &mut changed,
                false,
            );
            state
                .node_mut(conflict.second)
                .definitions
                .retain(|definition| {
                    owner_key(&definition.unit) != owner_of_unit
                        || definition.provider != conflict.second
                });
            if let Some((unit, value)) = restored {
                let content =
                    (region.descriptor.content.define)(&region.content, &unit, Arc::clone(&value));
                Self::commit_content(state, region, content, revision, &mut changed);
                region
                    .owners
                    .insert(Arc::new(owner_key(&unit)), conflict.first_owner.clone());
                Self::restore_definition(state, region, &unit, value, revision, &mut changed);
            }
            // Restoring the unit restores the field's structure too: the
            // retraction above published the post-retraction topology, and a
            // restored collection must not keep serving it.
            Self::refresh_structure(state, region, conflict.unit.field, revision, &mut changed);
            changed
        })
        .unwrap_or_default();
        if seeds.is_empty() {
            return false;
        }
        invalidate_tracked(state, seeds, revision).1
    }
}
