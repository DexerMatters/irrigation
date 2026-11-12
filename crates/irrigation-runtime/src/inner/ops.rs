//! The kernel behind the crate's own interface: the runtime-ops trait and
//! reclamation.
//!
//! `RuntimeOps` is what every frame, helper, and general component talks to, so
//! its implementation here is the single door into the kernel. Invocation
//! eviction and reclamation share the file because they are the one place that
//! removes what the trait installed.

use super::*;

impl RuntimeOps for Inner {
    fn revision(&self) -> Revision {
        self.current_revision()
    }

    fn eval(
        &self,
        node: NodeId,
        compute: &mut dyn FnMut() -> Result<Value>,
        eq: &dyn Fn(&Value, &Value) -> bool,
    ) -> Result<Value> {
        Inner::eval(self, node, compute, eq)
    }

    fn site_node(&self, scope: ScopeId, site: SiteId) -> NodeId {
        Inner::site_node(self, scope, site)
    }

    fn unit_node(&self, view: ViewId, key: Arc<dyn AnyKey>) -> NodeId {
        Inner::cell_node(self, view, key)
    }

    fn unit_node_init(
        &self,
        view: ViewId,
        key: Arc<dyn AnyKey>,
        init: &mut dyn FnMut(&dyn Any) -> Option<Value>,
    ) -> NodeId {
        Inner::unit_node_init(self, view, key, init)
    }

    fn read_unit(&self, node: NodeId) -> Result<Value> {
        Inner::read_unit(self, node)
    }

