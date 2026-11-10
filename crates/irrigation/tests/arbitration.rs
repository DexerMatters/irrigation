use irrigation::{
    ChangeBatch, ComponentId, DependencyUpdates, FieldId, GeneralComponent, GeneralCx, Runtime,
    UnitKey, UnitKind, ViewDescriptor, ViewSchema, irrigation, view,
};

#[view(source)]
pub struct Flag {
    on: bool,
}

#[view(derived)]
pub struct Shared(irrigation::Map<u32, u32>);

#[irrigation]
mod read_after_write {
    use super::{Flag, Shared};

    #[component]
    pub fn reader() -> u32 {
        Shared.get(&1).unwrap_or(0)
    }

    #[component]
    pub fn writer() -> u32 {
        Shared.define(1, 42);
        0
    }

    #[component]
    pub fn root(flag: &Flag) -> u32 {
        let first = reader();
        let _ = writer();
        if flag.on() { first + 1 } else { first }
    }
}

#[test]
fn read_after_a_deferred_write_sees_the_write() {
    let runtime = Runtime::new();
    runtime.register_view(<Shared as ViewSchema>::DESCRIPTOR);
    runtime.install(read_after_write::Root).expect("install");

    assert_eq!(
        runtime
            .read_outputs::<read_after_write::Root>()
            .expect("read"),
        42
    );
    runtime
        .edit(|edit| edit.view::<Flag>().set_on(true))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<read_after_write::Root>()
            .expect("read"),
        43
    );

    let snapshot = runtime.read().expect("snapshot");
    assert_eq!(snapshot.view::<Shared>().field0(), vec![(1, 42)]);
    let stats = runtime.stats();
    assert!(stats.write_claims > 0, "the writer claims its port");
    assert!(stats.demand_retries > 0, "the stale read forces a retry");
}

#[view(derived)]
pub struct Disjoint(irrigation::Map<u32, u32>);

#[irrigation]
mod disjoint_writers {
    use super::Disjoint;

    #[component]
    pub fn first() -> u32 {
        Disjoint.define(1, 10);
        0
    }

    #[component]
    pub fn second() -> u32 {
        Disjoint.define(2, 20);
        0
    }

    #[component]
    pub fn reader() -> u32 {
        Disjoint.get(&1).unwrap_or(0) + Disjoint.get(&2).unwrap_or(0)
    }

    #[component]
    pub fn root() -> u32 {
        let _ = first();
        let _ = second();
        reader()
    }
}

#[test]
fn disjoint_writers_compose_in_any_order() {
    let runtime = Runtime::new();
    runtime.register_view(<Disjoint as ViewSchema>::DESCRIPTOR);
    runtime.install(disjoint_writers::Root).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<disjoint_writers::Root>()
            .expect("read"),
        30
    );
    let snapshot = runtime.read().expect("snapshot");
    assert_eq!(snapshot.view::<Disjoint>().field0(), vec![(1, 10), (2, 20)]);
}

#[view(derived)]
pub struct Contended(irrigation::Map<u32, u32>);

#[irrigation]
mod contended_writers {
    use super::Contended;

    #[component]
    pub fn first() -> u32 {
        Contended.define(1, 10);
        0
    }

    #[component]
    pub fn second() -> u32 {
        Contended.define(1, 20);
        0
    }

    #[component]
    pub fn root() -> u32 {
        let _ = first();
        let _ = second();
        0
    }
}

#[test]
fn the_same_port_is_rejected() {
    let runtime = Runtime::new();
    runtime.register_view(<Contended as ViewSchema>::DESCRIPTOR);
    runtime.install(contended_writers::Root).expect("install");
    let error = runtime
        .read_outputs::<contended_writers::Root>()
        .expect_err("the same port must not be defined twice");
    assert!(
        matches!(error, irrigation::ComputeError::DuplicateDefinition { .. }),
        "a sequential second define is the documented duplicate-definition error, got {error:?}"
    );
}

#[view(derived)]
pub struct GraphView(irrigation::Graph<u32, u32>);

#[irrigation]
mod graph_order {
    use super::GraphView;

    #[component]
    pub fn edge_first() -> u32 {
        GraphView.field0().define_edge(10, 1, 2, 5);
        0
    }

    #[component]
    pub fn nodes_later() -> u32 {
        GraphView.field0().define_node(1, 10);
        GraphView.field0().define_node(2, 20);
        0
    }

    #[component]
    pub fn root() -> (Vec<u64>, Vec<u64>) {
        let _ = edge_first();
        let _ = nodes_later();
        (GraphView.nodes(), GraphView.edges())
    }
}

#[test]
fn a_graph_edge_deferred_before_its_nodes_is_installed() {
    let runtime = Runtime::new();
    runtime.register_view(<GraphView as ViewSchema>::DESCRIPTOR);
    runtime.install(graph_order::Root).expect("install");
    let (nodes, edges) = runtime.read_outputs::<graph_order::Root>().expect("read");
    assert_eq!(nodes, vec![1, 2]);
    assert_eq!(edges, vec![10]);
}

#[irrigation]
mod provider_graph {
    use super::GraphView;

    #[component]
    pub fn write_nodes(_tag: u32) -> u32 {
        GraphView.field0().define_node(1, 10);
        GraphView.field0().define_node(2, 20);
        0
    }
}

#[irrigation]
mod provider_root {
    use super::GraphView;

    #[component]
    pub fn edge_only() -> (Vec<u64>, Vec<u64>) {
        GraphView.field0().define_edge(10, 1, 2, 5);
        (GraphView.nodes(), GraphView.edges())
    }
}

struct NodeProvider(irrigation::Runtime);

impl irrigation::PortProvider for NodeProvider {
    fn provide(&self, _port: &irrigation::UnitKey) -> irrigation::Result<()> {
        self.0
            .call::<provider_graph::__WriteNodesSpec>(provider_graph::__WriteNodesArgs {
                _tag: 0,
            })?;
        Ok(())
    }
}

