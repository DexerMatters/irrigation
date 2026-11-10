//! Kernel change notification: a consumer reruns only when an observed unit changed.

#[cfg(feature = "observatory")]
use std::sync::atomic::AtomicU64;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};

use irrigation::{
    ChangeBatch, ComponentId, DependencyUpdates, FieldId, GeneralComponent, GeneralCx, Runtime,
    UnitKey, UnitKind, ViewDescriptor, ViewSchema,
};

#[irrigation::view(source)]
pub struct Source {
    pub value: u32,
}

#[irrigation::view(derived)]
pub struct Constant {
    pub value: u32,
}

#[irrigation::view(derived)]
pub struct Mirror {
    pub value: u32,
}

#[irrigation::view(derived)]
pub struct ConstantOut {
    pub value: u32,
}

#[irrigation::view(derived)]
pub struct MirrorOut {
    pub value: u32,
}

/// Publishes a constant and a mirrored value from one run.
struct Producer {
    runs: Arc<AtomicU32>,
}

impl GeneralComponent for Producer {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C1_0001);
    const NAME: &'static str = "tests::Producer";
    const VIEWS: &'static [&ViewDescriptor] = &[<Source as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[
        <Constant as ViewSchema>::DESCRIPTOR,
        <Mirror as ViewSchema>::DESCRIPTOR,
    ];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let value: u32 = cx.read_typed(UnitKey::field(
            Source::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        let mut output = cx.output();
        output.field(Constant::view_id(), FieldId(0), 7u32);
        output.field(Mirror::view_id(), FieldId(0), value);
        Ok(((), ()))
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

/// Reads only `Constant`.
struct ConstantReader {
    runs: Arc<AtomicU32>,
}

impl GeneralComponent for ConstantReader {
    type State = u32;
    type Output = u32;

    const ID: ComponentId = ComponentId(0x00C1_0002);
    const NAME: &'static str = "tests::ConstantReader";
    const VIEWS: &'static [&ViewDescriptor] = &[<Constant as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[<ConstantOut as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let value: u32 = cx.read_typed(UnitKey::field(
            Constant::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        cx.output().field(ConstantOut::view_id(), FieldId(0), value);
        Ok((value, value))
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

/// Reads only `Mirror`.
struct MirrorReader {
    runs: Arc<AtomicU32>,
}

impl GeneralComponent for MirrorReader {
    type State = u32;
    type Output = u32;

    const ID: ComponentId = ComponentId(0x00C1_0003);
    const NAME: &'static str = "tests::MirrorReader";
    const VIEWS: &'static [&ViewDescriptor] = &[<Mirror as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[<MirrorOut as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let value: u32 = cx.read_typed(UnitKey::field(
            Mirror::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        cx.output().field(MirrorOut::view_id(), FieldId(0), value);
        Ok((value, value))
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

/// Root driver: demands both reader outputs.
struct Driver;

impl GeneralComponent for Driver {
    type State = (u32, u32);
    type Output = (u32, u32);

    const ID: ComponentId = ComponentId(0x00C1_0004);
    const NAME: &'static str = "tests::Driver";
    const VIEWS: &'static [&ViewDescriptor] = &[
        <ConstantOut as ViewSchema>::DESCRIPTOR,
        <MirrorOut as ViewSchema>::DESCRIPTOR,
    ];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let constant: u32 = cx.read_typed(UnitKey::field(
            ConstantOut::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        let mirror: u32 = cx.read_typed(UnitKey::field(
            MirrorOut::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        Ok(((constant, mirror), (constant, mirror)))
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

struct Setup {
    runtime: Runtime,
    producer: Arc<AtomicU32>,
    constant: Arc<AtomicU32>,
    mirror: Arc<AtomicU32>,
}

fn setup() -> Setup {
    setup_with(Runtime::new())
}

/// The producer/reader topology over a runtime the caller built, so a test that
/// needs an observer can supply one.
fn setup_with(runtime: Runtime) -> Setup {
    let producer = Arc::new(AtomicU32::new(0));
    let constant = Arc::new(AtomicU32::new(0));
    let mirror = Arc::new(AtomicU32::new(0));
    runtime
        .install_general_component(Producer {
            runs: Arc::clone(&producer),
        })
        .expect("producer");
    runtime
        .install_general_component(ConstantReader {
            runs: Arc::clone(&constant),
        })
        .expect("constant reader");
    runtime
        .install_general_component(MirrorReader {
            runs: Arc::clone(&mirror),
        })
        .expect("mirror reader");
    runtime.install_general(Driver).expect("driver");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(1))
        .expect("initial edit");
    assert_eq!(
        runtime
            .read_outputs_general::<Driver>()
            .map(|o| o.0)
            .expect("read"),
        7
    );
    assert_eq!(
        runtime
            .read_outputs_general::<Driver>()
            .map(|o| o.1)
            .expect("read"),
        1
    );
    Setup {
        runtime,
        producer,
        constant,
        mirror,
    }
}

#[test]
fn unchanged_output_does_not_invalidate_consumers() {
    let setup = setup();
    let producer_before = setup.producer.load(Ordering::SeqCst);
    let constant_before = setup.constant.load(Ordering::SeqCst);
    let mirror_before = setup.mirror.load(Ordering::SeqCst);
    assert!(producer_before >= 1);
    assert_eq!(constant_before, 1);
    assert_eq!(mirror_before, 1);

    // The producer reruns, but `Constant` keeps the same value.
    setup
        .runtime
        .edit(|edit| edit.view::<Source>().set_value(2))
        .expect("edit");
    assert_eq!(
        setup
            .runtime
            .read_outputs_general::<Driver>()
            .expect("read")
            .1,
        2
    );
    assert!(
        setup.producer.load(Ordering::SeqCst) > producer_before,
        "the producer reruns because its input changed"
    );
    assert_eq!(
        setup.constant.load(Ordering::SeqCst),
        constant_before,
        "a reader of the unchanged unit must not rerun"
    );
    assert!(
        setup.mirror.load(Ordering::SeqCst) > mirror_before,
        "a reader of the changed unit reruns"
    );
    assert!(
        setup.runtime.stats().no_change_runs > 0,
        "the unchanged reader was re-certified instead of re-run"
    );
}

#[test]
fn changed_unit_still_notifies_its_reader() {
    let setup = setup();
    let mirror_before = setup.mirror.load(Ordering::SeqCst);
    setup
        .runtime
        .edit(|edit| edit.view::<Source>().set_value(9))
        .expect("edit");
    assert_eq!(
        setup
            .runtime
            .read_outputs_general::<Driver>()
            .expect("read")
            .1,
        9
    );
    assert_eq!(setup.mirror.load(Ordering::SeqCst), mirror_before + 1);
}

#[test]
fn unchanged_component_is_reused_without_running() {
    let setup = setup();
    let constant_before = setup.constant.load(Ordering::SeqCst);
    let producer_before = setup.producer.load(Ordering::SeqCst);
    let executions_before = setup.runtime.stats().executions;

    // No edit: every observed unit is still certified, so re-demanding the root
    // must not re-run the producer or its readers.
    assert_eq!(
        setup
            .runtime
            .read_outputs_general::<Driver>()
            .expect("read")
            .0,
        7
    );
    assert_eq!(setup.producer.load(Ordering::SeqCst), producer_before);
    assert_eq!(setup.constant.load(Ordering::SeqCst), constant_before);
    assert_eq!(
        setup.runtime.stats().executions,
        executions_before,
        "an unchanged replay executes nothing"
    );
}

/// A component that opts out of incremental change reporting.
struct ResettingReader {
    initializations: Arc<AtomicU32>,
    updates: Arc<AtomicU32>,
}

impl GeneralComponent for ResettingReader {
    type State = u32;
    type Output = u32;

    const ID: ComponentId = ComponentId(0x00C1_0005);
    const NAME: &'static str = "tests::ResettingReader";
    const VIEWS: &'static [&ViewDescriptor] = &[<Mirror as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initializations.fetch_add(1, Ordering::SeqCst);
        let value: u32 = cx.read_typed(UnitKey::field(
            Mirror::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        Ok((value, value))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.updates.fetch_add(1, Ordering::SeqCst);
        self.initialize(cx)
    }
}

#[test]
fn reset_component_reinitializes_when_its_input_moves() {
    let initializations = Arc::new(AtomicU32::new(0));
    let updates = Arc::new(AtomicU32::new(0));
    let producer_runs = Arc::new(AtomicU32::new(0));
    let runtime = Runtime::new();
    runtime
        .install_general_component(Producer {
            runs: Arc::clone(&producer_runs),
        })
        .expect("producer");
    runtime
        .install_general(ResettingReader {
            initializations: Arc::clone(&initializations),
            updates: Arc::clone(&updates),
        })
        .expect("resetting reader");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(1))
        .expect("edit");
    let _ = runtime
        .read_outputs_general::<ResettingReader>()
        .expect("read");
    let before = initializations.load(Ordering::SeqCst);

    runtime
        .edit(|edit| edit.view::<Source>().set_value(2))
        .expect("edit");
    let _ = runtime
        .read_outputs_general::<ResettingReader>()
        .expect("read");
    assert!(
        initializations.load(Ordering::SeqCst) > before,
        "a Reset component reinitializes when an observed unit moves"
    );
    assert_eq!(
        updates.load(Ordering::SeqCst),
        0,
        "a Reset component never receives an incremental update"
    );
}

#[test]
fn subscription_fires_only_for_observed_units() {
    let setup = setup();
    let constant_fired = Arc::new(AtomicU32::new(0));
    let mirror_fired = Arc::new(AtomicU32::new(0));
    let constant_counter = Arc::clone(&constant_fired);
    let mirror_counter = Arc::clone(&mirror_fired);
    let _constant = setup
        .runtime
        .observe::<Constant>(move |_snapshot| {
            constant_counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("observe constant");
    let _mirror = setup
        .runtime
        .observe::<Mirror>(move |_snapshot| {
            mirror_counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("observe mirror");

    // The producer reruns, but only `Mirror` changes.
    setup
        .runtime
        .edit(|edit| edit.view::<Source>().set_value(5))
        .expect("edit");
    let _ = setup
        .runtime
        .read_outputs_general::<Driver>()
        .expect("read");
    assert_eq!(
        constant_fired.load(Ordering::SeqCst),
        0,
        "a subscription on an unchanged View does not fire"
    );
    assert_eq!(
        mirror_fired.load(Ordering::SeqCst),
        1,
        "a subscription on the changed View fires once"
    );
}

/// Records the units reported by its last incremental update.
struct ChangeRecorder {
    seen: Arc<std::sync::Mutex<Vec<irrigation::UnitKey>>>,
}

impl GeneralComponent for ChangeRecorder {
    type State = ();
    type Output = u32;

    const ID: ComponentId = ComponentId(0x00C1_0006);
    const NAME: &'static str = "tests::ChangeRecorder";
    const VIEWS: &'static [&ViewDescriptor] = &[<Mirror as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u32 = cx.read_typed(UnitKey::field(
            Mirror::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        Ok(((), value))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        changes: &ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let mut seen = self.seen.lock().expect("lock");
        seen.clear();
        seen.extend(changes.changes().iter().map(|change| change.unit.clone()));
        drop(seen);
        self.initialize(cx)
    }
}

#[test]
fn change_batch_reports_the_moved_units() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let runtime = Runtime::new();
    runtime
        .install_general_component(Producer {
            runs: Arc::new(AtomicU32::new(0)),
        })
        .expect("producer");
    runtime
        .install_general(ChangeRecorder {
            seen: Arc::clone(&seen),
        })
        .expect("recorder");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(1))
        .expect("edit");
    let value: u32 = runtime
        .read_outputs_general::<ChangeRecorder>()
        .expect("read");
    assert_eq!(value, 1);

    runtime
        .edit(|edit| edit.view::<Source>().set_value(4))
        .expect("edit");
    let value: u32 = runtime
        .read_outputs_general::<ChangeRecorder>()
        .expect("read");
    assert_eq!(value, 4, "the edit is visible");
    let units = seen.lock().expect("lock").clone();
    assert!(
        units.iter().any(|unit| unit.view == Mirror::view_id()),
        "the moved unit is reported: {units:?}"
    );
    assert!(
        units.iter().all(|unit| unit.view == Mirror::view_id()),
        "only the moved unit is reported: {units:?}"
    );
}

/// An ordinary root. Its identity deliberately collides with `Producer`, so it
/// also stands for a root that tries to reuse a general component's identity.
struct PlainRoot;

impl irrigation::RootComponent for PlainRoot {
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C1_0001);
    const NAME: &'static str = "tests::PlainRoot";
    const ARITY: usize = 1;
    const VIEWS: &'static [&ViewDescriptor] = &[];

    fn run() {}

    fn project(_output: &Self::Output, _index: usize) -> irrigation::Value {
        irrigation::value_of(())
    }

    fn reconstruct(_parts: &[irrigation::Value]) -> Self::Output {}

    fn projection_eq(_index: usize, _left: &irrigation::Value, _right: &irrigation::Value) -> bool {
        true
    }
}

#[test]
fn a_general_root_blocks_a_later_root() {
    let runtime = Runtime::new();
    runtime.install_general(Driver).expect("general root");
    assert!(
        runtime.install(PlainRoot).is_err(),
        "a runtime has one root slot, whether the root is general or ordinary"
    );
}

#[test]
fn a_root_blocks_a_later_general_root() {
    let runtime = Runtime::new();
    runtime.install(PlainRoot).expect("root");
    assert!(runtime.install_general(Driver).is_err());
}

#[test]
fn a_root_cannot_share_a_component_id_with_a_general_component() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(Producer {
            runs: Arc::new(AtomicU32::new(0)),
        })
        .expect("producer");
    assert!(
        runtime.install(PlainRoot).is_err(),
        "the producer already owns that component id"
    );
}

#[test]
fn a_nested_refresh_does_not_rewind_last_version() {
    let runtime = Runtime::new();
    runtime.register_view(<Source as ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| edit.view::<Source>().set_value(1))
        .expect("seed");

    // The first subscription starts a second edit from inside its callback, so
    // a refresh runs while the outer refresh is still delivering. That is the
    // interleaving two concurrent edits produce.
    let nested = Arc::new(AtomicBool::new(false));
    let runtime_for_trigger = runtime.clone();
    let nested_for_trigger = Arc::clone(&nested);
    let _trigger = runtime
        .observe_with_changes::<Source>(move |_snapshot, _summary| {
            if nested_for_trigger.swap(true, Ordering::SeqCst) {
                return;
            }
            runtime_for_trigger
                .edit(|edit| edit.view::<Source>().set_value(2))
                .expect("nested edit");
        })
        .expect("trigger");

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_by_main = Arc::clone(&seen);
    let _main = runtime
        .observe_with_changes::<Source>(move |snapshot, summary| {
            seen_by_main.lock().expect("lock").push((
                snapshot.revision(),
                summary.previous,
                summary.current,
            ));
        })
        .expect("main");

    runtime
        .edit(|edit| edit.view::<Source>().set_value(3))
        .expect("edit");

    let notifications = seen.lock().expect("lock").clone();
    assert!(
        !notifications.is_empty(),
        "the main subscription was never notified"
    );
    let mut dialed_at = 0;
    for (revision, previous, current) in &notifications {
        assert!(
            *revision > dialed_at,
            "a delivery went back in time: {notifications:?}"
        );
        assert!(
            current > previous,
            "revision {revision} was delivered with previous {previous} and current {current}"
        );
        dialed_at = *revision;
    }
    assert_eq!(
        notifications.last().map(|delivery| delivery.0),
        Some(3),
        "the newest published revision is the one left with the subscriber: {notifications:?}"
    );
}

#[test]
fn a_refresh_that_races_another_does_not_dial_an_older_snapshot() {
    let runtime = Runtime::new();
    runtime.register_view(<Source as ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| edit.view::<Source>().set_value(1))
        .expect("seed");

    // The first subscription hands the thread that commits a second revision
    // the moment this refresh is between its staging pass and its dialing pass.
    // That edit's refresh delivers revision 3 before this one reaches its own
    // already-staged revision 2.
    let (request, wait_for_request) = std::sync::mpsc::channel::<()>();
    let (finished, wait_for_finish) = std::sync::mpsc::channel::<()>();
    let wait_for_finish = Arc::new(std::sync::Mutex::new(wait_for_finish));
    let fired = Arc::new(AtomicBool::new(false));
    let fired_by_trigger = Arc::clone(&fired);
    let _trigger = runtime
        .observe_with_changes::<Source>(move |_snapshot, _summary| {
            if fired_by_trigger.swap(true, Ordering::SeqCst) {
                return;
            }
            request.send(()).expect("request the racing edit");
            wait_for_finish
                .lock()
                .expect("lock")
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("wait for the racing edit");
        })
        .expect("trigger");

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_by_main = Arc::clone(&seen);
    let _main = runtime
        .observe_with_changes::<Source>(move |snapshot, summary| {
            seen_by_main.lock().expect("lock").push((
                snapshot.revision(),
                summary.previous,
                summary.current,
            ));
        })
        .expect("main");

    let editor_runtime = runtime.clone();
    let editor = std::thread::spawn(move || {
        wait_for_request
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("request");
        editor_runtime
            .edit(|edit| edit.view::<Source>().set_value(2))
            .expect("racing edit");
        finished.send(()).expect("signal");
    });
    runtime
        .edit(|edit| edit.view::<Source>().set_value(3))
        .expect("edit");
    editor.join().expect("editor");

    let notifications = seen.lock().expect("lock").clone();
    let mut dialed_at = 0;
    for (revision, previous, current) in &notifications {
        assert!(
            *revision > dialed_at,
            "a superseded snapshot was dialed after a newer one: {notifications:?}"
        );
        assert!(
            current > previous,
            "revision {revision} was delivered with previous {previous} and current {current}"
        );
        dialed_at = *revision;
    }
    assert_eq!(
        notifications.last().map(|delivery| delivery.0),
        Some(3),
        "the newest published revision is the one left with the subscriber: {notifications:?}"
    );
}

#[test]
fn a_refresh_that_races_a_callback_does_not_deliver_an_older_snapshot() {
    let runtime = Runtime::new();
    runtime.register_view(<Source as ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| edit.view::<Source>().set_value(1))
        .expect("seed");

    // The pause sits inside the first callback, i.e. after the supersede check
    // has released the metadata lock, and the host applies the snapshot at the
    // end of the callback. A refresh dialed from another thread would apply
    // revision 3 first and then let this invocation apply its older revision 2;
    // with dialing serialized that refresh may only append.
    let (request, wait_for_request) = std::sync::mpsc::channel::<()>();
    let (finished, wait_for_finish) = std::sync::mpsc::channel::<()>();
    let wait_for_finish = Arc::new(std::sync::Mutex::new(wait_for_finish));
    let first = Arc::new(AtomicBool::new(true));
    let applied = Arc::new(std::sync::Mutex::new(Vec::new()));

    let applied_by_callback = Arc::clone(&applied);
    let first_by_callback = Arc::clone(&first);
    let _subscription = runtime
        .observe_with_changes::<Source>(move |snapshot, _summary| {
            let revision = snapshot.revision();
            applied_by_callback.lock().expect("lock").push(revision);
            if !first_by_callback.swap(false, Ordering::SeqCst) {
                return;
            }
            request.send(()).expect("request the racing edit");
            wait_for_finish
                .lock()
                .expect("lock")
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("wait for the racing edit");
            applied_by_callback.lock().expect("lock").push(revision);
        })
        .expect("main");

    let editor_runtime = runtime.clone();
    let editor = std::thread::spawn(move || {
        wait_for_request
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("request");
        editor_runtime
            .edit(|edit| edit.view::<Source>().set_value(3))
            .expect("racing edit");
        finished.send(()).expect("signal");
    });
    runtime
        .edit(|edit| edit.view::<Source>().set_value(2))
        .expect("edit");
    editor.join().expect("editor");

    let applied = applied.lock().expect("lock").clone();
    assert!(!applied.is_empty(), "the subscription was never notified");
    let mut applied_at = 0;
    for revision in &applied {
        assert!(
            *revision >= applied_at,
            "an older snapshot was applied after a newer one: {applied:?}"
        );
        applied_at = *revision;
    }
    assert_eq!(
        applied.last(),
        Some(&3),
        "the newest published revision is the one left applied: {applied:?}"
    );
}

#[test]
fn a_delivery_queued_behind_a_dropped_subscription_is_not_dialed() {
    let runtime = Runtime::new();
    runtime.register_view(<Source as ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| edit.view::<Source>().set_value(1))
        .expect("seed");

    // Both subscriptions are staged for revision 2. The first callback holds
    // the drain, so the second delivery is still queued when its owner drops
    // it from another thread. Dialing it after that would resurrect a
    // subscription the host already released.
    let (request, wait_for_request) = std::sync::mpsc::channel::<()>();
    let (finished, wait_for_finish) = std::sync::mpsc::channel::<()>();
    let wait_for_finish = Arc::new(std::sync::Mutex::new(wait_for_finish));
    let blocking = Arc::new(AtomicBool::new(false));
    let blocking_by_first = Arc::clone(&blocking);
    let _first = runtime
        .observe::<Source>(move |_snapshot| {
            if blocking_by_first.swap(true, Ordering::SeqCst) {
                return;
            }
            request.send(()).expect("request the drop");
            wait_for_finish
                .lock()
                .expect("lock")
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("wait for the drop");
        })
        .expect("first");

    let fired = Arc::new(AtomicU32::new(0));
    let fired_by_second = Arc::clone(&fired);
    let second = runtime
        .observe::<Source>(move |_snapshot| {
            fired_by_second.fetch_add(1, Ordering::SeqCst);
        })
        .expect("second");

    let dropper = std::thread::spawn(move || {
        wait_for_request
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("request");
        drop(second);
        finished.send(()).expect("signal");
    });
    runtime
        .edit(|edit| edit.view::<Source>().set_value(2))
        .expect("edit");
    dropper.join().expect("dropper");

    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "a delivery still queued for a dropped subscription was dialed"
    );
}

#[test]
fn a_panicking_callback_does_not_wedge_the_delivery_queue() {
    let runtime = Runtime::new();
    runtime.register_view(<Source as ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| edit.view::<Source>().set_value(1))
        .expect("seed");

    let calls = Arc::new(AtomicU32::new(0));
    let calls_by_panicker = Arc::clone(&calls);
    let _panicker = runtime
        .observe::<Source>(move |_snapshot| {
            if calls_by_panicker.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("the first delivery panics");
            }
        })
        .expect("panicker");

    let (notified, wait_for_notified) = std::sync::mpsc::channel();
    let _second = runtime
        .observe_with_changes::<Source>(move |snapshot, _summary| {
            let _ = notified.send(snapshot.revision());
        })
        .expect("second");

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.edit(|edit| edit.view::<Source>().set_value(2))
    }));
    assert!(
        panicked.is_err(),
        "the panicking callback must unwind through the edit"
    );

    // The panicker's delivery was consumed; the second delivery was still
    // queued when the owner unwound. A later refresh must be able to take the
    // queue again and dial the revision staged for it.
    runtime
        .edit(|edit| edit.view::<Source>().set_value(3))
        .expect("edit");
    assert_eq!(
        wait_for_notified
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the queue was never drained again"),
        3,
        "the delivery left queued by the panic is dialed with the newest revision"
    );
}

/// A host capture whose destructor reads the runtime, as a handle that releases
/// its own bookkeeping on drop does.
struct SnapshotProbe {
    runtime: Runtime,
}

impl Drop for SnapshotProbe {
    fn drop(&mut self) {
        let _ = self.runtime.stats();
    }
}

#[test]
fn dropping_a_subscription_destroys_its_callback_outside_the_metadata_lock() {
    let runtime = Runtime::new();
    runtime.register_view(<Source as ViewSchema>::DESCRIPTOR);
    let probe = SnapshotProbe {
        runtime: runtime.clone(),
    };
    let subscription = runtime
        .observe::<Source>(move |_snapshot| {
            let _ = &probe;
        })
        .expect("observe");

    // The record owns the only reference to the callback, so removing it
    // destroys the callback. A destructor that takes the metadata lock
    // deadlocks if the removal drops it under that lock.
    let (dropped, wait_for_drop) = std::sync::mpsc::channel();
    let dropper = std::thread::spawn(move || {
        drop(subscription);
        dropped.send(()).expect("signal");
    });
    wait_for_drop
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the callback was destroyed while the metadata lock was held");
    dropper.join().expect("dropper");
}

#[irrigation::view(source)]
pub struct Other {
    pub value: u32,
}

#[test]
fn an_empty_unit_set_is_a_whole_view_subscription() {
    let runtime = Runtime::new();
    runtime.register_view(<Source as ViewSchema>::DESCRIPTOR);
    runtime.register_view(<Other as ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| edit.view::<Source>().set_value(1))
        .expect("seed");

    let fired = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&fired);
    let _subscription = runtime
        .observe_units::<Source>(Vec::new(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("observe");

    runtime
        .edit(|edit| edit.view::<Other>().set_value(1))
        .expect("unrelated edit");
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "an unchanged View must not notify its whole-View subscription"
    );

    runtime
        .edit(|edit| edit.view::<Source>().set_value(2))
        .expect("edit");
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "a changed View notifies its whole-View subscription once"
    );
}

