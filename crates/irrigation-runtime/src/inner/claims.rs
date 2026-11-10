//! Write claims, port plans, and commit-time requirements.
//!
//! Part of the kernel's `impl Inner`; the struct and its lock live in the
//! parent module.

use super::*;

impl Inner {
    /// Claims a writable scope for the current computation.
    ///
    /// The claim is held by the executing node and released by commit, fail,
    /// cancellation, retirement, or a task teardown.
    pub fn claim_write(
        &self,
        scope: WriteScope,
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<()> {
        self.claim_writes(std::slice::from_ref(&scope), label, location)
    }

    /// Announces a group of ports the current computation intends to write.
    pub fn plan_writes(
        &self,
        scopes: &[WriteScope],
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<()> {
        if self.config.write_arbitration == WriteArbitration::Off || scopes.is_empty() {
            return Ok(());
        }
        let frame = frame::current().ok_or(ComputeError::OutsideComputation {
            operation: "write plan",
        })?;
        let task = task::current_or_detached();
        let mut state = self.lock();
        let owner = state
            .invocation_body(frame.scope())
            .unwrap_or_else(|| frame.node());
        match state.plan_group(owner, task, scopes, label, location) {
            Ok(added) => {
                if added > 0 {
                    state
                        .stats
                        .planned_ports
                        .fetch_add(added as u64, Ordering::Relaxed);
                }
                Ok(())
            }
            Err((port, first, second)) => {
                drop(state);
                Err(ComputeError::WriteConflict {
                    port,
                    first,
                    second,
                })
            }
        }
    }

    /// Withdraws planned ports for the current computation.
    pub fn unplan_writes(&self, scopes: &[WriteScope]) -> Result<()> {
        let frame = frame::current().ok_or(ComputeError::OutsideComputation {
            operation: "write plan withdrawal",
        })?;
        let mut state = self.lock();
        let owner = state
            .invocation_body(frame.scope())
            .unwrap_or_else(|| frame.node());
        let removed = state.unplan_group(owner, scopes);
        if removed > 0 {
            state
                .stats
                .unplanned_ports
                .fetch_add(removed as u64, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Claims a group of writable scopes atomically.
    ///
    /// All conflicts and strict declarations are checked before any claim is
    /// registered, so a rejected group has no partial ownership.
    pub fn claim_writes(
        &self,
        scopes: &[WriteScope],
        label: &'static str,
        location: Option<&'static std::panic::Location<'static>>,
    ) -> Result<()> {
        if self.config.write_arbitration == WriteArbitration::Off || scopes.is_empty() {
            return Ok(());
        }
        let frame = frame::current().ok_or(ComputeError::OutsideComputation {
            operation: "write claim",
        })?;
        let owner = frame.node();
        let task = task::current_or_detached();
        let mut state = self.lock();
        if self.config.write_arbitration == WriteArbitration::Strict {
            let component = component_of(&state, frame.scope());
            for scope in scopes {
                let declared = component
                    .is_some_and(|component| planned_for_component(&state, scope, component));
                if !declared {
                    drop(state);
                    return Err(ComputeError::WriteOutOfScope {
                        port: scope.describe(),
                    });
                }
            }
        }
        match state.claim_group(owner, task, scopes, label, location) {
            Ok(claimed) => {
                if claimed > 0 {
                    state
                        .stats
                        .write_claims
                        .fetch_add(claimed as u64, Ordering::Relaxed);
                }
                drop(state);
                #[cfg(feature = "observe")]
                for scope in scopes {
                    self.emit(crate::observe::RuntimeEvent::Arbitration {
                        kind: "write_claim",
                        node: owner,
                        port: scope.describe(),
                    });
                }
                Ok(())
            }
            Err((conflict_port, first, second)) => {
                state.stats.write_conflicts.fetch_add(1, Ordering::Relaxed);
                drop(state);
                #[cfg(feature = "observe")]
                self.emit(crate::observe::RuntimeEvent::Arbitration {
                    kind: "write_conflict",
                    node: owner,
                    port: conflict_port.clone(),
                });
                Err(ComputeError::WriteConflict {
                    port: conflict_port,
                    first,
                    second,
                })
            }
        }
    }

    /// Waits for a live writer claiming one of `ports`.
    pub fn wait_for_port(&self, ports: &[UnitKey]) -> Result<bool> {
        let owner = {
            let state = self.lock();
            ports.iter().find_map(|port| {
                state
                    .claim_covering(port)
                    .filter(|claim| state.node(claim.owner).is_running())
                    .map(|claim| claim.owner)
            })
        };
        let Some(owner) = owner else {
            return Ok(false);
        };
        let me = task::current_or_detached();
        self.wait_for(owner, me)?;
        Ok(true)
    }

    /// Runs host-installed providers for missing ports.
    pub fn provide_ports(&self, ports: &[UnitKey]) -> Result<bool> {
        let providers: Vec<(Arc<dyn crate::runtime::PortProvider>, UnitKey)> = {
            let registered = self.port_providers.lock();
            ports
                .iter()
                .filter_map(|port| {
                    registered
                        .get(&port.view)
                        .cloned()
                        .map(|provider| (provider, port.clone()))
                })
                .collect()
        };
        if providers.is_empty() {
            return Ok(false);
        }
        for (provider, port) in providers {
            provider.provide(&port)?;
        }
        Ok(true)
    }

    /// Records a structural precondition on the current write group.
    ///
    /// The condition is the caller's evaluated predicate about `ports`; the
    /// requirement asserts that those ports are present at commit. A false
    /// condition therefore defers the write group or fails it — a state in
    /// which the ports are absent never satisfies the write that observed the
    /// absence.
    /// Records a requirement that `ports` hold when the group commits.
    ///
    /// `observed` is the caller's own evaluation, from tracked reads it already
    /// made. It is a hint, not a demand: with ports to re-read the runtime
    /// decides at commit, because the world can move between the read and the
    /// publish, so a `false` there means "not satisfied when I looked", not
    /// "refuse". Callers pass it accordingly — several pass `false` about a port
    /// that a provider will supply in the same revision, which is exactly the
    /// deferral case.
    ///
    /// With no ports there is nothing to re-evaluate, so the answer has to be
    /// taken as given and a `false` is a contradiction now.
    /// Records that `ports` must be absent when the group commits.
    ///
    /// This exists so absence and presence go through one owner: a caller that
    /// built the requirement itself skipped the write-arbitration check below,
    /// so an undeclared port could be required absent under `Strict` while the
    /// same port required present was refused.
    pub fn require_absence(&self, ports: &[UnitKey], description: &'static str) -> Result<()> {
        self.check_ports_declared(ports)?;
        if frame::current().is_none() {
            return Err(ComputeError::OutsideComputation {
                operation: "write requirement",
            });
        }
        if ports.is_empty() {
            return Err(ComputeError::PreconditionUnmet {
                condition: description.to_string(),
                ports: Vec::new(),
                reason: irrigation_core::error::PreconditionReason::Contradiction,
            });
        }
        frame::require(Requirement::absent(ports.to_vec(), description));
        Ok(())
    }

    /// Refuses ports the group did not declare, under strict arbitration.
    fn check_ports_declared(&self, ports: &[UnitKey]) -> Result<()> {
        if self.config.write_arbitration != WriteArbitration::Strict {
            return Ok(());
        }
        let state = self.lock();
        for port in ports {
            if !port_declared(&state, port) {
                return Err(ComputeError::WriteOutOfScope {
                    port: format!("{port:?}"),
                });
            }
        }
        Ok(())
    }

    /// Records a requirement that `ports` hold when the group commits.
    ///
    /// `observed` is the caller's own evaluation, from tracked reads it already
    /// made; it is a hint, because with ports to re-read the runtime decides at
    /// commit.
    pub fn require_condition(
        &self,
        observed: bool,
        ports: &[UnitKey],
        description: &'static str,
    ) -> Result<()> {
        self.check_ports_declared(ports)?;
        if frame::current().is_none() {
            return Err(ComputeError::OutsideComputation {
                operation: "write requirement",
            });
        }
        if ports.is_empty() {
            // Nothing can be re-evaluated at commit, so honour the observation
            // now instead of registering a requirement no port can meet.
            if observed {
                return Ok(());
            }
            return Err(ComputeError::PreconditionUnmet {
                condition: description.to_string(),
                ports: Vec::new(),
                reason: irrigation_core::error::PreconditionReason::Contradiction,
            });
        }
        frame::require(Requirement::present(ports.to_vec(), description));
        Ok(())
    }
}
