//! Kernel change notification: a consumer reruns only when an observed unit changed.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use reactive::{
    ChangeBatch, ComponentId, DependencyUpdates, FieldId, GeneralComponent, GeneralCx, Runtime,
    UnitKey, UnitKind, ViewDescriptor, ViewSchema,
};

#[reactive::view(source)]
pub struct Source {
    pub value: u32,
}

#[reactive::view(derived)]
pub struct Constant {
    pub value: u32,
}

#[reactive::view(derived)]
pub struct Mirror {
    pub value: u32,
}

#[reactive::view(derived)]
pub struct ConstantOut {
    pub value: u32,
}

#[reactive::view(derived)]
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

    fn initialize(&self, cx: &mut GeneralCx) -> reactive::Result<(Self::State, Self::Output)> {
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
    ) -> reactive::Result<(Self::State, Self::Output)> {
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

    fn initialize(&self, cx: &mut GeneralCx) -> reactive::Result<(Self::State, Self::Output)> {
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
    ) -> reactive::Result<(Self::State, Self::Output)> {
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

    fn initialize(&self, cx: &mut GeneralCx) -> reactive::Result<(Self::State, Self::Output)> {
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
    ) -> reactive::Result<(Self::State, Self::Output)> {
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

    fn initialize(&self, cx: &mut GeneralCx) -> reactive::Result<(Self::State, Self::Output)> {
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
    ) -> reactive::Result<(Self::State, Self::Output)> {
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
    let producer = Arc::new(AtomicU32::new(0));
    let constant = Arc::new(AtomicU32::new(0));
    let mirror = Arc::new(AtomicU32::new(0));
    let runtime = Runtime::new();
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

    fn initialize(&self, cx: &mut GeneralCx) -> reactive::Result<(Self::State, Self::Output)> {
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
    ) -> reactive::Result<(Self::State, Self::Output)> {
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
    seen: Arc<std::sync::Mutex<Vec<reactive::UnitKey>>>,
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

    fn initialize(&self, cx: &mut GeneralCx) -> reactive::Result<(Self::State, Self::Output)> {
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
    ) -> reactive::Result<(Self::State, Self::Output)> {
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