/// A component demanded directly: its invocation nodes are unreachable from a
/// root, so `collect` reclaims them.
#[irrigation::irrigation]
mod direct {
    #[irrigation::component]
    pub fn offset(tag: u32) -> u32 {
        tag + 1
    }
}

#[test]
fn explaining_during_reclamation_never_panics() {
    let runtime = Runtime::new();
    let panicked = Arc::new(AtomicBool::new(false));
    for _round in 0..32 {
        // Unreachable invocation bodies: no collector has run since they were
        // allocated, so every one of them is live and reclaimable.
        for tag in 0..32u32 {
            let _ = runtime.call::<direct::__OffsetSpec>(direct::__OffsetArgs { tag });
        }
        let ids: Vec<irrigation::NodeId> = runtime
            .dump()
            .into_iter()
            .filter(|node| node.kind == "InvocationBody")
            .map(|node| node.id)
            .filter(|node| runtime.is_live(*node))
            .collect();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let runtime = &runtime;
                let ids = &ids;
                let panicked = Arc::clone(&panicked);
                scope.spawn(move || {
                    for node in ids {
                        let explained =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                runtime.try_explain_node(*node)
                            }));
                        if explained.is_err() {
                            panicked.store(true, Ordering::SeqCst);
                        }
                    }
                });
            }
            // This pass reclaims every target while the explanation threads are
            // between their identity check and the node read.
            let _ = runtime.collect();
        });
        if panicked.load(Ordering::SeqCst) {
            break;
        }
    }
    assert!(
        !panicked.load(Ordering::SeqCst),
        "explaining a node reclaimed under it panicked"
    );
}

#[irrigation::view(derived)]
pub struct Presence(irrigation::Map<u32, u32>);

/// Publishes one entity's membership as false on its first ever definition.
struct PresenceProducer;

impl GeneralComponent for PresenceProducer {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C1_0006);
    const NAME: &'static str = "tests::PresenceProducer";
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[<Presence as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        cx.output()
            .membership(Presence::view_id(), FieldId(0), 1u32, false);
        Ok(((), ()))
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

/// Reads the membership unit itself, as the observation surface does.
struct PresenceReader;

impl GeneralComponent for PresenceReader {
    type State = bool;
    type Output = bool;

    const ID: ComponentId = ComponentId(0x00C1_0007);
    const NAME: &'static str = "tests::PresenceReader";
    const VIEWS: &'static [&ViewDescriptor] = &[<Presence as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let present: bool = cx.read_typed(UnitKey::entity(
            Presence::view_id(),
            FieldId(0),
            UnitKind::Membership,
            irrigation::any_key(1u32),
        ))?;
        Ok((present, present))
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

#[test]
fn a_first_false_membership_define_reads_false() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(PresenceProducer)
        .expect("membership producer");
    runtime
        .install_general(PresenceReader)
        .expect("membership reader");
    assert!(
        !runtime
            .read_outputs_general::<PresenceReader>()
            .expect("read"),
        "a membership defined false for the first time must not read true"
    );
}