#[test]
fn a_port_provider_supplies_graph_endpoints() {
    use irrigation::ViewSchema;

    let runtime = Runtime::new();
    runtime.register_view(<GraphView as ViewSchema>::DESCRIPTOR);
    runtime.install_port_provider(
        GraphView::view_id(),
        std::sync::Arc::new(NodeProvider(runtime.clone())),
    );
    runtime.install(provider_root::EdgeOnly).expect("install");
    let (nodes, edges) = runtime
        .read_outputs::<provider_root::EdgeOnly>()
        .expect("read");
    assert_eq!(nodes, vec![1, 2]);
    assert_eq!(edges, vec![10]);
}

#[view(derived)]
pub struct CheckedMap(irrigation::Map<u32, u32>);

#[irrigation]
mod map_checks {
    use super::CheckedMap;

    #[component]
    pub fn insert(key: u32, value: u32) -> u32 {
        CheckedMap.field0().insert(key, value, "map_insert");
        0
    }

    #[component]
    pub fn update_if_present(key: u32, value: u32) -> u32 {
        CheckedMap
            .field0()
            .update_if_present(key, |_| value, "map_update");
        0
    }

    #[component]
    pub fn duplicate_insert_root() -> u32 {
        let _ = insert(1, 10);
        let _ = insert(1, 20);
        0
    }

    #[component]
    pub fn absent_update_root() -> u32 {
        update_if_present(1, 20)
    }
}

#[test]
fn checked_map_inserts_reject_an_existing_key() {
    let runtime = Runtime::new();
    runtime.register_view(<CheckedMap as ViewSchema>::DESCRIPTOR);
    runtime
        .install(map_checks::DuplicateInsertRoot)
        .expect("install");
    let error = runtime
        .read_outputs::<map_checks::DuplicateInsertRoot>()
        .expect_err("a duplicate insert must fail");
    assert!(
        matches!(error, irrigation::ComputeError::PreconditionUnmet { .. }),
        "the checked insert fails at its precondition, got {error:?}"
    );
}

#[test]
fn checked_map_update_if_present_is_a_noop_when_absent() {
    let runtime = Runtime::new();
    runtime.register_view(<CheckedMap as ViewSchema>::DESCRIPTOR);
    runtime
        .install(map_checks::AbsentUpdateRoot)
        .expect("install");
    assert_eq!(
        runtime
            .read_outputs::<map_checks::AbsentUpdateRoot>()
            .expect("read"),
        0
    );
    let snapshot = runtime.read().expect("snapshot");
    assert!(snapshot.view::<CheckedMap>().field0().is_empty());
}

#[view(source)]
pub struct ListMode {
    mode: u32,
}

#[view(derived)]
pub struct CheckedList(irrigation::List<u32>);

#[irrigation]
mod list_checks {
    use super::{CheckedList, ListMode};

    #[component]
    pub fn root(mode: &ListMode) -> Vec<u32> {
        match mode.mode() {
            0 => CheckedList.field0().define_all([1u32, 2], "list_build"),
            1 => CheckedList.field0().set_at(0, 9, "list_set"),
            2 => CheckedList.field0().insert_at(2, 3, "list_insert"),
            _ => CheckedList.field0().set_at(99, 0, "list_bad"),
        }
        CheckedList.to_vec()
    }
}

#[test]
fn checked_list_operations_read_the_current_sequence() {
    use irrigation::ViewSchema;

    let runtime = Runtime::new();
    runtime.register_view(<CheckedList as ViewSchema>::DESCRIPTOR);
    runtime.install(list_checks::Root).expect("install");
    assert_eq!(
        runtime.read_outputs::<list_checks::Root>().expect("read"),
        vec![1, 2]
    );
    runtime
        .edit(|edit| edit.view::<ListMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<list_checks::Root>().expect("read"),
        vec![9, 2]
    );
    runtime
        .edit(|edit| edit.view::<ListMode>().set_mode(3))
        .expect("edit");
    let error = runtime
        .read_outputs::<list_checks::Root>()
        .expect_err("an out-of-range set must fail");
    assert!(matches!(
        error,
        irrigation::ComputeError::PreconditionUnmet { .. }
    ));
}

#[view(source)]
pub struct GuardFlag {
    on: bool,
}

#[irrigation]
mod value_guard {
    use super::GuardFlag;

    #[component]
    pub fn root(flag: &GuardFlag) -> u32 {
        irrigation::require_value(flag.on(), &[], "guard flag must be on");
        1
    }
}

#[test]
fn a_false_value_requirement_fails_the_demand() {
    let runtime = Runtime::new();
    runtime.install(value_guard::Root).expect("install");
    let error = runtime
        .read_outputs::<value_guard::Root>()
        .expect_err("the guard must fail");
    assert!(matches!(
        error,
        irrigation::ComputeError::PreconditionUnmet { .. }
    ));
    runtime
        .edit(|edit| edit.view::<GuardFlag>().set_on(true))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<value_guard::Root>().expect("read"),
        1
    );
}

#[view(source)]
pub struct GraphMode {
    mode: u32,
}

#[view(derived)]
pub struct RemovableGraph(irrigation::Graph<u32, u32>);

#[irrigation]
mod graph_removal {
    use super::{GraphMode, RemovableGraph};

    #[component]
    pub fn root(mode: &GraphMode) -> (Vec<u64>, Vec<u64>) {
        match mode.mode() {
            0 => RemovableGraph.field0().define_graph(
                &[(1u64, 10u32), (2, 20)],
                &[(10u64, 1, 2, 5u32)],
                "graph",
            ),
            1 => RemovableGraph.field0().remove_node(1, "remove_1"),
            _ => RemovableGraph.field0().remove_node_if_empty(2, "remove_2"),
        }
        (RemovableGraph.nodes(), RemovableGraph.edges())
    }
}