    fn claim_write(
        &self,
        scope: WriteScope,
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<()> {
        Inner::claim_write(self, scope, label, location)
    }

    fn claim_writes(
        &self,
        scopes: &[WriteScope],
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<()> {
        Inner::claim_writes(self, scopes, label, location)
    }

    fn plan_writes(
        &self,
        scopes: &[WriteScope],
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<()> {
        Inner::plan_writes(self, scopes, label, location)
    }

    fn unplan_writes(&self, scopes: &[WriteScope]) -> Result<()> {
        Inner::unplan_writes(self, scopes)
    }

    fn runtime_id(&self) -> u64 {
        self.local_key
    }

    fn require_absence(&self, ports: &[UnitKey], description: &'static str) -> Result<()> {
        Inner::require_absence(self, ports, description)
    }

    fn require_condition(
        &self,
        condition: bool,
        ports: &[UnitKey],
        description: &'static str,
    ) -> Result<()> {
        Inner::require_condition(self, condition, ports, description)
    }

    fn provide_ports(&self, ports: &[UnitKey]) -> Result<bool> {
        Inner::provide_ports(self, ports)
    }

    fn wait_for_port(&self, ports: &[UnitKey]) -> Result<bool> {
        Inner::wait_for_port(self, ports)
    }

    fn ensure_unit(&self, unit: &UnitKey) -> Result<()> {
        let producer = {
            let state = self.lock();
            state.producers.get(&unit.view).copied()
        };
        if let Some(node) = producer {
            let ready = {
                let state = self.lock();
                let record = state.node(node);
                record.verified_at == state.revision
                    && record.has_valid_trace()
                    && record.value.is_some()
            };
            if !ready {
                self.demand_producer(node)?;
            }
        }
        let mut state = self.lock();
        if state.units.get(unit).is_some() {
            return Ok(());
        }
        // A cell has one creator. The runtime materializes the units the
        // committed content spells; a unit only its accessor can project — an
        // adjacency index, a whole-field payload, a field's protocol
        // certificate — stays without a cell until that accessor reads it,
        // because installing a vacant one would state an answer this call
        // cannot derive.
        let initial = state
            .regions
            .get(&unit.view)
            .and_then(|region| content_projection(region, unit));
        if initial.is_some() {
            Self::ensure_unit_node(&mut state, unit.clone(), initial);
        }
        Ok(())
    }

    fn child_scope(&self, parent: ScopeId, discriminant: u64, label: &'static str) -> ScopeId {
        Inner::child_scope(self, parent, discriminant, label)
    }

    fn field_is_keyed(&self, view: ViewId, field: FieldId) -> bool {
        Inner::field_is_keyed(self, view, field)
    }

    fn child_scope_keyed(
        &self,
        parent: ScopeId,
        key: Arc<dyn AnyKey>,
        label: &'static str,
    ) -> ScopeId {
        Inner::child_scope_keyed(self, parent, key, label)
    }

    fn check_cancelled(&self) -> Result<()> {
        match frame::current() {
            Some(frame) if frame.cancel().is_cancelled() => Err(ComputeError::Cancelled),
            _ => Ok(()),
        }
    }

    fn max_depth(&self) -> u32 {
        self.config.max_depth
    }

    fn view_content(&self, view: ViewId) -> Option<Arc<dyn Any + Send + Sync>> {
        Inner::view_content(self, view)
    }

    fn register_view(&self, descriptor: &'static ViewDescriptor) {
        Inner::register_view(self, descriptor);
    }

    fn workers(&self) -> usize {
        Inner::workers(self)
    }

    fn changed_at(&self, node: NodeId) -> Revision {
        Inner::changed_at(self, node)
    }

    fn validity_at(&self, node: NodeId) -> u64 {
        Inner::validity_at(self, node)
    }

    fn value_version_at(&self, node: NodeId) -> u64 {
        Inner::value_version_at(self, node)
    }

    fn suspend_request(&self, node: NodeId) -> bool {
        let me = task::current_or_detached();
        let mut state = self.lock();
        let pooled = state.tasks.get(&me).map(|slot| slot.mode) == Some(TaskMode::Pooled);
        if pooled {
            state.wait_graph.insert(me, node);
            if let Some(slot) = state.tasks.get_mut(&me) {
                slot.status = TaskStatus::Waiting(node);
            }
            merge_held(&mut state, me, &task::active_snapshot());
            state.node_mut(node).waiters.push(me);
        }
        pooled
    }

    fn declare_view(&self, scope: ScopeId, view: ViewId) {
        self.lock()
            .declared_views
            .entry(scope)
            .or_default()
            .insert(view);
    }

    fn try_read_unit(
        &self,
        node: NodeId,
        waker: &std::task::Waker,
    ) -> Result<std::task::Poll<Value>> {
        let me = task::current_or_detached();
        loop {
            match self.read_unit_step(node) {
                ReadStep::Ready(observation) => {
                    self.record_read(node, &observation);
                    return Ok(std::task::Poll::Ready(
                        observation.value.expect("a ready read carries a value"),
                    ));
                }
                ReadStep::Absent(observation) => {
                    self.record_read(node, &observation);
                    return Err(ComputeError::Absent { node });
                }
                ReadStep::Fail(error) => return Err(error),
                ReadStep::Retry => continue,
                ReadStep::Demand(producer) => {
                    self.demand_producer(producer)?;
                }
                ReadStep::Wait(target) => {
                    // A demand task waits like any other read: a pooled task
                    // yields its worker and an inline task parks. Only a
                    // detached receipt has to stay pending on a waker.
                    if self.lock().tasks.contains_key(&me) {
                        self.wait_for(target, me)?;
                    } else {
                        self.lock().node_mut(target).wakers.push(waker.clone());
                        return Ok(std::task::Poll::Pending);
                    }
                }
            }
        }
    }

    fn observe(&self, node: NodeId) -> Result<()> {
        let state = self.lock();
        let record = state.node(node);
        if record.value.is_none() {
            return Err(ComputeError::internal(format!(
                "observation of {node:?} before it produced a value"
            )));
        }
        // A dirty node still has its last value. Recording that value with the
        // certification stamp makes the enclosing commit fail validation and
        // retry; the retry re-evaluates the node. Refusing here would turn an
        // ordinary same-revision invalidation into a hard error.
        frame::record_dep(Dep {
            node,
            version: record.value_version,
            routing: record.routing_at,
            validity: record.verified_validity,
        });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn invocation_node(
        &self,
        component: ComponentId,
        name: &'static str,
        args_hash: u64,
        args: Arc<dyn Any + Send + Sync>,
        scope: ScopeId,
        arity: usize,
        args_eq: &dyn Fn(&dyn Any, &dyn Any) -> bool,
    ) -> (NodeId, Vec<NodeId>) {
        let mut state = self.lock();
        let revision = state.revision;
        if let Some(entries) = state.invocations.get_mut(&(component, args_hash)) {
            for invocation in entries.iter_mut() {
                if under_user_code(|| args_eq(invocation.args.as_ref(), args.as_ref())) {
                    invocation.last_used = revision;
                    return (invocation.body, invocation.outputs.clone());
                }
            }
        }
        self.evict_invocations(&mut state);
        let invocation_id = state.next_invocation + 1;
        state.next_invocation = invocation_id;
        let invocation_scope = state.scopes.child(scope, invocation_id);
        state.scope_components.insert(invocation_scope, component);
        state.component_names.entry(component).or_insert(name);
        let mut body_record = NodeRecord::empty(name, NodeKind::InvocationBody);
        body_record.scope = invocation_scope;
        // Tag the call with its own component: the observation projection
        // attributes a link to the component that made it, not to the caller.
        body_record.component = Some(component);
        let body = state.alloc(body_record);
        state.scope_bodies.insert(invocation_scope, body);
        let mut outputs = Vec::with_capacity(arity);
        for index in 0..arity {
            let mut record = NodeRecord::empty("invocation output", NodeKind::InvocationOutput);
            record.scope = invocation_scope;
            record.site = SiteId(index as u32);
            record.component = Some(component);
            outputs.push(state.alloc(record));
        }
        state
            .invocations
            .entry((component, args_hash))
            .or_default()
            .push(Invocation {
                args,
                scope,
                body,
                outputs: outputs.clone(),
                last_used: revision,
            });
        (body, outputs)
    }
}

impl Inner {
    /// Evicts the least recently used unreferenced invocations above the cap.
    fn evict_invocations(&self, state: &mut State) {
        let Some(cap) = self.config.max_invocations else {
            return;
        };
        loop {
            let total: usize = state.invocations.values().map(Vec::len).sum();
            if total <= cap {
                return;
            }
            let mut candidate: Option<((ComponentId, u64), usize, u64)> = None;
            for (&key, entries) in &state.invocations {
                for (index, invocation) in entries.iter().enumerate() {
                    if !state.node(invocation.body).dependents.is_empty() {
                        continue;
                    }
                    let used = invocation.last_used;
                    if candidate.is_none_or(|(_, _, best)| used < best) {
                        candidate = Some((key, index, used));
                    }
                }
            }
            let Some((key, index, _)) = candidate else {
                return;
            };
            if let Some(entries) = state.invocations.get_mut(&key) {
                if index < entries.len() {
                    entries.remove(index);
                }
                if entries.is_empty() {
                    state.invocations.remove(&key);
                }
            }
            state
                .stats
                .invocations_evicted
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl Inner {
    /// Whether a unit node is a false membership cell (a queried absence).
    fn is_absent_membership(record: &NodeRecord) -> bool {
        record
            .unit
            .as_ref()
            .is_some_and(|unit| unit.kind == UnitKind::Membership)
            && record
                .value
                .as_ref()
                .and_then(|value| value.downcast_ref::<bool>())
                .is_some_and(|present| !*present)
    }

    /// Reclaims nodes that no root, subscription, View unit, or trace can reach.
    ///
    /// Reclamation only loses reuse: a later demand reconstructs the graph.
    /// Nodes that still define derived units are never reclaimed, because their
    /// definitions would otherwise disappear from the region.
    /// Runs `collect` when the configured revision interval has elapsed.
    pub fn maybe_collect(&self) -> Option<ReclaimStats> {
        let interval = self.config.auto_collect_every?;
        if interval == 0 {
            return None;
        }
        let revision = {
            let state = self.lock();
            if state.revision.saturating_sub(state.last_collect) < interval {
                return None;
            }
            state.revision
        };
        let stats = self.reclaim()?;
        self.lock().last_collect = revision;
        self.lock()
            .stats
            .auto_collects
            .fetch_add(1, Ordering::Relaxed);
        Some(stats)
    }

    /// Walks `stack` to a fixpoint, marking every node its traces reach.
    fn seed_traces(
        state: &State,
        stack: &mut Vec<NodeId>,
        reachable: &mut std::collections::HashSet<NodeId>,
    ) {
        while let Some(node) = stack.pop() {
            if !reachable.insert(node) {
                continue;
            }
            for dep in &state.node(node).trace {
                stack.push(dep.node);
            }
        }
    }

    /// Reclaims unreachable nodes, their units, scopes, and invocation records.
    ///
    /// The pass is serialized with demands, so a call made inside one reclaims
    /// nothing.
    pub fn collect(&self) -> ReclaimStats {
        self.reclaim().unwrap_or_default()
    }

    /// Runs one reclamation pass when this thread can exclude demands.
    ///
    /// A demand holds a read permit for its whole task and reuses the nodes it
    /// interned, so the pass takes the write side and waits for every in-flight
    /// demand to finish. A caller that is already inside a demand cannot wait
    /// for the demands it belongs to without waiting for itself, so it skips the
    /// pass and reports none.
    /// Drops the per-scope bookkeeping of scopes that are no longer reachable.
    ///
    /// Every map keyed by `ScopeId` is written for each invocation scope, so each
    /// one has to be pruned here or it grows by an entry per invocation forever.
    /// `scope_components` was written and never pruned.
    fn retain_live_scopes(state: &mut State, live_scopes: &FxHashSet<ScopeId>) {
        state
            .declared_views
            .retain(|scope, _| live_scopes.contains(scope));
        state
            .scope_components
            .retain(|scope, _| live_scopes.contains(scope));
    }

    fn reclaim(&self) -> Option<ReclaimStats> {
        if task::current_in(self.local_key).is_some() {
            return None;
        }
        let _permit = self.gate.write();
        let mut state = self.lock();
        let mut reachable: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
        let mut stack: Vec<NodeId> = Vec::new();
        if let Some(root) = &state.root {
            stack.push(root.body);
            stack.extend(root.outputs.iter().copied());
        }
        // A general component is demanded through its own slot, not through a
        // trace: the root node and the producers it registers are live the
        // moment they are installed.
        stack.extend(state.general_root);
        stack.extend(state.producers.values().copied());
        for region in state.regions.values() {
            stack.push(region.content_node);
            for (_, slot) in region.units.iter() {
                stack.push(slot.node);
            }
        }
        // A subscription records the version of every unit it watches, so the
        // cell that answers for one is part of what it observes. Dropping that
        // cell would leave the recorded version naming a state the unit table no
        // longer answers, and the next staging pass would report a change this
        // pass invented — reclamation only loses reuse.
        for subscription in &state.subscriptions {
            for unit in &subscription.units {
                if let Some(node) = current_unit_node(&state, unit) {
                    stack.push(node);
                }
            }
        }
        for (_, node) in state.units.iter() {
            // An absence-predicate cell is kept alive only by a live trace, so
            // `collect` can drop it with the consumer that queried it.
            if Self::is_absent_membership(state.node(*node)) {
                continue;
            }
            stack.push(*node);
        }
        // A node that still owns definitions is retained whether or not anything
        // reaches it, and so is the trace it will be validated against: dropping
        // those observations would leave `validate_trace` reading a reclaimed
        // identity on the next demand.
        for (index, record) in state.nodes.iter().enumerate() {
            if record.freed || record.definitions.is_empty() {
                continue;
            }
            stack.push(NodeId::new(index as u32, record.generation));
        }
        Self::seed_traces(&state, &mut stack, &mut reachable);
        // An invocation the runtime still serves keeps its projections alive too:
        // they are handed out from the invocation record, so a reclaimed one
        // would be demanded again by an identity that names a freed slot.
        for entries in state.invocations.values() {
            for invocation in entries {
                if !reachable.contains(&invocation.body) {
                    continue;
                }
                stack.push(invocation.body);
                stack.extend(
                    invocation
                        .outputs
                        .iter()
                        .copied()
                        .filter(|node| node_is_live(&state, *node)),
                );
            }
        }
        Self::seed_traces(&state, &mut stack, &mut reachable);

        let nodes_before = state.nodes.len();
        let free: std::collections::HashSet<u32> = state.free_nodes.iter().copied().collect();
        let mut doomed: Vec<NodeId> = Vec::new();
        for (index, record) in state.nodes.iter().enumerate() {
            if free.contains(&(index as u32)) {
                continue;
            }
            let id = NodeId::new(index as u32, record.generation);
            if reachable.contains(&id) {
                continue;
            }
            let absent_membership =
                record.kind == NodeKind::Unit && Self::is_absent_membership(record);
            if matches!(record.kind, NodeKind::Unit | NodeKind::RegionContent) && !absent_membership
            {
                continue;
            }
            if !record.definitions.is_empty() {
                continue;
            }
            if record.is_running() {
                continue;
            }
            doomed.push(id);
        }
        let doomed_set: std::collections::HashSet<NodeId> = doomed.iter().copied().collect();
        for node in &doomed {
            let trace = state.node(*node).trace.clone();
            for dep in trace {
                state.remove_dependent(dep.node, *node);
            }
        }
        for node in &doomed {
            state.release_claims(*node);
            let record = state.node_mut(*node);
            record.value = None;
            record.trace.clear();
            record.state = NodeState::Vacant;
            record.verified_at = 0;
            record.verified_validity = 0;
            record.suspect_validity = 0;
            record.suspect_at = 0;
            record.changed_at = 0;
            record.value_version = 0;
            record.failure = None;
            record.dependents.clear();
            record.freed = true;
        }
        let live_scopes: FxHashSet<ScopeId> = reachable
            .iter()
            .map(|node| state.node(*node).scope)
            .collect();
        Inner::retain_live_scopes(&mut state, &live_scopes);
        let scopes_reclaimed = state.scopes.prune(&live_scopes);
        // Reclaimed slots stay allocated but become reusable; a later allocation
        // advances the slot generation so stale ids are detectable.
        state
            .free_nodes
            .extend(doomed.iter().map(|node| node.index() as u32));
        let doomed_keys: Vec<Arc<dyn AnyKey>> = state
            .units
            .iter()
            .filter(|(_, node)| doomed_set.contains(node))
            .map(|(key, _)| Arc::clone(key))
            .collect();
        let units_reclaimed = doomed_keys.len();
        for key in doomed_keys {
            if let Some(unit) = key.as_any().downcast_ref::<UnitKey>() {
                state.forget_cell(unit);
            }
            state.units.remove(key.as_ref());
        }
        state.sites.retain(|_, node| !doomed_set.contains(node));
        let mut invocations_reclaimed = 0;
        for entries in state.invocations.values_mut() {
            let before = entries.len();
            entries.retain(|invocation| !doomed_set.contains(&invocation.body));
            invocations_reclaimed += before - entries.len();
        }
        state.invocations.retain(|_, entries| !entries.is_empty());
        Some(ReclaimStats {
            nodes_before,
            nodes_after: state.nodes.len(),
            nodes_reclaimed: doomed.len(),
            units_reclaimed,
            invocations_reclaimed,
            scopes_reclaimed,
        })
    }

    /// Reports memory held by runtime metadata and committed values.
    #[must_use]
    pub fn memory_report(&self) -> MemoryReport {
        let state = self.lock();
        let mut value_bytes = 0;
        let mut trace_edges = 0;
        let mut dependent_edges = 0;
        let mut structure_units = 0;
        for node in &state.nodes {
            if let Some(value) = &node.value {
                value_bytes += std::mem::size_of_val(value.as_ref());
            }
            trace_edges += node.trace.len();
            dependent_edges += node.dependents.len();
            if node
                .unit
                .as_ref()
                .is_some_and(|unit| unit.kind == UnitKind::Structure)
            {
                structure_units += 1;
            }
        }
        MemoryReport {
            nodes: state.nodes.len(),
            value_bytes,
            trace_edges,
            dependent_edges,
            sites: state.sites.len(),
            units: state.units.len(),
            invocations: state.invocations.values().map(Vec::len).sum(),
            regions: state.regions.len(),
            scopes: state.scopes.len(),
            tasks: state.tasks.len(),
            structure_units,
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/inner/ops/scope_prune_tests.rs"]
mod scope_prune_tests;