#[test]
fn a_host_write_to_a_derived_view_is_refused() {
    let setup = setup();
    let revision = setup.runtime.revision();
    let unit = UnitKey::field(Constant::view_id(), FieldId(0), UnitKind::Value);

    let refused = setup.runtime.edit(|edit| edit.set(unit.clone(), 99u32));
    assert!(
        matches!(
            refused,
            Err(irrigation::ComputeError::DefinitionDenied { .. })
        ),
        "a host write to a derived View's unit must be refused: {refused:?}"
    );
    assert_eq!(
        setup.runtime.revision(),
        revision,
        "a refused transaction publishes no revision"
    );

    let refused = setup.runtime.edit(|edit| {
        edit.set_content(
            Constant::view_id(),
            Arc::new(ConstantContent { value: 99 }),
            irrigation::values_equal::<ConstantContent>,
        );
    });
    assert!(
        matches!(
            refused,
            Err(irrigation::ComputeError::DefinitionDenied { .. })
        ),
        "a host content replacement on a derived View must be refused: {refused:?}"
    );
    assert_eq!(setup.runtime.revision(), revision);

    // The writes staged beside the refused one are discarded with it.
    let refused = setup.runtime.edit(|edit| {
        edit.view::<Source>().set_value(9);
        edit.set(unit, 99u32);
    });
    assert!(
        matches!(
            refused,
            Err(irrigation::ComputeError::DefinitionDenied { .. })
        ),
        "one refused write refuses its transaction: {refused:?}"
    );
    assert_eq!(
        setup.runtime.read_outputs_general::<Driver>(),
        Ok((7, 1)),
        "neither the derived value nor the source value staged with it moved"
    );
    assert_eq!(
        setup
            .runtime
            .read()
            .expect("read")
            .view::<Constant>()
            .value(),
        7,
        "the derived View still publishes its provider's value"
    );

    // The refusal leaves the runtime usable for the edits that are in scope.
    setup
        .runtime
        .edit(|edit| edit.view::<Source>().set_value(5))
        .expect("source edit");
    assert_eq!(
        setup.runtime.read_outputs_general::<Driver>(),
        Ok((7, 5)),
        "the derived View re-derives from the new source value"
    );
}

/// A source View whose first unit a provider settles and whose second unit only
/// the host edits.
#[irrigation::view(source)]
pub struct Draft {
    pub settled: u32,
    pub pending: u32,
}

#[irrigation::irrigation]
mod draft_settler {
    use super::Draft;
    use irrigation::{FieldId, UnitKey, UnitKind, ViewSchema};

    #[component]
    pub fn root(draft: &Draft) -> u32 {
        let pending = draft.pending();
        irrigation::ensure_view(<Draft as ViewSchema>::DESCRIPTOR);
        irrigation::define_unit(
            UnitKey::field(Draft::view_id(), FieldId(0), UnitKind::Value),
            pending + 1,
            "draft_settler::root",
        );
        pending
    }
}

#[test]
fn a_host_write_to_a_provider_owned_source_unit_is_refused() {
    let runtime = Runtime::new();
    runtime.install(draft_settler::Root).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<draft_settler::Root>()
            .expect("provider run"),
        0,
        "the provider settles the first draft"
    );
    let revision = runtime.revision();

    // `Draft.settled` belongs to the provider, so the whole transaction is
    // refused before either staged write lands.
    let refused = runtime.edit(|edit| {
        let mut draft = edit.view::<Draft>();
        draft.set_settled(99);
        draft.set_pending(9);
    });
    assert!(
        matches!(
            refused,
            Err(irrigation::ComputeError::DefinitionDenied { .. })
        ),
        "a host write to the provider's source unit must be refused: {refused:?}"
    );
    assert_eq!(
        runtime.revision(),
        revision,
        "a refused transaction publishes no revision"
    );
    let snapshot = runtime.inspect_snapshot();
    assert_eq!(
        snapshot.view::<Draft>().settled(),
        1,
        "the provider's unit keeps its committed value"
    );
    assert_eq!(
        snapshot.view::<Draft>().pending(),
        0,
        "the source write co-staged with the refused one was discarded"
    );
}

#[test]
fn a_source_unit_no_provider_owns_stays_editable() {
    let runtime = Runtime::new();
    runtime.install(draft_settler::Root).expect("install");
    runtime
        .read_outputs::<draft_settler::Root>()
        .expect("provider run");
    let revision = runtime.revision();

    // `Draft.pending` has no owner: this is the ordinary source-edit path.
    runtime
        .edit(|edit| edit.view::<Draft>().set_pending(5))
        .expect("a host write to an unowned source unit must succeed");
    assert!(
        runtime.revision() > revision,
        "the source edit publishes its revision"
    );

    // The provider path is not the edit path: the owner restages its own unit
    // when the source edit invalidates it.
    assert_eq!(
        runtime
            .read_outputs::<draft_settler::Root>()
            .expect("provider re-run"),
        5
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Draft>().settled(),
        6,
        "the provider redefines the unit it owns"
    );
}

#[test]
fn a_unit_subscription_on_a_provider_owned_source_unit_drives_its_producer() {
    let runtime = Runtime::new();
    runtime.install(draft_settler::Root).expect("install");
    runtime
        .read_outputs::<draft_settler::Root>()
        .expect("provider run");

    let settled = UnitKey::field(Draft::view_id(), FieldId(0), UnitKind::Value);
    let fired = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let counter = Arc::clone(&fired);
    let sink = Arc::clone(&seen);
    let _subscription = runtime
        .observe_units::<Draft>(vec![settled], move |snapshot| {
            counter.fetch_add(1, Ordering::SeqCst);
            sink.lock()
                .expect("lock")
                .push(snapshot.view::<Draft>().settled());
        })
        .expect("observe the settled unit");

    // `Draft.pending` is the host's, but the edit invalidates the provider that
    // owns the watched unit: only running that provider moves the subscription.
    runtime
        .edit(|edit| edit.view::<Draft>().set_pending(5))
        .expect("edit");
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the refresh drives the producer that owns the watched unit"
    );
    assert_eq!(
        *seen.lock().expect("lock"),
        vec![6],
        "the callback reads the snapshot the producer refreshed"
    );
}

/// A source View whose provider fills one entity of a map field.
#[irrigation::view(source)]
pub struct Cache(irrigation::Map<u32, u32>);

#[irrigation::irrigation]
mod cache_filler {
    use super::Cache;
    use irrigation::{FieldId, UnitKey, UnitKind, ViewSchema};

    fn entry_unit(key: u32) -> UnitKey {
        UnitKey::entity(
            Cache::view_id(),
            FieldId(0),
            UnitKind::Value,
            irrigation::any_key(key),
        )
    }

    #[component]
    pub fn root() -> u32 {
        irrigation::ensure_view(<Cache as ViewSchema>::DESCRIPTOR);
        irrigation::define_unit(entry_unit(1), 10u32, "cache_filler::root");
        0
    }
}

#[test]
fn a_host_write_to_a_provider_owned_entity_source_unit_is_refused() {
    let runtime = Runtime::new();
    runtime.install(cache_filler::Root).expect("install");
    runtime
        .read_outputs::<cache_filler::Root>()
        .expect("provider run");

    // The provider's claim is recorded on the entity's membership unit, so the
    // value unit of the same entity answers to it.
    let unit = UnitKey::entity(
        Cache::view_id(),
        FieldId(0),
        UnitKind::Value,
        irrigation::any_key(1u32),
    );
    let refused = runtime.edit(|edit| edit.set(unit, 11u32));
    assert!(
        matches!(
            refused,
            Err(irrigation::ComputeError::DefinitionDenied { .. })
        ),
        "a host write to the provider's entity unit must be refused: {refused:?}"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Cache>().field0(),
        vec![(1, 10)],
        "the provider's entry keeps its committed value"
    );
}

#[test]
fn a_content_write_over_a_provider_owned_source_cell_is_refused() {
    let runtime = Runtime::new();
    runtime.install(draft_settler::Root).expect("install");
    runtime
        .read_outputs::<draft_settler::Root>()
        .expect("provider run");
    let revision = runtime.revision();

    // The replacement addresses the View's whole content, not the unit a
    // provider settles: the refusal has to decide per owned cell.
    let refused = runtime.edit(|edit| {
        edit.set_content(
            Draft::view_id(),
            Arc::new(DraftContent {
                settled: 99,
                pending: 0,
            }),
            irrigation::values_equal::<DraftContent>,
        );
    });
    assert!(
        matches!(
            refused,
            Err(irrigation::ComputeError::DefinitionDenied { .. })
        ),
        "a content write that replaces a provider-owned cell must be refused: {refused:?}"
    );
    assert_eq!(
        runtime.revision(),
        revision,
        "a refused transaction publishes no revision"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Draft>().settled(),
        1,
        "the provider's cell keeps its committed value"
    );

    // A replacement that agrees with the provider on its cell is the host's to
    // publish: the mask restores the committed cell and compares.
    runtime
        .edit(|edit| {
            edit.set_content(
                Draft::view_id(),
                Arc::new(DraftContent {
                    settled: 1,
                    pending: 0,
                }),
                irrigation::values_equal::<DraftContent>,
            );
        })
        .expect("a content write that keeps the provider's cell is not refused");
}

#[test]
fn a_content_write_that_empties_a_provider_owned_entity_is_refused() {
    let runtime = Runtime::new();
    runtime.install(cache_filler::Root).expect("install");
    runtime
        .read_outputs::<cache_filler::Root>()
        .expect("provider run");
    let revision = runtime.revision();

    let refused = runtime.edit(|edit| {
        edit.set_content(
            Cache::view_id(),
            Arc::new(CacheContent::empty()),
            irrigation::values_equal::<CacheContent>,
        );
    });
    assert!(
        matches!(
            refused,
            Err(irrigation::ComputeError::DefinitionDenied { .. })
        ),
        "a content write that drops a provider-owned entry must be refused: {refused:?}"
    );
    assert_eq!(
        runtime.revision(),
        revision,
        "a refused transaction publishes no revision"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Cache>().field0(),
        vec![(1, 10)],
        "the provider's entry keeps its committed value"
    );

    // Keeping the provider's membership but changing the value it publishes is
    // the same overwrite through the entity's value cell.
    let (kept, _) = irrigation::PMap::new().insert(1u32, 11u32);
    let refused = runtime.edit(|edit| {
        edit.set_content(
            Cache::view_id(),
            Arc::new(CacheContent {
                field0: kept.into(),
            }),
            irrigation::values_equal::<CacheContent>,
        );
    });
    assert!(
        matches!(
            refused,
            Err(irrigation::ComputeError::DefinitionDenied { .. })
        ),
        "a content write that changes a provider-owned value must be refused: {refused:?}"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Cache>().field0(),
        vec![(1, 10)],
        "the provider's value keeps its committed cell"
    );
}

#[test]
fn a_content_write_to_a_source_view_no_provider_owns_stays_writable() {
    let runtime = Runtime::new();
    runtime
        .edit(|edit| edit.view::<Source>().set_value(3))
        .expect("seed");
    let revision = runtime.revision();

    runtime
        .edit(|edit| {
            edit.set_content(
                Source::view_id(),
                Arc::new(SourceContent { value: 4 }),
                irrigation::values_equal::<SourceContent>,
            );
        })
        .expect("no provider owns a cell in this View");
    assert!(
        runtime.revision() > revision,
        "the content write publishes its revision"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Source>().value(),
        4,
        "the host's content is what the snapshot serves"
    );
}

#[irrigation::irrigation]
mod source_value_reader {
    use super::Source;

    #[component]
    pub fn root(source: &Source) -> u32 {
        source.value()
    }
}

#[test]
fn a_content_write_refreshes_a_materialized_scalar_cell() {
    let runtime = Runtime::new();
    runtime.install(source_value_reader::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(3))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<source_value_reader::Root>()
            .expect("read"),
        3,
        "the reader materializes the field's value cell"
    );

    runtime
        .edit(|edit| {
            edit.set_content(
                Source::view_id(),
                Arc::new(SourceContent { value: 4 }),
                irrigation::values_equal::<SourceContent>,
            );
        })
        .expect("no provider owns a cell in this View");
    assert_eq!(
        runtime
            .read_outputs::<source_value_reader::Root>()
            .expect("re-read"),
        4,
        "a materialized scalar cell must follow the replaced content"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Source>().value(),
        4,
        "the cell and the snapshot agree at one revision"
    );
}

#[test]
fn a_content_write_refreshes_the_host_cell_and_leaves_the_providers_alone() {
    let runtime = Runtime::new();
    runtime.install(draft_settler::Root).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<draft_settler::Root>()
            .expect("provider run"),
        0,
        "the provider settles the first draft"
    );

    // The reader materialized the host's `pending` cell; `settled` is the
    // provider's. The replacement may move the first and must not touch the
    // second.
    runtime
        .edit(|edit| {
            edit.set_content(
                Draft::view_id(),
                Arc::new(DraftContent {
                    settled: 1,
                    pending: 5,
                }),
                irrigation::values_equal::<DraftContent>,
            );
        })
        .expect("a replacement that keeps the provider's cell");
    assert_eq!(
        runtime
            .read_outputs::<draft_settler::Root>()
            .expect("re-read"),
        5,
        "the host's materialized cell must follow the replaced content"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Draft>().settled(),
        6,
        "the provider restages the cell it owns"
    );
}

#[irrigation::view(derived)]
pub struct Contested(u64);

#[irrigation::irrigation]
mod contested_route {
    use super::{Contested, install_late_selector};

    #[component]
    pub fn first(tag: u64) -> u64 {
        let _ = tag;
        Contested.define_field0(1);
        1
    }

    #[component]
    pub fn late(tag: u64) -> u64 {
        let _ = tag;
        Contested.define_field0(2);
        2
    }

    #[component]
    pub fn reader(tag: u64) -> u64 {
        let _ = tag;
        Contested.field0()
    }

    #[component]
    pub fn drive(tag: u64) -> u64 {
        let _ = tag;
        let _ = late(0);
        // The reader finalizes this revision against the late install, then the
        // demand's resolve pass restores the earlier provider under it.
        let seen = reader(0);
        install_late_selector();
        seen
    }
}

/// Chooses the earlier provider, so a late install is always restored.
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

/// The runtime a conflict driver installs a selector on, once.
static LATE_ROUTE: std::sync::Mutex<Option<Runtime>> = std::sync::Mutex::new(None);