#[test]
fn graph_node_removal_retracts_incident_edges() {
    use irrigation::ViewSchema;

    let runtime = Runtime::new();
    runtime.register_view(<RemovableGraph as ViewSchema>::DESCRIPTOR);
    runtime.install(graph_removal::Root).expect("install");
    assert_eq!(
        runtime.read_outputs::<graph_removal::Root>().expect("read"),
        (vec![1, 2], vec![10])
    );
    runtime
        .edit(|edit| edit.view::<GraphMode>().set_mode(1))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<graph_removal::Root>().expect("read"),
        (vec![2], vec![])
    );
    runtime
        .edit(|edit| edit.view::<GraphMode>().set_mode(2))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<graph_removal::Root>().expect("read"),
        (vec![], vec![])
    );
}

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::Duration;

static WAIT_READY: OnceLock<Mutex<mpsc::Sender<()>>> = OnceLock::new();
static WAIT_RELEASE: OnceLock<Mutex<mpsc::Receiver<()>>> = OnceLock::new();
static WAIT_READER_STARTED: OnceLock<Mutex<mpsc::Sender<()>>> = OnceLock::new();

fn signal_writer_ready() {
    if let Some(ready) = WAIT_READY.get() {
        let _ = ready.lock().expect("ready").send(());
    }
}

fn wait_for_release() {
    if let Some(release) = WAIT_RELEASE.get() {
        let _ = release.lock().expect("release").recv();
    }
}

fn signal_reader_started() {
    if let Some(started) = WAIT_READER_STARTED.get() {
        let _ = started.lock().expect("started").send(());
    }
}

#[view(derived)]
pub struct Waited(irrigation::Map<u32, u32>);

/// Defines the unit and blocks *before returning* from the evaluation node that
/// owns the claim. If the component called `define` in one expression and
/// blocked in another, the definition's site would commit (and release the
/// claim) before the block.
fn write_and_block() {
    Waited.define(1, 42);
    signal_writer_ready();
    wait_for_release();
}

#[irrigation]
mod wait_claim {
    use super::{Waited, signal_reader_started, write_and_block};

    #[component]
    pub fn slow_writer(_tag: u32) -> u32 {
        write_and_block();
        0
    }

    #[component]
    pub fn reader_root() -> u32 {
        signal_reader_started();
        Waited.get(&1).unwrap_or(0)
    }
}

/// Set once the first live writer holds its claim.
static LIVE_CLAIM_HELD: AtomicBool = AtomicBool::new(false);
/// Releases the first live writer after the second demand saw the conflict.
static LIVE_CLAIM_RELEASE: AtomicBool = AtomicBool::new(false);