/// Installs the selector after the late provider committed, so the demand ends
/// with a staged conflict for it to resolve. The replayed attempt finds it
/// already installed.
fn install_late_selector() {
    let mut slot = LATE_ROUTE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(runtime) = slot.take() {
        runtime.set_route_selector(Arc::new(PreferFirst {
            winner: std::sync::Mutex::new(None),
        }));
    }
}

#[test]
fn a_restore_that_overtakes_a_finalized_root_replays_the_demand() {
    let runtime = Runtime::new();
    assert_eq!(
        runtime
            .call::<contested_route::__FirstSpec>(contested_route::__FirstArgs { tag: 0 })
            .expect("first provider"),
        1
    );
    *LATE_ROUTE.lock().expect("late route") = Some(runtime.clone());

    let retries = runtime.stats().demand_retries;
    let value = runtime
        .call::<contested_route::__DriveSpec>(contested_route::__DriveArgs { tag: 0 })
        .expect("the demand replays and settles on the restored provider");
    assert_eq!(
        value, 1,
        "the demand carried out the value the restore retracted"
    );
    assert!(
        runtime.stats().demand_retries > retries,
        "a restore that overtook a finalized root must report a retry"
    );
    assert_eq!(
        runtime.snapshot().view::<Contested>().field0(),
        1,
        "the restored provider's value is the committed one"
    );
    assert_eq!(
        runtime
            .call::<contested_route::__ReaderSpec>(contested_route::__ReaderArgs { tag: 0 })
            .expect("a reader at the same revision agrees"),
        1
    );
}

/// Parks the first observation refresh right after its demand returns — the
/// point a split refresh read its unit versions from — so a racing edit commits
/// there.
#[cfg(feature = "observatory")]
struct ParkRefresh {
    runtime: std::sync::OnceLock<Runtime>,
    request: std::sync::mpsc::Sender<()>,
    parked: AtomicBool,
    /// The refresh task, remembered from its start event.
    refresh: AtomicU64,
    /// Tasks this observer has already seen an event for, so a `DemandStarted`
    /// that arrives after its own task has begun is caught.
    seen: std::sync::Mutex<std::collections::HashSet<u64>>,
}

#[cfg(feature = "observatory")]
impl irrigation_observe::RuntimeObserver for ParkRefresh {
    fn level(&self) -> irrigation_observe::ObserveLevel {
        irrigation_observe::ObserveLevel::Summary
    }

    fn record(&self, event: irrigation_observe::RuntimeEvent) {
        // The refresh demand is identified when it starts and parked when it
        // finishes: `DemandStarted` now means "queued", so it is the wrong
        // moment to act on -- the point this test needs is that the demand has
        // returned and released its revision pin.
        match event {
            irrigation_observe::RuntimeEvent::DemandStarted {
                task,
                cause: irrigation_observe::DemandCause::ObservationRefresh,
            } => {
                assert!(
                    !self.seen.lock().expect("seen").contains(&task),
                    "DemandStarted for task {task} arrived after an event for the same task: a started demand must be announced before it runs"
                );
                self.refresh.store(task, Ordering::SeqCst);
            }
            irrigation_observe::RuntimeEvent::DemandFinished { task, .. }
                if self.refresh.load(Ordering::SeqCst) == task
                    && self.refresh.load(Ordering::SeqCst) != u64::MAX =>
            {
                self.park();
            }
            irrigation_observe::RuntimeEvent::TaskRunning { task }
            | irrigation_observe::RuntimeEvent::TaskWaiting { task, .. }
            | irrigation_observe::RuntimeEvent::TaskWoken { task, .. } => {
                self.seen.lock().expect("seen").insert(task);
            }
            _ => {}
        }
    }
}

#[cfg(feature = "observatory")]
impl ParkRefresh {
    fn park(&self) {
        if self.parked.swap(true, Ordering::SeqCst) {
            return;
        }
        // The demand has returned and released its permit. A refresh that read
        // its versions from here would pair them with the snapshot the demand
        // already captured; the staged pass inside the attempt is what keeps
        // the pair at one revision.
        self.request.send(()).expect("request the racing edit");
        let runtime = self.runtime.get().expect("the observer is wired");
        let committed = runtime.revision() + 1;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while runtime.revision() < committed {
            assert!(
                std::time::Instant::now() < deadline,
                "the racing edit did not commit while the refresh was parked"
            );
            std::thread::yield_now();
        }
    }
}

#[cfg(feature = "observatory")]
#[test]
fn a_refresh_pairs_its_snapshot_with_the_versions_of_that_revision() {
    let (request, wait_for_request) = std::sync::mpsc::channel::<()>();
    let observer = Arc::new(ParkRefresh {
        runtime: std::sync::OnceLock::new(),
        request,
        parked: AtomicBool::new(false),
        refresh: AtomicU64::new(u64::MAX),
        seen: std::sync::Mutex::new(std::collections::HashSet::new()),
    });
    let runtime = Runtime::with_observer(
        irrigation::RuntimeConfig::default(),
        Arc::clone(&observer) as Arc<dyn irrigation_observe::RuntimeObserver>,
    );
    observer
        .runtime
        .set(runtime.clone())
        .expect("the observer is wired before it runs");
    let Setup { runtime, .. } = setup_with(runtime);

    // The producer's constant unit forces the refresh to demand the root; the
    // host's source unit is the one the racing edit moves.
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_by_callback = Arc::clone(&seen);
    let _subscription = runtime
        .observe_units::<Source>(
            vec![
                UnitKey::field(Source::view_id(), FieldId(0), UnitKind::Value),
                UnitKey::field(Constant::view_id(), FieldId(0), UnitKind::Value),
            ],
            move |snapshot| {
                seen_by_callback.lock().expect("lock").push((
                    snapshot.revision(),
                    u64::from(snapshot.view::<Source>().value()),
                ));
            },
        )
        .expect("subscription");

    let editor = runtime.clone();
    let racer = std::thread::spawn(move || {
        wait_for_request
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the refresh parks");
        editor
            .edit(|edit| edit.view::<Source>().set_value(3))
            .expect("racing edit");
    });
    runtime
        .edit(|edit| edit.view::<Source>().set_value(2))
        .expect("edit");
    racer.join().expect("racer");

    let notifications = seen.lock().expect("lock").clone();
    let mut dialed_at = 0;
    for (revision, value) in &notifications {
        assert!(
            *revision > dialed_at,
            "a delivery went back in time: {notifications:?}"
        );
        assert_eq!(
            *value, *revision,
            "revision {revision} was delivered with value {value}"
        );
        dialed_at = *revision;
    }
    assert_eq!(
        notifications.last().copied(),
        Some((runtime.revision(), 3)),
        "the racing edit's own refresh delivered nothing: {notifications:?}"
    );
}

// ---------------------------------------------------------------------------
// A content replacement installs what it moved, at the revision that moved it
// ---------------------------------------------------------------------------

#[irrigation::tree_view(root = Doc, view = Script)]
pub mod script {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Doc {
        Module { items: Vec<Item> },
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Item {
        Num(i64),
    }
}

fn script_document() -> script::Doc {
    script::Doc::Module {
        items: vec![script::Item::Num(1)],
    }
}

fn script_root_unit() -> UnitKey {
    UnitKey::field(script::Script::view_id(), FieldId(0), UnitKind::Value)
}

fn script_children_unit(node: u64) -> UnitKey {
    UnitKey::entity(
        script::Script::view_id(),
        FieldId(irrigation::tree_slots::CHILDREN),
        UnitKind::Structure,
        irrigation::any_key((node, 0u16)),
    )
}

/// Reads a tree by unit id: the root cell and the first child slot of it.
///
/// This is the read shape a general component has — `GeneralCx::read` brings an
/// accessor's initializer for neither cell — so it is the one that must answer
/// from the committed content after a replacement.
struct TreeProbe;

impl GeneralComponent for TreeProbe {
    type State = ();
    type Output = (Option<u64>, usize);

    const ID: ComponentId = ComponentId(0x00C2_0001);
    const NAME: &'static str = "tests::TreeProbe";
    const VIEWS: &'static [&ViewDescriptor] = &[<script::Script as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Reset
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let root: Option<u64> = cx.read_typed(script_root_unit())?;
        let children = match root {
            Some(root) => {
                let slot: irrigation::ChildSlot = cx.read_typed(script_children_unit(root))?;
                slot.ids().len()
            }
            None => 0,
        };
        Ok(((), (root, children)))
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

#[test]
fn a_node_id_read_of_a_replaced_tree_cell_answers_the_replacement() {
    let runtime = Runtime::new();
    runtime
        .install_general(TreeProbe)
        .expect("the probe registers its View");
    runtime
        .edit(|edit| edit.view::<script::Script>().set_root(&script_document()))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs_general::<TreeProbe>()
            .expect("first read"),
        (Some(0), 1),
        "the probe materializes the root cell and the child slot"
    );

    // A whole-region replacement: one value holds the whole tree, so every cell
    // the probe materialized is a projection of it.
    runtime
        .edit(|edit| {
            let current = edit.content::<irrigation::TreeStorage>(script::Script::view_id());
            let mut next = (*current).clone();
            let moved = 7u64;
            next.nodes = next
                .nodes
                .insert(
                    moved,
                    irrigation::TreeNode {
                        type_index: 0,
                        variant: 0,
                        fields: Vec::new(),
                        children: vec![irrigation::ChildSlot::Many(vec![1])],
                        parent: None,
                    },
                )
                .0;
            next.root = Some(moved);
            next.next_id = moved + 1;
            edit.set_content(
                script::Script::view_id(),
                Arc::new(next),
                irrigation::tree_eq,
            );
        })
        .expect("a source tree View has no provider to refuse the replacement");

    assert_eq!(
        runtime
            .read_outputs_general::<TreeProbe>()
            .expect("a node-id read must answer the replaced cell"),
        (Some(7), 1),
        "the replaced root and child slot are this revision's answer"
    );
    let (root, children) = {
        let mut seen = (None, None);
        runtime
            .edit(|edit| {
                seen = (
                    edit.current::<Option<u64>>(&script_root_unit()),
                    edit.current::<irrigation::ChildSlot>(&script_children_unit(7)),
                );
            })
            .expect("a transaction that stages nothing");
        seen
    };
    assert_eq!(
        root,
        Some(Some(7)),
        "`Edit::current` must serve the payload the snapshot of that revision does"
    );
    assert_eq!(
        children,
        Some(irrigation::ChildSlot::Many(vec![1])),
        "a child slot the replacement moved is not the pre-replacement slot"
    );
}

#[test]
fn a_content_replacement_moves_the_change_version_of_the_cells_it_moved() {
    let runtime = Runtime::new();
    runtime.install(source_value_reader::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(3))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<source_value_reader::Root>()
            .expect("read"),
        3,
        "the reader materializes the field's value cell"
    );

    let unit = UnitKey::field(Source::view_id(), FieldId(0), UnitKind::Value);
    let seen: Arc<std::sync::Mutex<Vec<(irrigation::Revision, u32)>>> = Arc::default();
    let recorded = Arc::clone(&seen);
    let _subscription = runtime
        .observe_units::<Source>(vec![unit], move |snapshot| {
            recorded
                .lock()
                .expect("lock")
                .push((snapshot.revision(), snapshot.view::<Source>().value()));
        })
        .expect("subscription");

    runtime
        .edit(|edit| {
            edit.set_content(
                Source::view_id(),
                Arc::new(SourceContent { value: 4 }),
                irrigation::values_equal::<SourceContent>,
            );
        })
        .expect("a source View has no provider to refuse the replacement");
    let replaced = runtime.revision();
    assert_eq!(
        seen.lock().expect("lock").clone(),
        vec![(replaced, 4)],
        "a replacement that moves a cell must report it, at its own revision"
    );

    // A later read re-derives the cell; the change still belongs to the
    // replacement, so the cell must not be reported a second time.
    assert_eq!(
        runtime
            .read_outputs::<source_value_reader::Root>()
            .expect("re-read"),
        4
    );
    runtime
        .edit(|edit| edit.view::<Source>().set_value(5))
        .expect("edit");
    let delivered = runtime.revision();
    assert_eq!(
        seen.lock().expect("lock").clone(),
        vec![(replaced, 4), (delivered, 5)],
        "a lazy re-derivation must not stand in for a change of its own"
    );
}

#[test]
fn a_staged_cell_write_that_publishes_nothing_does_not_stage_the_delta() {
    let runtime = Runtime::new();
    runtime.install(source_value_reader::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(3))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<source_value_reader::Root>()
            .expect("read"),
        3
    );

    // The staged cell write holds the value the cell already has, so it writes
    // nothing; the replacement beside it owns the cell and must republish it.
    runtime
        .edit(|edit| {
            edit.set(
                UnitKey::field(Source::view_id(), FieldId(0), UnitKind::Value),
                3u32,
            );
            edit.set_content(
                Source::view_id(),
                Arc::new(SourceContent { value: 4 }),
                irrigation::values_equal::<SourceContent>,
            );
        })
        .expect("replace");
    assert_eq!(
        runtime.inspect_snapshot().view::<Source>().value(),
        4,
        "the content is the committed one"
    );
    assert_eq!(
        runtime
            .read_outputs::<source_value_reader::Root>()
            .expect("re-read"),
        4,
        "a write that publishes nothing cannot claim the cell the replacement owns"
    );
}

#[test]
fn a_staged_cell_write_is_normalized_once_against_the_content_it_found() {
    let runtime = Runtime::new();
    runtime.install(source_value_reader::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(3))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<source_value_reader::Root>()
            .expect("read"),
        3
    );

    // The same pair of writes as the gate above, staged in the other order.
    // Staging order is not part of a transaction's meaning: the cell write
    // restates the committed value, so the replacement owns the cell either
    // way.
    runtime
        .edit(|edit| {
            edit.set_content(
                Source::view_id(),
                Arc::new(SourceContent { value: 4 }),
                irrigation::values_equal::<SourceContent>,
            );
            edit.set(
                UnitKey::field(Source::view_id(), FieldId(0), UnitKind::Value),
                3u32,
            );
        })
        .expect("replace");
    let replaced = runtime.revision();
    assert_eq!(
        runtime.inspect_snapshot().view::<Source>().value(),
        4,
        "the content is the committed one"
    );
    assert_eq!(
        runtime
            .read_outputs::<source_value_reader::Root>()
            .expect("re-read"),
        4,
        "a write that publishes nothing cannot claim the cell the replacement owns"
    );
    assert_eq!(
        cell_version(
            &runtime,
            &UnitKey::field(Source::view_id(), FieldId(0), UnitKind::Value),
        ),
        replaced,
        "the replacement owns the cell's generation, not the restated write"
    );
}

#[test]
fn a_content_write_at_a_non_canonical_field_writes_only_its_unit() {
    let runtime = Runtime::new();
    runtime.install(source_value_reader::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(3))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<source_value_reader::Root>()
            .expect("read"),
        3
    );

    let fired = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&fired);
    let _subscription = runtime
        .observe::<Source>(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("a whole-View subscription");
    let content_version = runtime.inspect_snapshot().version::<Source>();

    // A content-kind key that is not the View's field-0 content cell names only
    // its own unit: `set_content` owns the View's content, and this write must
    // not swap it behind the content cell's back.
    let stray = UnitKey::field(Source::view_id(), FieldId(1), UnitKind::Content);
    let write_stray = |runtime: &Runtime, value: u32| {
        runtime
            .edit(|edit| {
                edit.set_value(
                    stray.clone(),
                    Arc::new(SourceContent { value }),
                    irrigation::values_equal::<SourceContent>,
                );
            })
            .expect("the fallback writes the unit it names");
    };
    write_stray(&runtime, 9);
    assert_eq!(
        runtime.inspect_snapshot().view::<Source>().value(),
        3,
        "the View's content is not this key's"
    );
    assert_eq!(
        runtime.inspect_snapshot().version::<Source>(),
        content_version,
        "the content cell did not move"
    );
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "a whole-View subscriber must not hear about a unit that is not the View's content"
    );
    assert_eq!(
        runtime
            .read_outputs::<source_value_reader::Root>()
            .expect("re-read"),
        3,
        "the field's cell is untouched"
    );

    // A canonical replacement owns the View's content. The stray unit still
    // holds 9, so writing it again publishes nothing — and must not silently
    // revert the content the replacement committed.
    runtime
        .edit(|edit| {
            edit.set_content(
                Source::view_id(),
                Arc::new(SourceContent { value: 5 }),
                irrigation::values_equal::<SourceContent>,
            );
        })
        .expect("replace");
    assert_eq!(runtime.inspect_snapshot().view::<Source>().value(), 5);
    let replaced = runtime.revision();
    write_stray(&runtime, 9);
    assert_eq!(
        runtime.revision(),
        replaced,
        "the repeated write publishes nothing"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Source>().value(),
        5,
        "a write that publishes nothing cannot move the View's content"
    );
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the replacement is the only change the View heard"
    );
}

/// Replaces the content of a View it read: a self-dependent program.
///
/// A root that declares no output Views may replace any registered View's
/// content, so nothing but the kernel's self-dependency rule stands between
/// this body and a write it derived from the state it replaces.
struct Feedback;

impl GeneralComponent for Feedback {
    type State = ();
    type Output = u32;

    const ID: ComponentId = ComponentId(0x00C2_0002);
    const NAME: &'static str = "tests::Feedback";
    const VIEWS: &'static [&ViewDescriptor] = &[<Source as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u32 = cx.read_typed(UnitKey::field(
            Source::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        cx.set_content(
            Source::view_id(),
            Arc::new(SourceContent { value: value + 1 }),
        );
        Ok(((), value))
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

#[test]
fn a_general_root_that_replaces_the_content_it_read_is_self_dependent() {
    let runtime = Runtime::new();
    runtime.install_general(Feedback).expect("install");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(3))
        .expect("seed");

    let error = runtime
        .read_outputs_general::<Feedback>()
        .expect_err("a body that replaces the content of the cell it read must fail");
    assert!(
        matches!(
            error,
            irrigation::ComputeError::PreconditionUnmet {
                reason: irrigation_core::error::PreconditionReason::SelfDependent,
                ..
            }
        ),
        "reading a cell of the content a body replaces is read-your-own-write: {error:?}"
    );
    assert!(
        error.is_permanent(),
        "the rule is a program error, not a retryable attempt: {error:?}"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Source>().value(),
        3,
        "the rejected replacement must not publish"
    );
    let repeated = runtime
        .read_outputs_general::<Feedback>()
        .expect_err("the diagnosed program must stay diagnosed");
    assert!(
        matches!(
            repeated,
            irrigation::ComputeError::PreconditionUnmet {
                reason: irrigation_core::error::PreconditionReason::SelfDependent,
                ..
            }
        ),
        "a repeated demand must replay the failure instead of serving f(old): {repeated:?}"
    );
}

/// Chooses the later provider, so a late install keeps the unit.
///
/// The winner is remembered: a replay re-runs both providers, and the later one
/// is then the earlier claimant of the pair.
struct PreferLast {
    winner: std::sync::Mutex<Option<irrigation::NodeId>>,
}

impl irrigation::RouteSelector for PreferLast {
    fn select(
        &self,
        _unit: &irrigation::UnitKey,
        _first: irrigation::NodeId,
        second: irrigation::NodeId,
    ) -> irrigation::NodeId {
        *self
            .winner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert(second)
    }
}

/// The runtime a late resolver installs its selector on, once.
static LAST_ROUTE: std::sync::Mutex<Option<Runtime>> = std::sync::Mutex::new(None);

/// Installs a later-provider selector between the late install and the resolve.
fn install_last_selector() {
    let mut slot = LAST_ROUTE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(runtime) = slot.take() {
        runtime.set_route_selector(Arc::new(PreferLast {
            winner: std::sync::Mutex::new(None),
        }));
    }
}

#[irrigation::irrigation]
mod contested_overtake {
    use super::{Contested, install_last_selector};

    #[component]
    pub fn first(tag: u64) -> u64 {
        let _ = tag;
        Contested.define_field0(1);
        1
    }

    #[component]
    pub fn late(tag: u64) -> u64 {
        let _ = tag;
        Contested.define_field0(2);
        2
    }

    #[component]
    pub fn reader(tag: u64) -> u64 {
        let _ = tag;
        Contested.field0()
    }

    #[component]
    pub fn root() -> (u64, u64) {
        // Both providers run in the body, so both of their results are values
        // this body publishes. The selector lands after the late install and
        // before the demand resolves the conflict it staged.
        let earlier = first(0);
        let later = late(0);
        install_last_selector();
        (earlier, later)
    }
}

#[test]
fn a_winner_that_retires_a_provider_after_the_last_commit_replays_the_demand() {
    let runtime = Runtime::new();
    runtime
        .install(contested_overtake::Root)
        .expect("install the contested root");
    *LAST_ROUTE.lock().expect("last route") = Some(runtime.clone());
    let retries = runtime.stats().demand_retries;

    let (earlier, later) = runtime
        .read_outputs::<contested_overtake::Root>()
        .expect("the demand replays and settles on the later provider");
    assert_eq!(
        (earlier, later),
        (1, 2),
        "the projections must serve the values the replayed body published"
    );
    assert!(
        runtime.stats().demand_retries > retries,
        "a resolution that discards a value this revision published must report a retry"
    );
    assert_eq!(
        runtime.snapshot().view::<Contested>().field0(),
        2,
        "the later provider's value is the committed one"
    );
    assert_eq!(
        runtime
            .call::<contested_overtake::__ReaderSpec>(contested_overtake::__ReaderArgs { tag: 0 })
            .expect("a reader at the same revision agrees"),
        2
    );
}

#[irrigation::view(source)]
pub struct Register(irrigation::Map<u32, u32>);

#[irrigation::irrigation]
mod presence_guard {
    use super::Register;

    /// Reads the key's presence, then stages the payload only when it is
    /// present: the plan's `Skip` outcome, which a conditional write verb is
    /// built from.
    /// The shape a conditional write verb has: one call reads the key's
    /// presence and stages the write the read decided.
    fn guarded_update() {
        if Register.field0().contains_key(&1) {
            Register.field0().update(1, 11, "map_update");
        }
    }

    #[component]
    pub fn presence() -> u32 {
        guarded_update();
        0
    }

    /// Reads the payload of the key it stages: read-your-own-write.
    #[component]
    pub fn payload() -> u32 {
        let base = Register.field0().get(&1).unwrap_or(0);
        Register.field0().update(1, base + 1, "map_update");
        base
    }
}

#[test]
fn a_group_may_read_the_presence_of_the_entity_it_stages() {
    let runtime = Runtime::new();
    runtime.register_view(<Register as ViewSchema>::DESCRIPTOR);
    runtime.install(presence_guard::Presence).expect("install");
    assert_eq!(
        runtime
            .read_outputs::<presence_guard::Presence>()
            .expect("a presence read is not read-your-own-write"),
        0
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Register>().field0(),
        Vec::new(),
        "absent is a skip: the group stages nothing"
    );

    // The key appears at a later revision. The site observed its absence, so it
    // must re-decide instead of serving the skip for ever.
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Register>();
            let mut entries = view.field0();
            entries.insert(1u32, 10u32);
        })
        .expect("host edit");
    assert_eq!(
        runtime
            .read_outputs::<presence_guard::Presence>()
            .expect("re-decide"),
        0
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Register>().field0(),
        vec![(1u32, 11u32)],
        "the conditional write happens once the key it observed absent appears"
    );
}

#[test]
fn a_group_that_reads_the_payload_of_the_entity_it_stages_is_self_dependent() {
    let runtime = Runtime::new();
    runtime.register_view(<Register as ViewSchema>::DESCRIPTOR);
    runtime
        .edit(|edit| {
            let mut view = edit.view::<Register>();
            let mut entries = view.field0();
            entries.insert(1u32, 10u32);
        })
        .expect("seed the payload the body reads");
    runtime.install(presence_guard::Payload).expect("install");
    let error = runtime
        .read_outputs::<presence_guard::Payload>()
        .expect_err("a body that reads the payload its own write installs must fail");
    assert!(
        matches!(
            error,
            irrigation::ComputeError::PreconditionUnmet {
                reason: irrigation_core::error::PreconditionReason::SelfDependent,
                ..
            }
        ),
        "the presence exemption must not extend to the payload: {error:?}"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Register>().field0(),
        vec![(1u32, 10u32)],
        "the rejected group must not publish"
    );
}

// ---------------------------------------------------------------------------
// A general root owns the root scope its own frame runs in
// (r10-runtime-state-01)
// ---------------------------------------------------------------------------

#[irrigation::view(derived)]
pub struct RootOwned(u64);

/// A general root that publishes one derived View unit and claims the field.
struct OwningRoot;

impl GeneralComponent for OwningRoot {
    type State = ();
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00C1_0010);
    const NAME: &'static str = "tests::OwningRoot";
    const VIEWS: &'static [&ViewDescriptor] = &[<RootOwned as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let view = <RootOwned as ViewSchema>::view_id();
        cx.claim_scope(
            irrigation_core::descriptor::WriteScope::Field(view, FieldId(0)),
            "tests::OwningRoot",
        );
        cx.output().field(view, FieldId(0), 7u64);
        Ok(((), 7))
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

fn root_owned_unit() -> UnitKey {
    UnitKey::field(
        <RootOwned as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
    )
}

#[test]
fn a_plan_naming_a_general_root_is_accepted_by_its_own_definitions() {
    let runtime = Runtime::new();
    runtime.install_general(OwningRoot).expect("general root");
    runtime.plan_route([(root_owned_unit(), <OwningRoot as GeneralComponent>::ID)]);
    assert_eq!(
        runtime
            .read_outputs_general::<OwningRoot>()
            .expect("the plan names the component the definition is attributed to"),
        7
    );
}

#[test]
fn a_general_root_claim_is_in_scope_under_strict_arbitration() {
    use irrigation::WriteArbitration;

    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        write_arbitration: WriteArbitration::Strict,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.install_general(OwningRoot).expect("general root");
    runtime.plan_route([(root_owned_unit(), <OwningRoot as GeneralComponent>::ID)]);
    assert_eq!(
        runtime
            .read_outputs_general::<OwningRoot>()
            .expect("a claim from the root that owns the scope is in scope"),
        7
    );
}

// ---------------------------------------------------------------------------
// An observation refresh demands the producers its subscriptions watch
// (r10-runtime-state-02)
// ---------------------------------------------------------------------------

#[irrigation::view(source)]
pub struct SideInput {
    pub value: u32,
}

#[irrigation::view(derived)]
pub struct Side(u32);

/// A producer the installed root never reads.
struct SideProducer;

impl GeneralComponent for SideProducer {
    type State = u32;
    type Output = u32;

    const ID: ComponentId = ComponentId(0x00C1_0011);
    const NAME: &'static str = "tests::SideProducer";
    const VIEWS: &'static [&ViewDescriptor] = &[<SideInput as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[<Side as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u32 = cx.read_typed(UnitKey::field(
            SideInput::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        cx.output().field(Side::view_id(), FieldId(0), value);
        Ok((value, value))
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

fn side_unit() -> UnitKey {
    UnitKey::field(Side::view_id(), FieldId(0), UnitKind::Value)
}

#[test]
fn a_unit_subscription_on_a_producer_only_runtime_runs_the_producer() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(SideProducer)
        .expect("producer");
    runtime
        .edit(|edit| edit.view::<SideInput>().set_value(1))
        .expect("seed the producer's input");

    let fired = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let counter = Arc::clone(&fired);
    let sink = Arc::clone(&seen);
    let _subscription = runtime
        .observe_units::<Side>(vec![side_unit()], move |snapshot| {
            counter.fetch_add(1, Ordering::SeqCst);
            sink.lock()
                .expect("lock")
                .push(snapshot.view::<Side>().field0());
        })
        .expect("observe the produced unit");

    runtime
        .edit(|edit| edit.view::<SideInput>().set_value(5))
        .expect("edit the producer's input");
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "a producer-only runtime must run the producer the subscription watches"
    );
    assert_eq!(
        *seen.lock().expect("lock"),
        vec![5],
        "the callback reads the value the producer published"
    );
}