fn hold_live_claim() {
    LIVE_CLAIM_HELD.store(true, Ordering::Release);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !LIVE_CLAIM_RELEASE.load(Ordering::Acquire) {
        assert!(
            std::time::Instant::now() < deadline,
            "the live claim was never released"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The contended-port counterpart of [`write_and_block`]: the block shares the
/// evaluation node that owns the definition, so the claim stays live.
fn define_and_hold() {
    Contended.define(1, 10);
    hold_live_claim();
}

#[irrigation]
mod live_contention {
    use super::Contended;

    /// Defines the port and then holds the evaluation node that owns the claim
    /// open, so a second writer meets a live claim.
    #[component]
    pub fn first(_tag: u32) -> u32 {
        super::define_and_hold();
        0
    }

    #[component]
    pub fn second(_tag: u32) -> u32 {
        Contended.define(1, 20);
        0
    }
}

#[test]
fn two_live_writers_on_one_port_conflict() {
    use irrigation::ViewSchema;

    LIVE_CLAIM_HELD.store(false, Ordering::Release);
    LIVE_CLAIM_RELEASE.store(false, Ordering::Release);
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 2,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.register_view(<Contended as ViewSchema>::DESCRIPTOR);

    let first_runtime = runtime.clone();
    let first = std::thread::spawn(move || {
        first_runtime.call::<live_contention::__FirstSpec>(live_contention::__FirstArgs { _tag: 0 })
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !LIVE_CLAIM_HELD.load(Ordering::Acquire) {
        assert!(
            std::time::Instant::now() < deadline,
            "the first writer never claimed its port"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let second =
        runtime.call::<live_contention::__SecondSpec>(live_contention::__SecondArgs { _tag: 0 });
    LIVE_CLAIM_RELEASE.store(true, Ordering::Release);
    let first = first.join().expect("first thread");
    match second {
        Err(irrigation::ComputeError::WriteConflict {
            port,
            first: first_provider,
            second: second_provider,
        }) => {
            assert!(!port.is_empty(), "the conflict names its port");
            assert_ne!(
                first_provider, second_provider,
                "the conflict names both live providers"
            );
        }
        other => panic!("expected a live write conflict, got {other:?}"),
    }
    assert!(first.is_ok(), "the first writer must publish");
    assert_eq!(
        runtime.stats().write_conflicts,
        1,
        "the live claim check fired"
    );
}

#[test]
fn a_reader_waits_for_a_live_writer_claim() {
    use irrigation::ViewSchema;

    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let _ = WAIT_READY.set(Mutex::new(ready_tx));
    let _ = WAIT_RELEASE.set(Mutex::new(release_rx));
    let _ = WAIT_READER_STARTED.set(Mutex::new(started_tx));

    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 2,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.register_view(<Waited as ViewSchema>::DESCRIPTOR);
    runtime.install(wait_claim::ReaderRoot).expect("install");

    let writer_runtime = runtime.clone();
    let writer = std::thread::spawn(move || {
        writer_runtime
            .call::<wait_claim::__SlowWriterSpec>(wait_claim::__SlowWriterArgs { _tag: 0 })
    });
    ready_rx.recv().expect("writer ready");

    let reader_runtime = runtime.clone();
    let reader =
        std::thread::spawn(move || reader_runtime.read_outputs::<wait_claim::ReaderRoot>());
    started_rx.recv().expect("reader started");
    // The signal only says the body started; wait until the reader is actually
    // registered on the live claim, so a descheduled reader cannot let the
    // writer retire first and turn a correct wait into a spurious failure.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while runtime.stats().reader_waits == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the reader never registered on the live claim"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    release_tx.send(()).expect("release");

    assert_eq!(reader.join().expect("reader thread").expect("read"), 42);
    assert!(writer.join().expect("writer thread").is_ok());
    assert!(
        runtime.stats().reader_waits > 0,
        "the reader must suspend on the live claim"
    );
}

#[test]
fn arbitration_can_be_disabled_for_migration() {
    use irrigation::WriteArbitration;

    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        write_arbitration: WriteArbitration::Off,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.register_view(<Disjoint as ViewSchema>::DESCRIPTOR);
    runtime.install(disjoint_writers::Root).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<disjoint_writers::Root>()
            .expect("read"),
        30
    );
    assert_eq!(
        runtime.stats().write_claims,
        0,
        "Off mode must not claim ports"
    );
}

#[view(derived)]
pub struct StrictValue(u32);

#[irrigation]
mod strict_mode {
    use super::StrictValue;

    #[component]
    pub fn root() -> u32 {
        StrictValue.define_field0(7);
        StrictValue.field0()
    }
}

#[test]
fn strict_mode_requires_a_declared_write() {
    use irrigation::{FieldId, RootComponent, UnitKey, UnitKind, ViewSchema, WriteArbitration};

    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        write_arbitration: WriteArbitration::Strict,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.register_view(<StrictValue as ViewSchema>::DESCRIPTOR);
    runtime.install(strict_mode::Root).expect("install");
    let error = runtime
        .read_outputs::<strict_mode::Root>()
        .expect_err("an undeclared write must be rejected");
    assert!(matches!(
        error,
        irrigation::ComputeError::WriteOutOfScope { .. }
    ));

    let planned = Runtime::with_config(irrigation::RuntimeConfig {
        write_arbitration: WriteArbitration::Strict,
        ..irrigation::RuntimeConfig::default()
    });
    planned.register_view(<StrictValue as ViewSchema>::DESCRIPTOR);
    planned.plan_route([(
        UnitKey::field(StrictValue::view_id(), FieldId(0), UnitKind::Value),
        <strict_mode::Root as RootComponent>::ID,
    )]);
    planned.install(strict_mode::Root).expect("install");
    assert_eq!(
        planned.read_outputs::<strict_mode::Root>().expect("read"),
        7
    );
}

#[irrigation]
mod dangling_edge {
    use super::GraphView;

    #[component]
    pub fn edge_only() -> (Vec<u64>, Vec<u64>) {
        GraphView.field0().define_edge(10, 1, 2, 5);
        (GraphView.nodes(), GraphView.edges())
    }
}

#[test]
fn a_graph_edge_without_its_endpoints_never_publishes() {
    let runtime = Runtime::new();
    runtime.register_view(<GraphView as ViewSchema>::DESCRIPTOR);
    runtime.install(dangling_edge::EdgeOnly).expect("install");
    let error = runtime
        .read_outputs::<dangling_edge::EdgeOnly>()
        .expect_err("an edge whose endpoints do not exist must not publish");
    assert!(
        matches!(error, irrigation::ComputeError::PreconditionUnmet { .. }),
        "expected the structural precondition to fail, got {error:?}"
    );
    // The group was drained out of the pending set and dropped. Its owner is
    // certified for the revision, but the write it staged never landed: a
    // second demand must fail the same way instead of serving that owner.
    let repeated = runtime
        .read_outputs::<dangling_edge::EdgeOnly>()
        .expect_err("a dropped write group is a failed revision, not a cached success");
    assert!(
        matches!(repeated, irrigation::ComputeError::PreconditionUnmet { .. }),
        "expected the same structural precondition to fail again, got {repeated:?}"
    );
    let snapshot = runtime.inspect_snapshot();
    assert!(
        snapshot.view::<GraphView>().field0_edges().is_empty(),
        "the edge was published without its endpoints"
    );
}

#[view(derived)]
pub struct RejectedMap(irrigation::Map<u32, u32>);

fn rejected_value_unit(key: u32) -> UnitKey {
    UnitKey::entity(
        <RejectedMap as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
        irrigation::any_key(key),
    )
}

fn rejected_membership_unit(key: u32) -> UnitKey {
    UnitKey::entity(
        <RejectedMap as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Membership,
        irrigation::any_key(key),
    )
}

#[irrigation]
mod rejected_group {
    use super::{RejectedMap, rejected_value_unit};

    /// Stages one group whose requirement only holds at the end of the demand
    /// and whose drafts duplicate one unit. The drain's validation is the first
    /// code that can see the duplicate.
    fn stage_duplicate_group() {
        use irrigation::ViewSchema;

        irrigation::ensure_view(<RejectedMap as ViewSchema>::DESCRIPTOR);
        irrigation::require_condition(
            false,
            &[super::rejected_membership_unit(7)],
            "late port present",
        );
        irrigation::define_unit(rejected_value_unit(1), 10u64, "first");
        irrigation::define_unit(rejected_value_unit(1), 20u64, "second");
    }

    #[component]
    pub fn write(tag: u64) -> u32 {
        let _ = tag;
        stage_duplicate_group();
        0
    }

    #[component]
    pub fn provide(tag: u64) -> u32 {
        let _ = tag;
        RejectedMap.define(7, 1);
        0
    }

    #[component]
    pub fn root() -> u32 {
        let _ = write(0);
        let _ = provide(0);
        0
    }
}

#[test]
fn a_group_the_drain_rejects_stays_failed_for_the_revision() {
    let runtime = Runtime::new();
    runtime.register_view(<RejectedMap as ViewSchema>::DESCRIPTOR);
    runtime.install(rejected_group::Root).expect("install");
    let error = runtime
        .read_outputs::<rejected_group::Root>()
        .expect_err("the drain must reject the duplicated unit");
    assert!(
        matches!(error, irrigation::ComputeError::DuplicateDefinition { .. }),
        "expected the drain to reject the group, got {error:?}"
    );
    // The group is gone and its owner still holds the value it committed before
    // the deferral; replaying the owner must surface the loss again.
    let repeated = runtime
        .call::<rejected_group::__WriteSpec>(rejected_group::__WriteArgs { tag: 0 })
        .expect_err("a rejected write group must stay failed for the revision");
    assert!(
        matches!(
            repeated,
            irrigation::ComputeError::DuplicateDefinition { .. }
        ),
        "expected the rejection to be sticky, got {repeated:?}"
    );
    let entries = runtime.inspect_snapshot().view::<RejectedMap>().field0();
    assert!(
        entries.iter().all(|(key, _)| *key != 1),
        "the rejected group published its definition: {entries:?}"
    );
}

#[irrigation]
mod empty_port_requirement {
    use super::GuardFlag;

    #[component]
    pub fn root(flag: &GuardFlag) -> u32 {
        irrigation::require_condition(flag.on(), &[], "empty port condition");
        1
    }
}

#[test]
fn an_empty_port_requirement_honours_its_condition() {
    let runtime = Runtime::new();
    runtime
        .install(empty_port_requirement::Root)
        .expect("install");
    let error = runtime
        .read_outputs::<empty_port_requirement::Root>()
        .expect_err("a false condition with no ports must fail");
    assert!(
        matches!(error, irrigation::ComputeError::PreconditionUnmet { .. }),
        "an empty-port requirement must not be vacuously satisfied, got {error:?}"
    );
    runtime
        .edit(|edit| edit.view::<GuardFlag>().set_on(true))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<empty_port_requirement::Root>()
            .expect("read"),
        1
    );
}

#[view(source)]
pub struct SeedFlag {
    on: bool,
}

#[view(source)]
pub struct PruneFlag {
    on: bool,
}

#[view(derived)]
pub struct RegistryMap(irrigation::Map<u32, u32>);

#[irrigation]
mod nested_retraction {
    use super::{PruneFlag, RegistryMap, SeedFlag};

    /// Seeds the derived map once. It never observes the units it defines, so
    /// a later revision keeps them without re-running this body.
    #[component]
    pub fn seed(flag: &SeedFlag) -> u32 {
        let on = flag.on();
        (0u32..2).for_each(|key| {
            if on {
                RegistryMap.field0().define(key, key * 10, "seed");
            }
        });
        0
    }

    /// A root-style component invoked as a plain function: the macro wraps its
    /// body in a per-iteration scope frame, and the retraction runs inside a
    /// transparent iterator adapter, so it is staged in that scope frame.
    #[component]
    pub fn prune(flag: &PruneFlag) -> u32 {
        let on = flag.on();
        (1u32..2).for_each(|key| {
            if on {
                RegistryMap.field0().remove_if_present(key, "prune");
            }
        });
        0
    }

    #[component]
    pub fn root(seed_flag: &SeedFlag, prune_flag: &PruneFlag) -> Vec<(u32, u32)> {
        let _ = seed(seed_flag);
        let _ = prune(prune_flag);
        RegistryMap.field0().to_vec()
    }
}

#[test]
fn a_nested_root_component_retracts_a_derived_unit() {
    let runtime = Runtime::new();
    runtime.register_view(<RegistryMap as ViewSchema>::DESCRIPTOR);
    runtime.install(nested_retraction::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<SeedFlag>().set_on(true))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<nested_retraction::Root>()
            .expect("read"),
        vec![(0, 0), (1, 10)]
    );
    runtime
        .edit(|edit| edit.view::<PruneFlag>().set_on(true))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<nested_retraction::Root>()
            .expect("read"),
        vec![(0, 0)],
        "a retraction staged inside a per-iteration scope was lost"
    );
}

#[view(source)]
pub struct ProducerInput {
    value: u64,
}

#[view(derived)]
pub struct HousekeepingOutput {
    value: u64,
}

/// A general component that publishes one output View. Consumers observe its
/// units, so a stale one is certified by housekeeping demand.
struct HousekeepingProducer;

impl GeneralComponent for HousekeepingProducer {
    type State = u64;
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C0_0101);
    const NAME: &'static str = "arbitration::HousekeepingProducer";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<ProducerInput as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] =
        &[<HousekeepingOutput as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(UnitKey::field(
            <ProducerInput as ViewSchema>::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        cx.output().field(
            <HousekeepingOutput as ViewSchema>::view_id(),
            FieldId(0),
            value,
        );
        Ok((value, ()))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[irrigation]
mod housekeeping {
    use super::HousekeepingOutput;

    #[component]
    pub fn reader() -> u64 {
        HousekeepingOutput.value()
    }

    #[component]
    pub fn root() -> u64 {
        let value = reader();
        value
    }
}

fn traces_observing(runtime: &Runtime, node: irrigation::NodeId) -> usize {
    runtime
        .dump()
        .into_iter()
        .filter(|record| record.deps.iter().any(|dep| dep.node == node))
        .count()
}

#[test]
fn a_housekeeping_producer_stays_out_of_consumer_traces() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(HousekeepingProducer)
        .expect("producer");
    runtime.install(housekeeping::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<ProducerInput>().set_value(7))
        .expect("seed");
    assert_eq!(
        runtime.read_outputs::<housekeeping::Root>().expect("read"),
        7
    );

    let producer = runtime
        .dump()
        .into_iter()
        .find(|record| record.label == "general component producer")
        .expect("the general producer node")
        .id;
    let before = traces_observing(&runtime, producer);
    assert!(
        before > 0,
        "the direct reader must observe its producer once"
    );

    runtime
        .edit(|edit| edit.view::<ProducerInput>().set_value(11))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs::<housekeeping::Root>().expect("read"),
        11
    );

    assert_eq!(
        traces_observing(&runtime, producer),
        before,
        "a housekeeping producer demand became a consumer dependency"
    );
}

#[view(source)]
pub struct Network {
    links: irrigation::Graph<u32, u32>,
}

#[test]
fn a_transaction_that_returns_to_its_committed_graph_publishes_nothing() {
    let runtime = Runtime::new();
    runtime.register_view(<Network as ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Network>();
            let mut links = view.links();
            links.add_node(1, 10u32);
            links.add_node(2, 20u32);
        })
        .expect("seed");
    let revision = runtime.revision();

    // The edge never reaches committed content: the transaction ends exactly
    // where it started, so it has no write to publish.
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Network>();
            let mut links = view.links();
            let edge = links.add_edge(1, 2, 5u32);
            links.remove_edge(edge);
        })
        .expect("edit");
    assert_eq!(
        runtime.revision(),
        revision,
        "a transaction whose final graph equals the committed graph published a revision"
    );
    assert!(
        runtime
            .snapshot()
            .view::<Network>()
            .links_edges()
            .is_empty()
    );
}

#[view(derived)]
pub struct Disputed(u64);

#[irrigation]
mod disputed_call {
    use super::Disputed;

    #[component]
    pub fn first(tag: u64) -> u64 {
        let _ = tag;
        Disputed.define_field0(1);
        1
    }

    #[component]
    pub fn second(tag: u64) -> u64 {
        let _ = tag;
        Disputed.define_field0(2);
        2
    }
}

#[irrigation]
mod disputed_reader {
    use super::Disputed;

    #[component]
    pub fn read(tag: u64) -> u64 {
        let _ = tag;
        Disputed.field0()
    }
}

#[test]
fn a_rejected_duplicate_leaves_the_committed_definition_in_place() {
    let runtime = Runtime::new();
    assert_eq!(
        runtime
            .call::<disputed_call::__FirstSpec>(disputed_call::__FirstArgs { tag: 0 })
            .expect("first"),
        1
    );
    assert_eq!(runtime.snapshot().view::<Disputed>().field0(), 1);

    let error = runtime
        .call::<disputed_call::__SecondSpec>(disputed_call::__SecondArgs { tag: 0 })
        .expect_err("the second provider is a duplicate");
    assert!(
        matches!(error, irrigation::ComputeError::DuplicateDefinition { .. }),
        "expected a duplicate-definition error, got {error:?}"
    );
    assert_eq!(
        runtime.snapshot().view::<Disputed>().field0(),
        1,
        "the rejected duplicate dropped the committed definition"
    );
    assert_eq!(
        runtime
            .call::<disputed_reader::__ReadSpec>(disputed_reader::__ReadArgs { tag: 0 })
            .expect("the winner's unit must stay readable"),
        1
    );
}

struct PreferFirst {
    winner: std::sync::Mutex<Option<irrigation::NodeId>>,
}

impl irrigation::RouteSelector for PreferFirst {
    fn select(
        &self,
        _unit: &irrigation::UnitKey,
        first: irrigation::NodeId,
        _second: irrigation::NodeId,
    ) -> irrigation::NodeId {
        *self
            .winner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert(first)
    }
}

static LATE_SELECTOR: std::sync::Mutex<Option<Runtime>> = std::sync::Mutex::new(None);

/// Installs the selector after the late provider committed, so the demand ends
/// with a staged conflict for the selector to resolve.
fn install_late_selector() {
    let runtime = LATE_SELECTOR
        .lock()
        .expect("late selector")
        .take()
        .expect("runtime");
    runtime.set_route_selector(std::sync::Arc::new(PreferFirst {
        winner: std::sync::Mutex::new(None),
    }));
}

#[irrigation]
mod late_selector {
    use super::{Disputed, install_late_selector};

    #[component]
    pub fn second(tag: u64) -> u64 {
        let _ = tag;
        Disputed.define_field0(2);
        2
    }

    #[component]
    pub fn drive(tag: u64) -> u64 {
        let _ = tag;
        let _ = second(0);
        install_late_selector();
        0
    }
}

#[test]
fn restoring_the_selected_route_keeps_the_content_unit_current() {
    let runtime = Runtime::new();
    assert_eq!(
        runtime
            .call::<disputed_call::__FirstSpec>(disputed_call::__FirstArgs { tag: 0 })
            .expect("first"),
        1
    );
    *LATE_SELECTOR.lock().expect("late selector") = Some(runtime.clone());
    let _ = runtime
        .call::<late_selector::__DriveSpec>(late_selector::__DriveArgs { tag: 0 })
        .expect("root");
    assert_eq!(
        runtime.snapshot().view::<Disputed>().field0(),
        1,
        "the selector keeps the earlier provider"
    );
    let restored = runtime.snapshot().version::<Disputed>();

    // The restore must commit through the content unit, not only through
    // `region.content`: the winner re-installs its identical value at the next
    // revision, and an unchanged View keeps its version.
    runtime
        .edit(|edit| edit.view::<GuardFlag>().set_on(true))
        .expect("unrelated edit");
    let _ = runtime
        .call::<disputed_call::__FirstSpec>(disputed_call::__FirstArgs { tag: 0 })
        .expect("first again");
    assert_eq!(runtime.snapshot().view::<Disputed>().field0(), 1);
    assert_eq!(
        runtime.snapshot().version::<Disputed>(),
        restored,
        "the restored content node disagrees with the View content"
    );
}

#[view(derived)]
pub struct DisputedMap(irrigation::Map<u32, u32>);

#[irrigation]
mod disputed_map {
    use super::DisputedMap;

    #[component]
    pub fn first(tag: u64) -> u32 {
        let _ = tag;
        DisputedMap.define(1, 10);
        1
    }

    #[component]
    pub fn second(tag: u64) -> u32 {
        let _ = tag;
        DisputedMap.define(1, 20);
        2
    }
}

#[irrigation]
mod disputed_map_reader {
    use super::DisputedMap;

    #[component]
    pub fn read(tag: u64) -> (Vec<u32>, Option<u32>) {
        let _ = tag;
        let field = DisputedMap.field0();
        (field.keys(), field.get(&1))
    }
}

#[test]
fn a_restored_map_entry_agrees_with_the_structure_unit() {
    let runtime = Runtime::new();
    assert_eq!(
        runtime
            .call::<disputed_map::__FirstSpec>(disputed_map::__FirstArgs { tag: 0 })
            .expect("first"),
        1
    );
    assert_eq!(
        runtime.snapshot().view::<DisputedMap>().field0(),
        vec![(1, 10)]
    );

    let error = runtime
        .call::<disputed_map::__SecondSpec>(disputed_map::__SecondArgs { tag: 0 })
        .expect_err("the second provider is a duplicate");
    assert!(
        matches!(error, irrigation::ComputeError::DuplicateDefinition { .. }),
        "expected a duplicate-definition error, got {error:?}"
    );

    // A from-scratch evaluation of the winning provider alone: one entry, one
    // key. The restored View has to serve exactly that, entry and topology.
    let fresh = Runtime::new();
    fresh
        .call::<disputed_map::__FirstSpec>(disputed_map::__FirstArgs { tag: 0 })
        .expect("fresh first");
    let expected = fresh
        .call::<disputed_map_reader::__ReadSpec>(disputed_map_reader::__ReadArgs { tag: 0 })
        .expect("fresh read");

    let restored = runtime
        .call::<disputed_map_reader::__ReadSpec>(disputed_map_reader::__ReadArgs { tag: 0 })
        .expect("read");
    assert_eq!(
        restored, expected,
        "the restored entry disagrees with the recomputed structure"
    );
    assert_eq!(
        runtime.snapshot().view::<DisputedMap>().field0(),
        vec![(1, 10)],
        "the restored content lost the winning entry"
    );
}

// ---------------------------------------------------------------------------
// Reclamation is serialized with demands (runtime-kernel-06)
// ---------------------------------------------------------------------------

static RECLAIM_STARTED: AtomicBool = AtomicBool::new(false);
static RECLAIM_RELEASE: AtomicBool = AtomicBool::new(false);

/// Keeps a demand in flight while the attempt owns a write claim.
fn hold_reclaim() {
    RECLAIM_STARTED.store(true, Ordering::Release);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !RECLAIM_RELEASE.load(Ordering::Acquire) {
        assert!(
            std::time::Instant::now() < deadline,
            "the reclaim gate was never released"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[irrigation]
mod reclaim_gate {
    use super::{Contended, hold_reclaim};

    #[component]
    pub fn hold(_tag: u64) -> u32 {
        Contended.define(1, 10);
        hold_reclaim();
        0
    }
}

#[test]
fn a_reclaim_pass_waits_for_the_demands_it_would_free_under() {
    use irrigation::ViewSchema;

    RECLAIM_STARTED.store(false, Ordering::Release);
    RECLAIM_RELEASE.store(false, Ordering::Release);
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 2,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.register_view(<Contended as ViewSchema>::DESCRIPTOR);

    let writer_runtime = runtime.clone();
    let writer = std::thread::spawn(move || {
        writer_runtime.call::<reclaim_gate::__HoldSpec>(reclaim_gate::__HoldArgs { _tag: 0 })
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !RECLAIM_STARTED.load(Ordering::Acquire) {
        assert!(
            std::time::Instant::now() < deadline,
            "the demand never started"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    // The demand is live and its attempt holds a claim. The pass must wait for
    // it: a demand reuses the nodes it interned, so freeing one under it is a
    // panic in the kernel, not a lost cache entry.
    let (done_tx, done_rx) = mpsc::channel();
    let collector = std::thread::spawn(move || {
        let stats = runtime.collect();
        let _ = done_tx.send(stats);
    });
    assert!(
        done_rx.recv_timeout(Duration::from_millis(150)).is_err(),
        "a reclaim pass ran while a demand was in flight"
    );

    RECLAIM_RELEASE.store(true, Ordering::Release);
    assert!(writer.join().expect("writer thread").is_ok());
    let stats = done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the pass must run once the demand finishes");
    assert!(
        stats.nodes_before > 0,
        "the waiting pass reclaimed nothing at all"
    );
    collector.join().expect("collector thread");
}

/// Provides the graph's endpoints and reclaims from inside the demand that
/// asked for them.
struct ReclaimingProvider(irrigation::Runtime);

impl irrigation::PortProvider for ReclaimingProvider {
    fn provide(&self, _port: &irrigation::UnitKey) -> irrigation::Result<()> {
        // Both calls run inside the demanding attempt. The pass cannot wait for
        // the demand it belongs to, so it is skipped; a nested `call` is a
        // demand of its own but shares that attempt.
        let _ = self.0.collect();
        self.0
            .call::<provider_graph::__WriteNodesSpec>(provider_graph::__WriteNodesArgs {
                _tag: 0,
            })?;
        Ok(())
    }
}

#[test]
fn a_reclaim_from_inside_a_demand_does_not_deadlock_the_demand() {
    use irrigation::ViewSchema;

    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        auto_collect_every: Some(1),
        ..irrigation::RuntimeConfig::default()
    });
    runtime.register_view(<GraphView as ViewSchema>::DESCRIPTOR);
    runtime.install_port_provider(
        GraphView::view_id(),
        std::sync::Arc::new(ReclaimingProvider(runtime.clone())),
    );
    runtime.install(provider_root::EdgeOnly).expect("install");
    runtime
        .edit(|edit| edit.view::<GuardFlag>().set_on(true))
        .expect("edit");

    let (sender, receiver) = mpsc::channel();
    let reader = {
        let runtime = runtime.clone();
        std::thread::spawn(move || {
            let _ = sender.send(runtime.read_outputs::<provider_root::EdgeOnly>());
        })
    };
    let result = receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("a reclaim pass inside the demand deadlocked it");
    assert_eq!(result.expect("read"), (vec![1, 2], vec![10]));
    reader.join().expect("reader thread");
    assert!(
        runtime.stats().auto_collects > 0,
        "the automatic pass never ran on the host thread"
    );
}

// ---------------------------------------------------------------------------
// A reader waits for a planned port before the writer claims it
// ---------------------------------------------------------------------------

static PLAN_READY: OnceLock<Mutex<mpsc::Sender<()>>> = OnceLock::new();
static PLAN_RELEASE: OnceLock<Mutex<mpsc::Receiver<()>>> = OnceLock::new();
static PLAN_READER_STARTED: OnceLock<Mutex<mpsc::Sender<()>>> = OnceLock::new();

fn signal_plan_ready() {
    if let Some(ready) = PLAN_READY.get() {
        let _ = ready.lock().expect("ready").send(());
    }
}

fn wait_for_plan_release() {
    if let Some(release) = PLAN_RELEASE.get() {
        let _ = release.lock().expect("release").recv();
    }
}

fn signal_plan_reader_started() {
    if let Some(started) = PLAN_READER_STARTED.get() {
        let _ = started.lock().expect("started").send(());
    }
}

#[irrigation]
mod planned_wait {
    use super::{Waited, signal_plan_reader_started, signal_plan_ready, wait_for_plan_release};
    use irrigation::ViewSchema;

    #[component]
    pub fn slow_planned_writer(_tag: u32) -> u32 {
        irrigation::plan_ports(
            &[irrigation::WriteScope::Field(
                Waited::view_id(),
                irrigation::FieldId(0),
            )],
            "planned writer",
        );
        signal_plan_ready();
        wait_for_plan_release();
        Waited.define(1, 42);
        0
    }

    #[component]
    pub fn reader_root() -> u32 {
        signal_plan_reader_started();
        Waited.get(&1).unwrap_or(0)
    }
}

#[test]
fn a_reader_waits_for_a_planned_writer_before_its_claim() {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let _ = PLAN_READY.set(Mutex::new(ready_tx));
    let _ = PLAN_RELEASE.set(Mutex::new(release_rx));
    let _ = PLAN_READER_STARTED.set(Mutex::new(started_tx));

    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 2,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.register_view(<Waited as ViewSchema>::DESCRIPTOR);
    runtime.install(planned_wait::ReaderRoot).expect("install");

    let writer_runtime = runtime.clone();
    let writer = std::thread::spawn(move || {
        writer_runtime.call::<planned_wait::__SlowPlannedWriterSpec>(
            planned_wait::__SlowPlannedWriterArgs { _tag: 0 },
        )
    });
    ready_rx.recv().expect("writer planned its port");

    let reader_runtime = runtime.clone();
    let reader =
        std::thread::spawn(move || reader_runtime.read_outputs::<planned_wait::ReaderRoot>());
    started_rx.recv().expect("reader started");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while runtime.stats().reader_waits == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the reader never registered on the planned port"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    release_tx.send(()).expect("release");

    assert_eq!(reader.join().expect("reader thread").expect("read"), 42);
    assert!(writer.join().expect("writer thread").is_ok());
    assert!(
        runtime.stats().reader_waits > 0,
        "the reader must suspend on the planned provider"
    );
    assert!(
        runtime.stats().reader_plan_waits > 0,
        "the plan wait is counted separately from a claim wait"
    );
    assert!(runtime.stats().planned_ports > 0);
}

#[irrigation]
mod grouped_write {
    use super::Waited;
    use irrigation::ViewSchema;

    #[component]
    pub fn writer(_tag: u32) -> u32 {
        let group = irrigation::WriteGroup::from_scopes([irrigation::WriteScope::Field(
            Waited::view_id(),
            irrigation::FieldId(0),
        )]);
        group.plan("grouped writer");
        group.claim("grouped writer");
        Waited.define(1, 42);
        0
    }
}

#[test]
fn a_write_group_plans_and_claims_its_ports() {
    let runtime = Runtime::new();
    runtime.register_view(<Waited as ViewSchema>::DESCRIPTOR);
    runtime
        .call::<grouped_write::__WriterSpec>(grouped_write::__WriterArgs { _tag: 0 })
        .expect("grouped writer");
    assert_eq!(runtime.snapshot().view::<Waited>().field0(), vec![(1, 42)]);
    assert!(runtime.stats().write_claims > 0);
}

#[irrigation]
mod withdrawn_plan {
    use super::Waited;
    use irrigation::ViewSchema;

    #[component]
    pub fn writer(_tag: u32) -> u32 {
        let group = irrigation::WriteGroup::from_scopes([irrigation::WriteScope::Field(
            Waited::view_id(),
            irrigation::FieldId(0),
        )]);
        group.plan("withdrawn writer");
        group.unplan();
        Waited.define(1, 42);
        0
    }
}

#[test]
fn a_plan_can_be_withdrawn_before_writing() {
    let runtime = Runtime::new();
    runtime.register_view(<Waited as ViewSchema>::DESCRIPTOR);
    runtime
        .call::<withdrawn_plan::__WriterSpec>(withdrawn_plan::__WriterArgs { _tag: 0 })
        .expect("withdrawn plan writer");
    assert_eq!(runtime.snapshot().view::<Waited>().field0(), vec![(1, 42)]);
    assert!(runtime.stats().unplanned_ports > 0);
}