#[test]
fn a_unit_subscription_on_a_producer_the_root_never_reads_runs_it() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(Producer {
            runs: Arc::new(AtomicU32::new(0)),
        })
        .expect("producer");
    runtime
        .install_general(ConstantReader {
            runs: Arc::new(AtomicU32::new(0)),
        })
        .expect("root");
    runtime
        .install_general_component(SideProducer)
        .expect("side producer");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(1))
        .expect("seed the root's input");
    let _ = runtime
        .read_outputs_general::<ConstantReader>()
        .expect("root run");

    let fired = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let counter = Arc::clone(&fired);
    let sink = Arc::clone(&seen);
    let _subscription = runtime
        .observe_units::<Side>(vec![side_unit()], move |snapshot| {
            counter.fetch_add(1, Ordering::SeqCst);
            sink.lock()
                .expect("lock")
                .push(snapshot.view::<Side>().field0());
        })
        .expect("observe the produced unit");

    runtime
        .edit(|edit| edit.view::<SideInput>().set_value(5))
        .expect("edit the producer the root does not reach");
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the refresh must demand the watched unit's producer, not only the root"
    );
    assert_eq!(*seen.lock().expect("lock"), vec![5]);
}

#[test]
fn a_whole_view_subscription_on_a_producer_only_runtime_runs_the_producer() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(SideProducer)
        .expect("producer");
    runtime
        .edit(|edit| edit.view::<SideInput>().set_value(1))
        .expect("seed the producer's input");

    let fired = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let counter = Arc::clone(&fired);
    let sink = Arc::clone(&seen);
    let _subscription = runtime
        .observe::<Side>(move |snapshot| {
            counter.fetch_add(1, Ordering::SeqCst);
            sink.lock()
                .expect("lock")
                .push(snapshot.view::<Side>().field0());
        })
        .expect("observe the produced View");

    runtime
        .edit(|edit| edit.view::<SideInput>().set_value(5))
        .expect("edit the producer's input");
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "a whole-View subscription must demand the View's producer without a root"
    );
    assert_eq!(*seen.lock().expect("lock"), vec![5]);
}

// ---------------------------------------------------------------------------
// A replacement at the reading revision invalidates the observation it moves
// (runtime-kernel-r10-02)
// ---------------------------------------------------------------------------

#[irrigation::irrigation]
mod same_revision_reader {
    use super::Source;

    #[component]
    pub fn read(tag: u64) -> u32 {
        let _ = tag;
        Source.value()
    }
}

/// Replaces a View's content without reading any of its cells.
struct ContentReplacer;

impl GeneralComponent for ContentReplacer {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C2_0003);
    const NAME: &'static str = "tests::ContentReplacer";
    const VIEWS: &'static [&ViewDescriptor] = &[<Source as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        cx.set_content(Source::view_id(), Arc::new(SourceContent { value: 99 }));
        Ok(((), ()))
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

#[test]
fn a_replacement_at_the_reading_revision_invalidates_the_observation_it_moves() {
    let runtime = Runtime::new();
    runtime.install_general(ContentReplacer).expect("install");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(10))
        .expect("seed");
    let revision = runtime.revision();
    assert_eq!(
        runtime
            .call::<same_revision_reader::__ReadSpec>(same_revision_reader::__ReadArgs { tag: 0 })
            .expect("read"),
        10,
        "the reader certifies the cell at this revision"
    );

    // The replacement is published by a computation, so it lands at the same
    // revision the reader certified against.
    runtime
        .read_outputs_general::<ContentReplacer>()
        .expect("replace the content");
    assert_eq!(
        runtime.revision(),
        revision,
        "a content publication does not move the revision"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Source>().value(),
        99,
        "the committed content is the replacement"
    );
    assert_eq!(
        runtime
            .call::<same_revision_reader::__ReadSpec>(same_revision_reader::__ReadArgs { tag: 0 })
            .expect("re-read"),
        99,
        "an observation recorded before the replacement must not validate as unchanged"
    );
}

// ---------------------------------------------------------------------------
// A replacement owes a provider-owned cell nothing (runtime-api-r10-02)
// ---------------------------------------------------------------------------

#[irrigation::view(source)]
pub struct KeptCell {
    pub stable: u32,
    pub host: u32,
}

/// Settles the first field once and never reads anything, so the cell it owns
/// keeps the version its own install gave it.
struct StableSettler;

impl GeneralComponent for StableSettler {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C2_0004);
    const NAME: &'static str = "tests::StableSettler";
    const VIEWS: &'static [&ViewDescriptor] = &[<KeptCell as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        cx.output().field(KeptCell::view_id(), FieldId(0), 1u32);
        Ok(((), ()))
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

#[test]
fn a_replacement_that_keeps_a_provider_cell_does_not_move_its_version() {
    let runtime = Runtime::new();
    runtime.install_general(StableSettler).expect("install");
    runtime
        .read_outputs_general::<StableSettler>()
        .expect("the provider settles its cell");
    // The host's own cell, materialized so the replacement has one to move.
    runtime
        .edit(|edit| edit.view::<KeptCell>().set_host(2))
        .expect("host edit");

    let stable = UnitKey::field(KeptCell::view_id(), FieldId(0), UnitKind::Value);
    let host = UnitKey::field(KeptCell::view_id(), FieldId(1), UnitKind::Value);
    // A cell's change version is the revision it last changed at; the dump is
    // the one read-only surface that answers it without the observation path.
    let version_of = |runtime: &Runtime, unit: &UnitKey| {
        let key = format!("{unit:?}");
        runtime
            .dump()
            .into_iter()
            .find(|node| node.unit.as_deref() == Some(key.as_str()))
            .map(|node| node.changed_at)
            .expect("the cell is materialized")
    };
    let settled = version_of(&runtime, &stable);
    let fired = Arc::new(AtomicBool::new(false));
    let recorded = Arc::clone(&fired);
    let _subscription = runtime
        .observe_units::<KeptCell>(vec![stable.clone()], move |_| {
            recorded.store(true, Ordering::SeqCst);
        })
        .expect("subscription");

    runtime
        .edit(|edit| {
            edit.set_content(
                KeptCell::view_id(),
                Arc::new(KeptCellContent { stable: 1, host: 5 }),
                irrigation::values_equal::<KeptCellContent>,
            );
        })
        .expect("a replacement that keeps the provider's cell");
    let replaced = runtime.revision();
    assert!(
        !fired.load(Ordering::SeqCst),
        "a provider-owned cell the replacement kept must not notify its readers"
    );
    assert_eq!(
        version_of(&runtime, &stable),
        settled,
        "the replacement did not publish that cell, so its change version must not move"
    );
    assert_eq!(
        version_of(&runtime, &host),
        replaced,
        "the host cell the replacement moved changes at the replacement's revision"
    );
    let moved = {
        let mut seen = None;
        runtime
            .edit(|edit| seen = edit.current::<u32>(&host))
            .expect("a transaction that stages nothing");
        seen
    };
    assert_eq!(
        moved,
        Some(5),
        "the host cell the replacement moved is re-derived from the new content"
    );
}

// ---------------------------------------------------------------------------
// A lazy fill publishes the generation the replacement opened
// (runtime-kernel-r11-01, runtime-api-r11-02)
// ---------------------------------------------------------------------------

/// One unit's change version, read from the read-only dump.
fn cell_version(runtime: &Runtime, unit: &UnitKey) -> u64 {
    let key = format!("{unit:?}");
    runtime
        .dump()
        .into_iter()
        .find(|node| node.unit.as_deref() == Some(key.as_str()))
        .map(|node| node.changed_at)
        .expect("the cell is materialized")
}

/// Publishes a revision that touches no cell of the View under test.
fn publish_noise(runtime: &Runtime, noise: u32) {
    let unit = UnitKey::field(KeptCell::view_id(), FieldId(0), UnitKind::Value);
    runtime
        .edit(|edit| edit.set(unit, noise))
        .expect("an unrelated publishing edit");
}

#[test]
fn a_late_read_fills_the_cell_without_making_a_change_of_its_own() {
    let runtime = Runtime::new();
    runtime.install(source_value_reader::Root).expect("install");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(3))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<source_value_reader::Root>()
            .expect("read"),
        3,
        "the reader materializes the field's value cell"
    );

    let unit = UnitKey::field(Source::view_id(), FieldId(0), UnitKind::Value);
    let seen: Arc<std::sync::Mutex<Vec<(irrigation::Revision, u32)>>> = Arc::default();
    let recorded = Arc::clone(&seen);
    let _subscription = runtime
        .observe_units::<Source>(vec![unit.clone()], move |snapshot| {
            recorded
                .lock()
                .expect("lock")
                .push((snapshot.revision(), snapshot.view::<Source>().value()));
        })
        .expect("subscription");

    runtime
        .edit(|edit| {
            edit.set_content(
                Source::view_id(),
                Arc::new(SourceContent { value: 4 }),
                irrigation::values_equal::<SourceContent>,
            );
        })
        .expect("a source View has no provider to refuse the replacement");
    let replaced = runtime.revision();
    assert_eq!(
        seen.lock().expect("lock").clone(),
        vec![(replaced, 4)],
        "the replacement moved the cell it owns"
    );

    // Nothing reads the replaced cell at the revision that replaced it, so the
    // derivation happens at this later revision.
    publish_noise(&runtime, 1);
    assert_eq!(
        runtime
            .read_outputs::<source_value_reader::Root>()
            .expect("re-read"),
        4,
        "the late read answers the replacement's content"
    );
    assert_eq!(
        cell_version(&runtime, &unit),
        replaced,
        "the fill keeps the revision the replacement stamped"
    );

    publish_noise(&runtime, 2);
    assert_eq!(
        seen.lock().expect("lock").clone(),
        vec![(replaced, 4)],
        "a lazy re-derivation must not stand in for a change of its own"
    );
}

// ---------------------------------------------------------------------------
// A generation opened after a retraction marks the readers that re-certified
// since (runtime-kernel-r11-03)
// ---------------------------------------------------------------------------

fn cache_unit(kind: UnitKind) -> UnitKey {
    UnitKey::entity(
        Cache::view_id(),
        FieldId(0),
        kind,
        irrigation::any_key(1u32),
    )
}

#[irrigation::view(derived)]
pub struct ProbeOut {
    pub value: u64,
}

/// Reads one entity's payload cell and nothing else, so the payload's
/// retraction is its only dependency. It is a producer rather than a root
/// because the replacement beside it has to hold the root slot.
struct PayloadProbe;

impl GeneralComponent for PayloadProbe {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C3_0001);
    const NAME: &'static str = "tests::PayloadProbe";
    const VIEWS: &'static [&ViewDescriptor] = &[<Cache as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[<ProbeOut as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value = match cx.read_typed::<u32>(cache_unit(UnitKind::Value)) {
            Ok(value) => u64::from(value),
            Err(irrigation::ComputeError::Absent { .. }) => 0,
            Err(error) => return Err(error),
        };
        cx.output().field(ProbeOut::view_id(), FieldId(0), value);
        Ok(((), ()))
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

#[irrigation::irrigation]
mod probe_reader {
    use super::ProbeOut;

    #[component]
    pub fn read(tag: u64) -> u64 {
        let _ = tag;
        ProbeOut.value()
    }
}

/// Re-adds one entry by replacing the View's whole content, reading no cell.
struct EntryRestorer;

impl GeneralComponent for EntryRestorer {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C3_0002);
    const NAME: &'static str = "tests::EntryRestorer";
    const VIEWS: &'static [&ViewDescriptor] = &[<Cache as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let (entries, _) = irrigation::PMap::new().insert(1u32, 99u32);
        cx.set_content(
            Cache::view_id(),
            Arc::new(CacheContent {
                field0: entries.into(),
            }),
        );
        Ok(((), ()))
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

#[test]
fn a_generation_opened_after_a_retraction_marks_the_reader_that_recertified() {
    let runtime = Runtime::new();
    runtime.install_general(EntryRestorer).expect("restorer");
    runtime
        .install_general_component(PayloadProbe)
        .expect("probe");
    // Each tag is a fresh invocation, so every call runs the reader body and
    // reaches the probe instead of reusing an earlier answer.
    let probe = |tag: u64| {
        runtime
            .call::<probe_reader::__ReadSpec>(probe_reader::__ReadArgs { tag })
            .expect("probe run")
    };

    runtime
        .edit(|edit| edit.view::<Cache>().field0().insert(1u32, 10u32))
        .expect("insert");
    assert_eq!(probe(0), 10, "the probe reads the installed payload");

    runtime
        .edit(|edit| edit.view::<Cache>().field0().remove(&1u32))
        .expect("remove");
    let removed = runtime.revision();
    assert_eq!(
        probe(1),
        0,
        "the probe re-certifies at this revision against the retraction"
    );

    // The replacement is published by a computation, so it lands at the
    // revision the probe certified against, and it re-adds the entry.
    runtime
        .read_outputs_general::<EntryRestorer>()
        .expect("replace");
    assert_eq!(
        runtime.revision(),
        removed,
        "a content publication does not move the revision"
    );
    assert_eq!(
        probe(2),
        99,
        "a reader that re-certified after the retraction must hear about the generation that re-adds the entry"
    );
}

// ---------------------------------------------------------------------------
// A unit that appears through committed content reports itself once
// (runtime-kernel-r11-02)
// ---------------------------------------------------------------------------

#[irrigation::irrigation]
mod cache_entry {
    use super::Cache;

    #[component]
    pub fn read(tag: u64) -> (bool, Option<u32>) {
        let _ = tag;
        let entries = Cache.field0();
        (entries.contains_key(&1), entries.get(&1))
    }
}

#[irrigation::irrigation]
mod cache_missing_entry {
    use super::Cache;

    #[component]
    pub fn read(tag: u64) -> (bool, Option<u32>) {
        let _ = tag;
        let entries = Cache.field0();
        (entries.contains_key(&2), entries.get(&2))
    }
}

#[test]
fn a_unit_that_appears_through_committed_content_notifies_its_subscription() {
    let runtime = Runtime::new();
    runtime.register_view(<Cache as ViewSchema>::DESCRIPTOR);
    let fired = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&fired);
    let _subscription = runtime
        .observe_units::<Cache>(
            vec![
                cache_unit(UnitKind::Membership),
                cache_unit(UnitKind::Value),
            ],
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            },
        )
        .expect("subscription");

    let (entries, _) = irrigation::PMap::new().insert(1u32, 10u32);
    runtime
        .edit(|edit| {
            edit.set_content(
                Cache::view_id(),
                Arc::new(CacheContent {
                    field0: entries.into(),
                }),
                irrigation::values_equal::<CacheContent>,
            );
        })
        .expect("a replacement with no cell write beside it");
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the entry appeared at the replacement, so the subscription hears about it there"
    );

    // The read materializes the entry's cells at the revision the content
    // changed at. The appearance already belongs to that revision, so the read
    // must not report it a second time.
    assert_eq!(
        runtime
            .call::<cache_entry::__ReadSpec>(cache_entry::__ReadArgs { tag: 0 })
            .expect("read"),
        (true, Some(10)),
        "the read projects the entry the content gained"
    );
    publish_noise(&runtime, 1);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "materializing a unit the content already held is not a change of its own"
    );
    publish_noise(&runtime, 2);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "a materialized unchanged unit is not reported again"
    );
}

#[test]
fn a_unit_the_content_already_held_is_not_reported_as_new() {
    let runtime = Runtime::new();
    runtime.register_view(<Cache as ViewSchema>::DESCRIPTOR);
    let (entries, _) = irrigation::PMap::new().insert(1u32, 10u32);
    runtime
        .edit(|edit| {
            edit.set_content(
                Cache::view_id(),
                Arc::new(CacheContent {
                    field0: entries.into(),
                }),
                irrigation::values_equal::<CacheContent>,
            );
        })
        .expect("the content is committed before any cell is read");

    let fired = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&fired);
    let _subscription = runtime
        .observe_units::<Cache>(
            vec![
                cache_unit(UnitKind::Membership),
                cache_unit(UnitKind::Value),
            ],
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            },
        )
        .expect("subscription");

    // The entry's cells are materialized only now, by this read.
    assert_eq!(
        runtime
            .call::<cache_entry::__ReadSpec>(cache_entry::__ReadArgs { tag: 0 })
            .expect("read"),
        (true, Some(10))
    );
    publish_noise(&runtime, 1);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "a unit the content already held when it was subscribed must not look new"
    );
}

#[test]
fn a_unit_the_content_already_held_does_not_move_before_its_first_read() {
    let runtime = Runtime::new();
    runtime.register_view(<Cache as ViewSchema>::DESCRIPTOR);
    let (entries, _) = irrigation::PMap::new().insert(1u32, 10u32);
    runtime
        .edit(|edit| {
            edit.set_content(
                Cache::view_id(),
                Arc::new(CacheContent {
                    field0: entries.into(),
                }),
                irrigation::values_equal::<CacheContent>,
            );
        })
        .expect("the content is committed before any cell is read");

    let fired = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&fired);
    let _subscription = runtime
        .observe_units::<Cache>(
            vec![
                cache_unit(UnitKind::Membership),
                cache_unit(UnitKind::Value),
            ],
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            },
        )
        .expect("subscription");

    // Neither unit has a cell yet: the committed content is the answer for
    // both, and it has not moved since the subscription was seeded from it.
    publish_noise(&runtime, 1);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "a unit the content already held at subscription time must not look new"
    );

    // The read materializes both cells at the revision the content changed at,
    // which is the revision the subscription is already seeded with.
    assert_eq!(
        runtime
            .call::<cache_entry::__ReadSpec>(cache_entry::__ReadArgs { tag: 0 })
            .expect("read"),
        (true, Some(10))
    );
    publish_noise(&runtime, 2);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "materializing an unchanged unit is not a change of its own"
    );
}

#[test]
fn a_watched_unit_keeps_its_version_through_a_reclaim() {
    let runtime = Runtime::new();
    runtime.register_view(<Cache as ViewSchema>::DESCRIPTOR);
    let (entries, _) = irrigation::PMap::new().insert(1u32, 10u32);
    runtime
        .edit(|edit| {
            edit.set_content(
                Cache::view_id(),
                Arc::new(CacheContent {
                    field0: entries.into(),
                }),
                irrigation::values_equal::<CacheContent>,
            );
        })
        .expect("the content is committed");
    assert_eq!(
        runtime
            .call::<cache_entry::__ReadSpec>(cache_entry::__ReadArgs { tag: 0 })
            .expect("read"),
        (true, Some(10)),
        "the read materializes the entry's cells from the content"
    );

    let membership = cache_unit(UnitKind::Membership);
    let fired = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&fired);
    let _subscription = runtime
        .observe_units::<Cache>(
            vec![membership.clone(), cache_unit(UnitKind::Value)],
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            },
        )
        .expect("subscription");

    publish_noise(&runtime, 1);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "the entry has not moved since the subscription was seeded"
    );

    // The retraction mutates the presence cell alone; its payload cell goes
    // with it.
    runtime
        .edit(|edit| edit.set(membership.clone(), false))
        .expect("retract through the membership cell alone");
    let retracted = runtime.revision();
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the retraction is a change the subscription hears"
    );
    assert_eq!(
        cell_version(&runtime, &membership),
        retracted,
        "the retracted presence cell is stamped with the published revision"
    );

    runtime.collect();
    publish_noise(&runtime, 2);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "a reclaim loses reuse, not the change version the subscription recorded"
    );
}

#[test]
fn a_watched_absent_unit_does_not_move_before_or_after_its_first_read() {
    let runtime = Runtime::new();
    runtime.register_view(<Cache as ViewSchema>::DESCRIPTOR);
    let (entries, _) = irrigation::PMap::new().insert(1u32, 10u32);
    runtime
        .edit(|edit| {
            edit.set_content(
                Cache::view_id(),
                Arc::new(CacheContent {
                    field0: entries.into(),
                }),
                irrigation::values_equal::<CacheContent>,
            );
        })
        .expect("the content is committed");

    let membership = UnitKey::entity(
        Cache::view_id(),
        FieldId(0),
        UnitKind::Membership,
        irrigation::any_key(2u32),
    );
    let value = UnitKey::entity(
        Cache::view_id(),
        FieldId(0),
        UnitKind::Value,
        irrigation::any_key(2u32),
    );
    let fired = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&fired);
    let _subscription = runtime
        .observe_units::<Cache>(vec![membership, value], move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("subscription");

    // The key is absent from the committed content, and its value unit has no
    // projection at all; neither has moved.
    publish_noise(&runtime, 1);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "an absent unit must not be reported as new"
    );

    assert_eq!(
        runtime
            .call::<cache_missing_entry::__ReadSpec>(cache_missing_entry::__ReadArgs { tag: 0 })
            .expect("read"),
        (false, None),
        "the read confirms the absence"
    );
    publish_noise(&runtime, 2);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "materializing an absence the content already held is not a change"
    );

    runtime.collect();
    publish_noise(&runtime, 3);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "reclaiming an unchanged watched absence is not a change"
    );
}

// ---------------------------------------------------------------------------
// A presence-only retraction is a published change
// (runtime-api-r11-01)
// ---------------------------------------------------------------------------

#[test]
fn a_presence_only_retraction_publishes_and_notifies() {
    let runtime = Runtime::new();
    runtime.register_view(<Cache as ViewSchema>::DESCRIPTOR);
    let value = cache_unit(UnitKind::Value);
    let membership = cache_unit(UnitKind::Membership);
    // The payload alone: the editor stages no presence beside it, so no
    // membership cell exists for the retraction to clear.
    runtime
        .edit(|edit| edit.set(value.clone(), 10u32))
        .expect("payload");
    let installed = runtime.revision();

    let fired = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&fired);
    let _subscription = runtime
        .observe_units::<Cache>(vec![value.clone(), membership.clone()], move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("subscription");

    runtime
        .edit(|edit| edit.set(membership, false))
        .expect("retract through the membership cell alone");
    let retracted = runtime.revision();
    assert_eq!(
        retracted,
        installed + 1,
        "a retraction that mutates a cell publishes the revision it stamps"
    );
    assert_eq!(
        cell_version(&runtime, &value),
        retracted,
        "the retracted cell is stamped with the published revision"
    );
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the subscription on the removed cell must be told"
    );

    let current = {
        let mut seen = None;
        runtime
            .edit(|edit| seen = edit.current::<u32>(&value))
            .expect("a transaction that stages nothing");
        seen
    };
    assert_eq!(
        current, None,
        "a retracted payload is gone, not a stale value"
    );
}

// ---------------------------------------------------------------------------
// A retraction's readers are the pass's result, not its seeds
// (runtime-kernel-r11-04)
// ---------------------------------------------------------------------------

#[irrigation::view(derived)]
pub struct RetryPort(irrigation::Map<u32, u32>);

fn retry_port_unit(key: u32) -> UnitKey {
    UnitKey::entity(
        RetryPort::view_id(),
        FieldId(0),
        UnitKind::Value,
        irrigation::any_key(key),
    )
}

#[irrigation::irrigation]
mod late_retraction {
    use super::{RetryPort, retry_port_unit};
    use irrigation::ViewSchema;

    /// Installs the port a later call retracts. A plain helper per group: a
    /// component statement would give the requirement and the retraction their
    /// own write groups.
    fn install_group() {
        irrigation::ensure_view(<RetryPort as ViewSchema>::DESCRIPTOR);
        irrigation::define_unit(retry_port_unit(1), 42u32, "port");
    }

    /// Defers a group that retracts the port once the driver appears.
    fn stage_group() {
        irrigation::ensure_view(<RetryPort as ViewSchema>::DESCRIPTOR);
        irrigation::require_condition(false, &[retry_port_unit(9)], "driver present");
        irrigation::retract_unit(retry_port_unit(1), "late retraction");
        // A group with no definition cannot be deferred; this one keeps the
        // retraction pending until the drain.
        irrigation::define_unit(retry_port_unit(2), 7u32, "company");
    }

    /// Satisfies the deferred group's requirement.
    fn provide_group() {
        irrigation::ensure_view(<RetryPort as ViewSchema>::DESCRIPTOR);
        irrigation::define_unit(retry_port_unit(9), 1u32, "driver");
    }

    /// One component decides which group a call stages. The retraction must
    /// belong to the component that installed the port: a retraction from
    /// another component is a takeover, and the install beside it is still live.
    #[component]
    pub fn port(tag: u64) -> u32 {
        if tag == 0 {
            install_group();
        } else if tag == 1 {
            stage_group();
        } else {
            provide_group();
        }
        match tag {
            0 => 42,
            _ => 0,
        }
    }
}

/// Reads the port the nested retraction removes, so the drain's seed is the
/// general root itself rather than a site node beneath it.
struct LateRetractionRoot {
    runtime: Runtime,
}

impl GeneralComponent for LateRetractionRoot {
    type State = u64;
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00C3_0003);
    const NAME: &'static str = "tests::LateRetractionRoot";
    const VIEWS: &'static [&ViewDescriptor] = &[<RetryPort as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        self.runtime
            .call::<late_retraction::__PortSpec>(late_retraction::__PortArgs { tag: 0 })?;
        let read = match cx.read_typed::<u32>(retry_port_unit(1)) {
            Ok(value) => u64::from(value),
            Err(irrigation::ComputeError::Absent { .. }) => 0,
            Err(error) => return Err(error),
        };
        self.runtime
            .call::<late_retraction::__PortSpec>(late_retraction::__PortArgs { tag: 1 })?;
        self.runtime
            .call::<late_retraction::__PortSpec>(late_retraction::__PortArgs { tag: 2 })?;
        Ok((read, read))
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

#[test]
fn a_drain_retraction_republishes_the_value_the_demand_read() {
    let runtime = Runtime::new();
    runtime
        .install_general(LateRetractionRoot {
            runtime: runtime.clone(),
        })
        .expect("root");
    assert_eq!(
        runtime
            .read_outputs_general::<LateRetractionRoot>()
            .expect("read"),
        0,
        "the drain retracted the port after the read: the demand must not carry the pre-drain value out"
    );
    assert_eq!(
        runtime
            .read_outputs_general::<LateRetractionRoot>()
            .expect("read at the same revision"),
        0,
        "one revision serves one result"
    );
}

// ---------------------------------------------------------------------------
// A fill is a change for the readers that observed the unfilled generation
// (runtime-kernel-r11-01)
// ---------------------------------------------------------------------------

#[irrigation::view(source)]
pub struct Network {
    pub links: irrigation::Graph<u64, u64>,
}

/// One node's outgoing adjacency index: an accessor projection the content
/// vtable cannot spell, so a generic read of it answers absent while a
/// replacement owns it.
fn adjacency_unit(node: u64) -> UnitKey {
    UnitKey::entity(
        Network::view_id(),
        FieldId(0),
        UnitKind::Adjacency,
        irrigation::any_key(irrigation::NodeKey(node)),
    )
}

#[irrigation::irrigation]
mod network_access {
    use super::Network;

    /// Materializes node 1's adjacency cell through the accessor that owns it.
    #[component]
    pub fn outgoing(tag: u64) -> Vec<(u64, u64)> {
        let _ = tag;
        Network.links().outgoing(1)
    }
}

#[irrigation::view(derived)]
pub struct AdjacencyOut {
    pub value: u64,
}

/// Reads the raw adjacency unit and catches its absence.
struct AdjacencyProbe;

impl GeneralComponent for AdjacencyProbe {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C3_0004);
    const NAME: &'static str = "tests::AdjacencyProbe";
    const VIEWS: &'static [&ViewDescriptor] = &[<Network as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[<AdjacencyOut as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value = match cx.read_typed::<Vec<(u64, u64)>>(adjacency_unit(1)) {
            Ok(edges) => edges.len() as u64,
            Err(irrigation::ComputeError::Absent { .. }) => 0,
            Err(error) => return Err(error),
        };
        cx.output()
            .field(AdjacencyOut::view_id(), FieldId(0), value);
        Ok(((), ()))
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

#[irrigation::irrigation]
mod adjacency_reader {
    use super::AdjacencyOut;

    #[component]
    pub fn read(tag: u64) -> u64 {
        let _ = tag;
        AdjacencyOut.value()
    }
}

#[test]
fn a_fill_reaches_the_reader_that_observed_the_unfilled_generation() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(AdjacencyProbe)
        .expect("probe");
    let probe = |tag: u64| {
        runtime
            .call::<adjacency_reader::__ReadSpec>(adjacency_reader::__ReadArgs { tag })
            .expect("probe run")
    };
    let accessor = |tag: u64| {
        runtime
            .call::<network_access::__OutgoingSpec>(network_access::__OutgoingArgs { tag })
            .expect("accessor run")
    };

    runtime
        .edit(|edit| {
            let mut view = edit.view::<Network>();
            let mut links = view.links();
            links.add_node(1, 0u64);
            links.add_node(2, 0u64);
            links.add_edge(1, 2, 5u64);
        })
        .expect("seed");
    assert_eq!(
        probe(0),
        1,
        "the accessor's projection of the seeded content has one edge"
    );

    // The replacement stages no cell, so it owns every projection of the
    // content — including the adjacency index only its accessor can derive.
    let network = irrigation_store::GraphStore::<u64, u64>::new()
        .add_node(1, 0u64)
        .add_node(2, 0u64)
        .add_node(3, 0u64);
    let (network, _) = network.add_edge(1, 2, 5u64);
    let (network, _) = network.add_edge(1, 3, 7u64);
    let before = runtime.revision();
    runtime
        .edit(|edit| {
            edit.set_content(
                Network::view_id(),
                Arc::new(NetworkContent { links: network }),
                irrigation::values_equal::<NetworkContent>,
            );
        })
        .expect("a source View has no provider to refuse the replacement");
    let replaced = runtime.revision();
    assert_eq!(
        replaced,
        before + 1,
        "the replacement publishes its revision"
    );
    assert_eq!(
        probe(1),
        0,
        "a stale cell the content vtable cannot spell answers absent"
    );

    assert_eq!(
        accessor(0),
        vec![(0, 2), (1, 3)],
        "the accessor re-derives the adjacency index from the new content"
    );
    assert_eq!(
        probe(2),
        2,
        "the payload the accessor installed must reach the reader that observed the absence"
    );
}

// ---------------------------------------------------------------------------
// A cell an accessor projects has one creator (runtime-kernel-r13-01)
// ---------------------------------------------------------------------------

#[irrigation::irrigation]
mod cache_negative_read {
    use super::Cache;
    use irrigation::{FieldId, UnitKey, UnitKind, ViewSchema};

    fn entry_unit(key: u32) -> UnitKey {
        UnitKey::entity(
            Cache::view_id(),
            FieldId(0),
            UnitKind::Value,
            irrigation::any_key(key),
        )
    }

    /// Answers one entry's payload with a negative read, creating its cell
    /// vacant without a projection.
    #[component]
    pub fn negative(tag: u64) -> u32 {
        let _ = tag;
        let _ = irrigation::try_read_unit_value(entry_unit(1), |_| None::<u32>);
        0
    }

    /// Reads the same entry through the accessor that owns its payload.
    #[component]
    pub fn value(tag: u64) -> Option<u32> {
        let _ = tag;
        Cache.field0().get(&1)
    }
}

/// Seeds `Network`'s committed content alone, so no cell of its adjacency
/// index exists: the content vtable cannot spell an accessor's projection.
fn seed_network(runtime: &Runtime, nodes: &[(u64, u64)], edges: &[(u64, u64, u64)]) -> u64 {
    let mut network = irrigation_store::GraphStore::<u64, u64>::new();
    for (node, payload) in nodes {
        network = network.add_node(*node, *payload);
    }
    let mut last = 0;
    for (from, to, payload) in edges {
        let (next, edge) = network.add_edge(*from, *to, *payload);
        network = next;
        last = edge;
    }
    runtime
        .edit(|edit| {
            edit.set_content(
                Network::view_id(),
                Arc::new(NetworkContent { links: network }),
                irrigation::values_equal::<NetworkContent>,
            );
        })
        .expect("a source View has no provider to refuse the replacement");
    last
}

#[test]
fn a_general_read_does_not_leave_an_accessor_projected_unit_vacant() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(AdjacencyProbe)
        .expect("probe");
    seed_network(&runtime, &[(1, 0), (2, 0)], &[(1, 2, 5)]);
    assert_eq!(
        runtime
            .call::<adjacency_reader::__ReadSpec>(adjacency_reader::__ReadArgs { tag: 0 })
            .expect("probe run"),
        0,
        "the content vtable cannot spell an adjacency index, so a generic read answers absent"
    );
    assert_eq!(
        runtime
            .call::<network_access::__OutgoingSpec>(network_access::__OutgoingArgs { tag: 1 })
            .expect("the accessor fills the vacant cell from the committed content"),
        vec![(0, 2)],
        "the accessor that owns the projection answers where the generic read left it vacant"
    );
}

#[test]
fn a_negative_read_does_not_poison_a_cell_the_accessor_can_project() {
    let runtime = Runtime::new();
    let view = <Cache as irrigation::ViewSchema>::view_id();
    runtime.register_view(<Cache as irrigation::ViewSchema>::DESCRIPTOR);
    let entries: irrigation::PMap<u32, u32> = [(1u32, 9u32)].into_iter().collect();
    runtime
        .edit(|edit| {
            edit.set_content(
                view,
                Arc::new(CacheContent {
                    field0: entries.into(),
                }),
                irrigation::values_equal::<CacheContent>,
            );
        })
        .expect("seed the content");
    runtime
        .call::<cache_negative_read::__NegativeSpec>(cache_negative_read::__NegativeArgs { tag: 0 })
        .expect("a negative read of an unwritten cell");
    assert_eq!(
        runtime
            .call::<cache_negative_read::__ValueSpec>(cache_negative_read::__ValueArgs { tag: 0 })
            .expect("the accessor read"),
        Some(9),
        "a cell the accessor can project is not poisoned by the negative read"
    );
}

#[test]
fn a_subscription_on_an_accessor_projected_unit_does_not_deliver_an_unchanged_output() {
    let runtime = Runtime::new();
    runtime.register_view(<Network as irrigation::ViewSchema>::DESCRIPTOR);
    seed_network(&runtime, &[(1, 0), (2, 0)], &[(1, 2, 5)]);

    // The index has no cell yet, so the seed is the version a first
    // materialization will inherit.
    let delivered = Arc::new(AtomicU32::new(0));
    let notified = Arc::clone(&delivered);
    let _subscription = runtime
        .observe_units::<Network>(vec![adjacency_unit(1)], move |_| {
            notified.fetch_add(1, Ordering::SeqCst);
        })
        .expect("subscribe to the adjacency index");

    assert_eq!(
        runtime
            .call::<network_access::__OutgoingSpec>(network_access::__OutgoingArgs { tag: 0 })
            .expect("materialize the accessor's cell"),
        vec![(0, 2)],
        "the accessor projects the committed content"
    );
    runtime
        .edit(|edit| edit.view::<Source>().set_value(1))
        .expect("an unrelated edit");
    assert_eq!(
        delivered.load(Ordering::SeqCst),
        0,
        "materializing a projection is not a change of the answer the subscription seeds"
    );
}

// ---------------------------------------------------------------------------
// A pooled demand announces itself before it runs (r10-runtime-state-03)
// ---------------------------------------------------------------------------

/// Records the order of a task's own events, so "started" can be checked against
/// "running" instead of asserted.
#[cfg(feature = "observatory")]
#[derive(Default)]
struct OrderRecorder {
    /// Started-order violation, if one was seen.
    violation: std::sync::Mutex<Option<String>>,
    /// Tasks already seen running or waiting.
    began: std::sync::Mutex<std::collections::HashSet<u64>>,
    /// Demands that reached the pool, by task.
    pooled: AtomicU32,
}

#[cfg(feature = "observatory")]
impl irrigation_observe::RuntimeObserver for OrderRecorder {
    fn level(&self) -> irrigation_observe::ObserveLevel {
        // Task events are `Full`-level; a summary observer would never see the
        // running event this test compares against.
        irrigation_observe::ObserveLevel::Full
    }

    fn record(&self, event: irrigation_observe::RuntimeEvent) {
        use irrigation_observe::RuntimeEvent;
        match event {
            RuntimeEvent::DemandStarted { task, .. } => {
                if self.began.lock().expect("began").contains(&task) {
                    *self.violation.lock().expect("violation") = Some(format!(
                        "DemandStarted for task {task} arrived after that task was already running"
                    ));
                }
                self.pooled.fetch_add(1, Ordering::SeqCst);
            }
            RuntimeEvent::TaskRunning { task }
            | RuntimeEvent::TaskWaiting { task, .. }
            | RuntimeEvent::TaskWoken { task, .. } => {
                self.began.lock().expect("began").insert(task);
            }
            _ => {}
        }
    }
}

#[cfg(feature = "observatory")]
#[test]
fn a_pooled_demand_announces_itself_before_it_runs() {
    let recorder = Arc::new(OrderRecorder::default());
    // A pool, or the demand would take the inline path and this test would say
    // nothing about the pooled one.
    let runtime = Runtime::with_observer(
        irrigation::RuntimeConfig {
            workers: 2,
            ..irrigation::RuntimeConfig::default()
        },
        Arc::clone(&recorder) as Arc<dyn irrigation_observe::RuntimeObserver>,
    );
    runtime.install(draft_settler::Root).expect("install");

    // Called from the test thread, which is not a worker and holds no task, so
    // this demand is a demand root and takes the pool.
    runtime
        .read_outputs::<draft_settler::Root>()
        .expect("provider run");

    assert!(
        recorder.pooled.load(Ordering::SeqCst) > 0,
        "no demand was announced at all"
    );
    assert_eq!(
        *recorder.violation.lock().expect("violation"),
        None,
        "a demand must be announced before it runs"
    );
}

// ---------------------------------------------------------------------------
// An observer's level is honoured by the kernel, not only by the consumer
// (runtime-api-09)
// ---------------------------------------------------------------------------

#[cfg(feature = "observatory")]
#[derive(Default)]
struct LevelCounter {
    level: irrigation_observe::ObserveLevel,
    commits: AtomicU32,
    non_commits: AtomicU32,
}

#[cfg(feature = "observatory")]
impl irrigation_observe::RuntimeObserver for LevelCounter {
    fn level(&self) -> irrigation_observe::ObserveLevel {
        self.level
    }

    fn record(&self, event: irrigation_observe::RuntimeEvent) {
        // Count by the event's own declared level, so the assertion covers every
        // node-level event -- most of them are emitted with no call-site guard and
        // are held back by `emit` alone.
        if event.level() == irrigation_observe::ObserveLevel::Nodes {
            self.commits.fetch_add(1, Ordering::SeqCst);
        } else {
            self.non_commits.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[cfg(feature = "observatory")]
fn count_commits_at(level: irrigation_observe::ObserveLevel) -> (u32, u32) {
    let counter = Arc::new(LevelCounter {
        level,
        ..LevelCounter::default()
    });
    let runtime = Runtime::with_observer(
        irrigation::RuntimeConfig::default(),
        Arc::clone(&counter) as Arc<dyn irrigation_observe::RuntimeObserver>,
    );
    runtime.install(draft_settler::Root).expect("install");
    runtime
        .read_outputs::<draft_settler::Root>()
        .expect("demand");
    (
        counter.commits.load(Ordering::SeqCst),
        counter.non_commits.load(Ordering::SeqCst),
    )
}

/// `NodeCommitted` is node-level detail: a summary observer declares it does not
/// want it, and the kernel is the side that has to honour that -- building the
/// event's payload for an observer that cannot see it is the cost this removes.
#[cfg(feature = "observatory")]
#[test]
fn a_summary_observer_receives_no_node_level_events() {
    let (summary_commits, summary_other) =
        count_commits_at(irrigation_observe::ObserveLevel::Summary);
    assert_eq!(
        summary_commits, 0,
        "a summary observer must not be sent node commits"
    );
    assert!(
        summary_other > 0,
        "but it still receives the summary events it asked for"
    );

    let (node_commits, _) = count_commits_at(irrigation_observe::ObserveLevel::Nodes);
    assert!(node_commits > 0, "a node-level observer does receive them");
}

/// The other half of the level's promise: an observer that does not want node
/// detail should not pay to build it. This is the counter that says so, and it is
/// the only observable that distinguishes the call-site guard from the filter in
/// `emit` -- the events look the same either way.
#[cfg(feature = "observatory")]
#[test]
fn a_summary_observer_builds_no_node_payloads() {
    let counter = Arc::new(LevelCounter {
        level: irrigation_observe::ObserveLevel::Summary,
        ..LevelCounter::default()
    });
    let runtime = Runtime::with_observer(
        irrigation::RuntimeConfig::default(),
        Arc::clone(&counter) as Arc<dyn irrigation_observe::RuntimeObserver>,
    );
    runtime.install(draft_settler::Root).expect("install");
    runtime
        .read_outputs::<draft_settler::Root>()
        .expect("demand");
    assert!(
        runtime.stats().observe_payloads_skipped > 0,
        "a summary observer's demand must not build node-commit payloads"
    );
}

// ---------------------------------------------------------------------------
// A unit notification says how many units moved (r8-runtime-state-07)
// ---------------------------------------------------------------------------

#[cfg(feature = "observatory")]
#[derive(Default)]
struct MovedRecorder {
    moved: AtomicU32,
    fired: AtomicU32,
}

#[cfg(feature = "observatory")]
impl irrigation_observe::RuntimeObserver for MovedRecorder {
    fn level(&self) -> irrigation_observe::ObserveLevel {
        irrigation_observe::ObserveLevel::Summary
    }

    fn record(&self, event: irrigation_observe::RuntimeEvent) {
        if let irrigation_observe::RuntimeEvent::SubscriptionNotified { moved, .. } = event {
            self.moved.store(moved, Ordering::SeqCst);
            self.fired.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// A unit subscription is woken by a per-unit move that does not advance the
/// View's content, so the content versions in the event are equal. The `moved`
/// count is what tells a consumer -- and the board's timeline -- that something
/// did happen.
#[cfg(feature = "observatory")]
#[test]
fn a_unit_notification_reports_how_many_units_moved() {
    let recorder = Arc::new(MovedRecorder::default());
    let runtime = Runtime::with_observer(
        irrigation::RuntimeConfig::default(),
        Arc::clone(&recorder) as Arc<dyn irrigation_observe::RuntimeObserver>,
    );
    runtime.register_view(<Source as ViewSchema>::DESCRIPTOR);
    let unit = irrigation::UnitKey::field(
        <Source as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
    );
    let _subscription = runtime
        .observe_units_of(
            <Source as ViewSchema>::DESCRIPTOR,
            vec![unit.clone()],
            |_snapshot| {},
        )
        .expect("subscribe");

    runtime
        .edit(|edit| edit.set(unit.clone(), 1u64))
        .expect("edit");
    // The refresh delivers the notification; any read drives one.
    let _ = runtime.snapshot();

    assert!(
        recorder.fired.load(Ordering::SeqCst) > 0,
        "the subscription fired"
    );
    assert_eq!(
        recorder.moved.load(Ordering::SeqCst),
        1,
        "one watched unit moved, and the event says so even though the content did not"
    );
}
