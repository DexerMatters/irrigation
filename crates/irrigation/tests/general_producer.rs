use irrigation::{
    ComponentId, DependencyUpdates, FieldId, GeneralComponent, GeneralCx, Runtime, UnitKey,
    UnitKind, ViewDescriptor, ViewSchema, tree_view, view,
};

#[view(source)]
pub struct Input {
    pub value: u64,
}

#[view(derived)]
pub struct Produced {
    pub value: u64,
}

fn produced_unit() -> UnitKey {
    UnitKey::field(
        <Produced as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
    )
}

struct Producer;

impl GeneralComponent for Producer {
    type State = u64;
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C0_0001);
    const NAME: &'static str = "tests::Producer";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Input as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] =
        &[<Produced as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(UnitKey::field(
            <Input as ViewSchema>::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        cx.output()
            .field(<Produced as ViewSchema>::view_id(), FieldId(0), value);
        Ok((value, ()))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

struct Consumer;

impl GeneralComponent for Consumer {
    type State = ();
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00C0_0002);
    const NAME: &'static str = "tests::Consumer";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Produced as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(produced_unit())?;
        Ok(((), value))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn general_component_producer_is_demanded_by_output_view() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(Producer)
        .expect("producer");
    runtime.install_general_root(Consumer).expect("consumer");
    runtime
        .edit(|edit| edit.view::<Input>().set_value(7))
        .expect("edit");
    assert_eq!(runtime.read_outputs_general::<Consumer>().expect("read"), 7);
    runtime
        .edit(|edit| edit.view::<Input>().set_value(11))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs_general::<Consumer>().expect("read"),
        11
    );
    assert!(
        runtime.stats().write_claims > 0,
        "general output definitions claim their units"
    );
}

#[irrigation::irrigation]
mod app {
    use super::Produced;

    #[irrigation::component]
    pub fn consume(produced: &Produced) -> u64 {
        produced.value()
    }
}

#[test]
fn ordinary_root_component_demands_a_general_producer() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(Producer)
        .expect("producer");
    runtime.install(app::Consume).expect("root");
    runtime
        .edit(|edit| edit.view::<Input>().set_value(21))
        .expect("edit");
    assert_eq!(runtime.read_outputs::<app::Consume>().expect("read"), 21);
}

#[test]
fn a_view_has_exactly_one_general_producer() {
    let runtime = Runtime::new();
    runtime.install_general_producer(Producer).expect("first");
    let error = runtime
        .install_general_producer(Producer)
        .expect_err("a second producer for the same View must be refused");
    assert!(
        format!("{error}").contains("component id is already installed"),
        "the refusal names the duplicate, not some other install failure: {error}"
    );
}

struct NoOutput;

impl GeneralComponent for NoOutput {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C0_0003);
    const NAME: &'static str = "tests::NoOutput";

    fn initialize(&self, _cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        Ok(((), ()))
    }

    fn update(
        &self,
        _cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        Ok(((), ()))
    }
}

#[test]
fn non_root_general_component_requires_an_output_view() {
    let runtime = Runtime::new();
    assert!(runtime.install_general_producer(NoOutput).is_err());
}

#[irrigation::view(source)]
pub struct SourceProduced {
    pub value: u64,
}

struct SourceOutputProducer;

impl GeneralComponent for SourceOutputProducer {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C0_0004);
    const NAME: &'static str = "tests::SourceOutputProducer";
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] =
        &[<SourceProduced as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, _cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        Ok(((), ()))
    }

    fn update(
        &self,
        _cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        Ok(((), ()))
    }
}

#[test]
fn source_views_cannot_be_produced_by_general_components() {
    let runtime = Runtime::new();
    assert!(
        runtime
            .install_general_producer(SourceOutputProducer)
            .is_err()
    );
}

struct BadProducer;

impl GeneralComponent for BadProducer {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C0_0005);
    const NAME: &'static str = "tests::BadProducer";
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[<Produced as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        cx.define(
            UnitKey::field(
                <Input as ViewSchema>::view_id(),
                FieldId(0),
                UnitKind::Value,
            ),
            99u64,
            "bad",
        );
        Ok(((), ()))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn definitions_outside_output_views_fail_as_component_errors() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(BadProducer)
        .expect("producer");
    runtime.install_general_root(Consumer).expect("consumer");
    runtime
        .edit(|edit| edit.view::<Input>().set_value(1))
        .expect("edit");
    let error = runtime
        .read_outputs_general::<Consumer>()
        .expect_err("the bad definition is reported");
    let message = format!("{error:?}");
    assert!(
        message.contains("outside output views"),
        "the report names the mistake: {message}"
    );
    assert!(
        message.contains("general_producer.rs"),
        "and the line it happened on, not the runtime's own source: {message}"
    );
}

struct PreservingProducer;

impl GeneralComponent for PreservingProducer {
    type State = u64;
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C0_0006);
    const NAME: &'static str = "tests::PreservingProducer";
    const VIEWS: &'static [&ViewDescriptor] = &[<Input as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&ViewDescriptor] = &[<Produced as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(UnitKey::field(
            <Input as ViewSchema>::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        cx.output()
            .field(<Produced as ViewSchema>::view_id(), FieldId(0), value);
        Ok((value, ()))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(UnitKey::field(
            <Input as ViewSchema>::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        // Only even values restage the output: the odd update exercises the
        // rule that a unit a component does not restage keeps its value.
        if value % 2 == 0 {
            cx.output()
                .field(<Produced as ViewSchema>::view_id(), FieldId(0), value);
        }
        Ok((value, ()))
    }
}

#[test]
fn a_unit_not_restaged_is_kept() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(PreservingProducer)
        .expect("producer");
    runtime.install_general_root(Consumer).expect("consumer");
    runtime
        .edit(|edit| edit.view::<Input>().set_value(5))
        .expect("edit");
    assert_eq!(runtime.read_outputs_general::<Consumer>().expect("read"), 5);
    runtime
        .edit(|edit| edit.view::<Input>().set_value(6))
        .expect("edit");
    assert_eq!(runtime.read_outputs_general::<Consumer>().expect("read"), 6);

    // The update that reads 9 stages nothing: the committed 6 must survive.
    runtime
        .edit(|edit| edit.view::<Input>().set_value(9))
        .expect("edit");
    assert_eq!(
        runtime.read_outputs_general::<Consumer>().expect("read"),
        6,
        "a unit the component did not restage was retracted"
    );
}

// ---------------------------------------------------------------------------
// A failing producer is a failure, never an absence (runtime-general-01)
// ---------------------------------------------------------------------------

#[tree_view(kind = derived, root = Doc, view = Ast)]
pub mod trees {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Doc {
        Leaf,
    }
}

struct FailingTreeProducer;

impl GeneralComponent for FailingTreeProducer {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C0_0010);
    const NAME: &'static str = "tests::FailingTreeProducer";
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] =
        &[<trees::Ast as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        // A definition outside this component's output Views rejects the
        // attempt, so the tree root is never published.
        cx.define(
            UnitKey::field(
                <Input as ViewSchema>::view_id(),
                FieldId(0),
                UnitKind::Value,
            ),
            99u64,
            "bad",
        );
        Ok(((), ()))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[irrigation::irrigation]
mod tree_reader {
    use super::trees;

    #[irrigation::component]
    pub fn root_present() -> bool {
        let tree = trees::Ast;
        tree.root().is_some()
    }
}

#[test]
fn a_failing_tree_producer_is_not_reported_as_an_absent_root() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(FailingTreeProducer)
        .expect("producer");
    runtime.install(tree_reader::RootPresent).expect("root");
    runtime
        .edit(|edit| edit.view::<Input>().set_value(1))
        .expect("seed");
    let result = runtime.read_outputs::<tree_reader::RootPresent>();
    assert!(
        matches!(
            result,
            Err(irrigation::ComputeError::DefinitionDenied { .. })
        ),
        "a failing producer must surface instead of publishing an absent tree: {result:?}"
    );
}

struct AbsentReader;

impl GeneralComponent for AbsentReader {
    type State = u64;
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00C0_0011);
    const NAME: &'static str = "tests::AbsentReader";

    fn initialize(&self, _cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value =
            irrigation::try_read_unit_value::<u64, _>(produced_unit(), |_| None).unwrap_or(0);
        Ok((value, value))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[irrigation::irrigation]
mod produced_reader {
    use super::Produced;

    #[irrigation::component]
    pub fn read_produced(tag: u64) -> u64 {
        let _ = tag;
        Produced.value()
    }
}

#[test]
fn an_absent_unit_is_observed_and_invalidated_by_its_definition() {
    let runtime = Runtime::new();
    runtime.install_general_root(AbsentReader).expect("reader");
    runtime
        .edit(|edit| edit.view::<Input>().set_value(5))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs_general::<AbsentReader>()
            .expect("absent read"),
        0,
        "an undefined unit reads as absent"
    );

    runtime
        .install_general_producer(Producer)
        .expect("producer");
    runtime
        .edit(|edit| edit.view::<Input>().set_value(6))
        .expect("edit");
    // Another demand defines the unit; the reader must have observed its
    // absence, so the definition invalidates it.
    assert_eq!(
        runtime
            .call::<produced_reader::__ReadProducedSpec>(produced_reader::__ReadProducedArgs {
                tag: 0
            })
            .expect("producer demand"),
        6
    );
    assert_eq!(
        runtime
            .read_outputs_general::<AbsentReader>()
            .expect("read after definition"),
        6,
        "the negative read must make the reader observe the later definition"
    );
}

// ---------------------------------------------------------------------------
// Content replacement is a write (runtime-general-03)
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct ContentSlot {
    pub value: u64,
}

struct ForeignContent;

impl GeneralComponent for ForeignContent {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C0_0012);
    const NAME: &'static str = "tests::ForeignContent";
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] =
        &[<Produced as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        // `ContentSlot` is registered but is not one of this component's output
        // Views: content is a write, and it is not this component's to make.
        cx.set_content(
            <ContentSlot as ViewSchema>::view_id(),
            std::sync::Arc::new(ContentSlotContent { value: 77 }),
        );
        Ok(((), ()))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

struct UnregisteredContent;

impl GeneralComponent for UnregisteredContent {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C0_0013);
    const NAME: &'static str = "tests::UnregisteredContent";

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        cx.set_content(
            irrigation::ViewId(0xDEAD_BEEF),
            std::sync::Arc::new(ContentSlotContent { value: 77 }),
        );
        Ok(((), ()))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn content_replacement_stays_inside_the_output_views() {
    let runtime = Runtime::new();
    runtime.register_view(<ContentSlot as ViewSchema>::DESCRIPTOR);
    runtime
        .install_general_root(ForeignContent)
        .expect("install");
    let error = runtime
        .read_outputs_general::<ForeignContent>()
        .expect_err("a foreign View's content is not this component's to write");
    assert!(
        matches!(error, irrigation::ComputeError::DefinitionDenied { .. }),
        "expected a definition denial, got {error:?}"
    );
    assert_eq!(
        runtime.snapshot().view::<ContentSlot>().value(),
        0,
        "a denied replacement must not be applied"
    );
}

#[test]
fn content_replacement_requires_a_registered_view() {
    let runtime = Runtime::new();
    runtime
        .install_general_root(UnregisteredContent)
        .expect("install");
    let error = runtime
        .read_outputs_general::<UnregisteredContent>()
        .expect_err("content for an unregistered View is not a write");
    assert!(
        matches!(error, irrigation::ComputeError::DefinitionDenied { .. }),
        "expected a definition denial, got {error:?}"
    );
}

struct HalfAppliedContent;

impl GeneralComponent for HalfAppliedContent {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C0_0014);
    const NAME: &'static str = "tests::HalfAppliedContent";
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] =
        &[<ContentSlot as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        cx.set_content(
            <ContentSlot as ViewSchema>::view_id(),
            std::sync::Arc::new(ContentSlotContent { value: 77 }),
        );
        // The same unit twice: the install is rejected after the content was
        // staged, so the content must not be applied.
        cx.output()
            .field(<ContentSlot as ViewSchema>::view_id(), FieldId(0), 1u64);
        cx.output()
            .field(<ContentSlot as ViewSchema>::view_id(), FieldId(0), 2u64);
        Ok(((), ()))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn a_rejected_install_does_not_publish_view_content() {
    let runtime = Runtime::new();
    runtime
        .install_general_root(HalfAppliedContent)
        .expect("install");
    let error = runtime
        .read_outputs_general::<HalfAppliedContent>()
        .expect_err("the duplicate draft rejects the install");
    assert!(
        matches!(error, irrigation::ComputeError::DuplicateDefinition { .. }),
        "expected a duplicate definition, got {error:?}"
    );
    assert_eq!(
        runtime.snapshot().view::<ContentSlot>().value(),
        0,
        "a rejected install must not replace View content"
    );
}

// ---------------------------------------------------------------------------
// A local component's retained state follows the commit (runtime-general-02)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct LocalMode {
    mode: u64,
}

/// Lengths of the retained state each `update` was handed.
static LOCAL_UPDATE_LENS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());
/// Whether the next `update` should block until the demand is cancelled.
static LOCAL_CANCEL_ARMED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static LOCAL_UPDATE_ENTERED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static LOCAL_CANCEL_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn local_mode_unit() -> UnitKey {
    UnitKey::field(
        <LocalMode as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
    )
}

struct LocalAppend;

impl irrigation::LocalGeneralComponent for LocalAppend {
    type State = Vec<u64>;
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00C0_0015);
    const NAME: &'static str = "tests::LocalAppend";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<LocalMode as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let mode: u64 = cx.read_typed(local_mode_unit())?;
        Ok((vec![mode], mode))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        use std::sync::atomic::Ordering;
        LOCAL_UPDATE_LENS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(state.len());
        let mode: u64 = cx.read_typed(local_mode_unit())?;
        if LOCAL_CANCEL_ARMED.swap(false, Ordering::SeqCst) {
            LOCAL_UPDATE_ENTERED.store(true, Ordering::Release);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !LOCAL_CANCEL_DONE.load(Ordering::Acquire) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the demand was never cancelled"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        let mut next = state.clone();
        next.push(mode);
        Ok((next, mode))
    }
}

#[test]
fn a_cancelled_local_update_keeps_the_retained_state() {
    use std::sync::atomic::Ordering;

    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 0,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.install_local_general(LocalAppend).expect("install");
    runtime
        .edit(|edit| edit.view::<LocalMode>().set_mode(5))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::LocalAdapter<LocalAppend>>()
            .expect("initialize"),
        5
    );

    runtime
        .edit(|edit| edit.view::<LocalMode>().set_mode(9))
        .expect("edit");
    LOCAL_UPDATE_LENS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    LOCAL_UPDATE_ENTERED.store(false, Ordering::Release);
    LOCAL_CANCEL_DONE.store(false, Ordering::Release);
    LOCAL_CANCEL_ARMED.store(true, Ordering::Release);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !LOCAL_UPDATE_ENTERED.load(Ordering::Acquire) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the update never started"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            runtime.cancel();
            LOCAL_CANCEL_DONE.store(true, Ordering::Release);
        });
        let cancelled = runtime.read_outputs_general::<irrigation::LocalAdapter<LocalAppend>>();
        assert!(
            cancelled.is_err(),
            "a cancelled attempt cannot publish: {cancelled:?}"
        );
    });

    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::LocalAdapter<LocalAppend>>()
            .expect("retry"),
        9
    );
    assert_eq!(
        *LOCAL_UPDATE_LENS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![1, 1],
        "a cancelled attempt must not advance the retained state"
    );
}

// ---------------------------------------------------------------------------
// Route selection is host code (runtime-kernel-03)
// ---------------------------------------------------------------------------

static SELECTOR_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct ReentrantSelector {
    runtime: std::sync::Weak<Runtime>,
}

impl irrigation::RouteSelector for ReentrantSelector {
    fn select(
        &self,
        unit: &UnitKey,
        first: irrigation::NodeId,
        _second: irrigation::NodeId,
    ) -> irrigation::NodeId {
        SELECTOR_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(runtime) = self.runtime.upgrade() {
            // Every one of these takes the metadata lock. If selection ran
            // under it, this selector would deadlock its own worker.
            let _ = runtime.provider_of(unit);
            let _ = runtime.stats();
        }
        first
    }
}

#[irrigation::irrigation]
mod overrides {
    use super::Produced;

    #[irrigation::component]
    pub fn override_field0(tag: u64) -> u64 {
        let _ = tag;
        let slot = Produced;
        slot.define_value(21);
        21
    }
}

#[test]
fn a_route_selector_may_call_back_into_the_runtime() {
    SELECTOR_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
    let runtime = std::sync::Arc::new(Runtime::new());
    runtime
        .install_general_producer(Producer)
        .expect("producer");
    runtime.install_general_root(Consumer).expect("consumer");
    runtime
        .edit(|edit| edit.view::<Input>().set_value(4))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs_general::<Consumer>()
            .expect("first read"),
        4
    );
    runtime.set_route_selector(std::sync::Arc::new(ReentrantSelector {
        runtime: std::sync::Arc::downgrade(&runtime),
    }));
    runtime
        .edit(|edit| edit.view::<Input>().set_value(7))
        .expect("edit");

    // The override contests the unit the general producer still owns, so the
    // call reaches the selector. A deadlock must fail the gate, not hang it.
    let (sender, receiver) = std::sync::mpsc::channel();
    {
        let runtime = std::sync::Arc::clone(&runtime);
        std::thread::spawn(move || {
            let value =
                runtime.call::<overrides::__OverrideField0Spec>(overrides::__OverrideField0Args {
                    tag: 0,
                });
            let _ = sender.send(value);
        });
    }
    let result = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a route selector that re-enters the runtime deadlocked the demand");
    assert_eq!(
        result.expect("the override call"),
        21,
        "the call publishes its output"
    );
    assert!(
        SELECTOR_CALLS.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "the gate must actually reach the route selector"
    );
}

// ---------------------------------------------------------------------------
// Deferral is part of the demand protocol (runtime-kernel-05)
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct DeferredSlot(u64);

/// A port nothing produces in this revision, so a write requiring it defers.
fn deferred_port() -> UnitKey {
    UnitKey::field(
        <DeferredSlot as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
    )
}

struct DeferredGeneral;

impl GeneralComponent for DeferredGeneral {
    type State = ();
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00C0_0016);
    const NAME: &'static str = "tests::DeferredGeneral";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<DeferredSlot as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] =
        &[<Produced as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        cx.output()
            .field(<Produced as ViewSchema>::view_id(), FieldId(0), 1u64);
        irrigation::require_condition(false, &[deferred_port()], "deferred port present");
        Ok(((), 0))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn a_deferred_definition_from_a_general_root_is_resolved() {
    let runtime = Runtime::new();
    runtime
        .install_general_root(DeferredGeneral)
        .expect("install");
    let error = runtime
        .read_outputs_general::<DeferredGeneral>()
        .expect_err("a deferred definition that can never hold must fail the demand");
    assert!(
        matches!(error, irrigation::ComputeError::PreconditionUnmet { .. }),
        "expected the deferred group to be resolved, got {error:?}"
    );
}

/// Stages one write whose structural requirement can never hold.
///
/// The requirement and the definition are staged from one call, so they land on
/// one write group: a lowered statement would give each its own site frame.
fn stage_deferred_write() {
    irrigation::ensure_view(<Produced as ViewSchema>::DESCRIPTOR);
    irrigation::require_condition(false, &[deferred_port()], "deferred port present");
    irrigation::define_unit(
        UnitKey::field(
            <Produced as ViewSchema>::view_id(),
            FieldId(0),
            UnitKind::Value,
        ),
        1u64,
        "deferred",
    );
}

#[irrigation::irrigation]
mod deferred_call {
    #[irrigation::component]
    pub fn define_and_defer(tag: u64) -> u64 {
        let _ = tag;
        super::stage_deferred_write();
        0
    }
}

#[test]
fn a_deferred_definition_from_a_host_call_is_resolved() {
    let runtime = Runtime::new();
    let error = runtime
        .call::<deferred_call::__DefineAndDeferSpec>(deferred_call::__DefineAndDeferArgs { tag: 0 })
        .expect_err("a deferred definition that can never hold must fail the call");
    assert!(
        matches!(error, irrigation::ComputeError::PreconditionUnmet { .. }),
        "expected the deferred group to be resolved, got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Host code that panics must not take a View's data with it (runtime-kernel-03)
// ---------------------------------------------------------------------------

/// A field value whose equality is host code and can panic.
#[derive(Clone, Default, Debug)]
pub struct Flaky {
    value: u64,
}

static FLAKY_PANICS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl PartialEq for Flaky {
    fn eq(&self, other: &Self) -> bool {
        assert!(
            !FLAKY_PANICS.load(std::sync::atomic::Ordering::SeqCst),
            "flaky field equality"
        );
        self.value == other.value
    }
}

#[view(derived)]
pub struct FlakyView {
    pub value: Flaky,
}

struct FlakyProducer;

impl GeneralComponent for FlakyProducer {
    type State = u64;
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C0_0017);
    const NAME: &'static str = "tests::FlakyProducer";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Input as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] =
        &[<FlakyView as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(UnitKey::field(
            <Input as ViewSchema>::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        cx.output().field(
            <FlakyView as ViewSchema>::view_id(),
            FieldId(0),
            Flaky { value },
        );
        Ok((value, ()))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

struct FlakyConsumer;

impl GeneralComponent for FlakyConsumer {
    type State = u64;
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00C0_0018);
    const NAME: &'static str = "tests::FlakyConsumer";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<FlakyView as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: Flaky = cx.read_typed(UnitKey::field(
            <FlakyView as ViewSchema>::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        Ok((value.value, value.value))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn a_panicking_host_equality_leaves_the_view_intact() {
    let runtime = Runtime::new();
    runtime
        .install_general_producer(FlakyProducer)
        .expect("producer");
    runtime
        .install_general_root(FlakyConsumer)
        .expect("consumer");
    runtime
        .edit(|edit| edit.view::<Input>().set_value(1))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs_general::<FlakyConsumer>()
            .expect("first read"),
        1
    );

    // The second publication compares the new field against the committed one
    // while the View's region is out of the state map.
    FLAKY_PANICS.store(true, std::sync::atomic::Ordering::SeqCst);
    runtime
        .edit(|edit| edit.view::<Input>().set_value(2))
        .expect("edit");
    let panicked = runtime.read_outputs_general::<FlakyConsumer>();
    FLAKY_PANICS.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(
        panicked.is_err(),
        "a panicking host equality cannot publish: {panicked:?}"
    );

    assert_eq!(
        runtime
            .read_outputs_general::<FlakyConsumer>()
            .expect("the runtime is still usable"),
        2
    );
    assert_eq!(
        runtime.snapshot().view::<FlakyView>().value().value,
        2,
        "the View kept its region"
    );
}

// ---------------------------------------------------------------------------
// A checked insert re-executed by its provider replaces its own publication
// (runtime-general-r5-01)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct CheckSource {
    pub entries: Map<u32, u64>,
}

#[view(derived)]
pub struct CheckedTable(Map<u32, u64>);

#[irrigation::irrigation]
mod checked_republish {
    use super::{CheckSource, CheckedTable};

    #[component]
    pub fn root(source: &CheckSource) -> u64 {
        let entries = source.entries().to_vec();
        entries.iter().for_each(|(key, value)| {
            CheckedTable.field0().insert(*key, *value, "map_insert");
        });
        entries.len() as u64
    }
}

fn checked_table(runtime: &Runtime) -> Vec<(u32, u64)> {
    runtime
        .read()
        .expect("snapshot")
        .view::<CheckedTable>()
        .field0()
}

fn checked_republish_runtime() -> Runtime {
    let runtime = Runtime::new();
    runtime.register_view(<CheckedTable as ViewSchema>::DESCRIPTOR);
    runtime.install(checked_republish::Root).expect("install");
    runtime
}

#[test]
fn a_checked_insert_replaces_the_key_its_own_previous_revision_installed() {
    let runtime = checked_republish_runtime();
    runtime
        .edit(|edit| edit.view::<CheckSource>().entries().insert(1u32, 10u64))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<checked_republish::Root>()
            .expect("first revision"),
        1
    );
    assert_eq!(checked_table(&runtime), vec![(1u32, 10u64)]);

    // The body runs again, re-issuing the insert for the key its own previous
    // revision published, and adds a second key. From scratch at this revision
    // both keys are absent, so the rebuild must succeed and publish both.
    runtime
        .edit(|edit| edit.view::<CheckSource>().entries().insert(2u32, 20u64))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs::<checked_republish::Root>()
            .expect("the rebuild replaces the units it published"),
        2
    );
    assert_eq!(checked_table(&runtime), vec![(1u32, 10u64), (2u32, 20u64)]);

    // A from-scratch evaluation at revision 2 builds the same table.
    let fresh = checked_republish_runtime();
    fresh
        .edit(|edit| {
            let mut entries = edit.view::<CheckSource>();
            entries.entries().insert(1u32, 10u64);
            entries.entries().insert(2u32, 20u64);
        })
        .expect("seed");
    assert_eq!(
        fresh
            .read_outputs::<checked_republish::Root>()
            .expect("from scratch"),
        2
    );
    assert_eq!(checked_table(&fresh), vec![(1u32, 10u64), (2u32, 20u64)]);
}

// ---------------------------------------------------------------------------
// A content-only publication is a change its observers see (runtime-general-r5-02)
// ---------------------------------------------------------------------------

struct ContentOnly;

impl GeneralComponent for ContentOnly {
    type State = u64;
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C0_0019);
    const NAME: &'static str = "tests::ContentOnly";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Input as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] =
        &[<ContentSlot as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(UnitKey::field(
            <Input as ViewSchema>::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        cx.set_content(
            <ContentSlot as ViewSchema>::view_id(),
            std::sync::Arc::new(ContentSlotContent { value }),
        );
        Ok((value, ()))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn a_content_only_publication_notifies_and_versions_the_view() {
    let runtime = Runtime::new();
    runtime.install_general_root(ContentOnly).expect("install");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = std::sync::Arc::clone(&seen);
    let _subscription = runtime
        .observe::<ContentSlot>(move |snapshot| {
            recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(snapshot.version::<ContentSlot>());
        })
        .expect("observe");

    runtime
        .edit(|edit| edit.view::<Input>().set_value(3))
        .expect("seed");
    let first = runtime.revision();
    assert_eq!(runtime.snapshot().view::<ContentSlot>().value(), 3);
    assert_eq!(
        runtime.snapshot().version::<ContentSlot>(),
        Some(first),
        "a content-only publication owns its content version"
    );
    assert_eq!(
        *seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![Some(first)],
        "the whole-View observer fires on the replacement"
    );

    runtime
        .edit(|edit| edit.view::<Input>().set_value(4))
        .expect("edit");
    let second = runtime.revision();
    assert_eq!(runtime.snapshot().view::<ContentSlot>().value(), 4);
    assert_eq!(runtime.snapshot().version::<ContentSlot>(), Some(second));
    assert_eq!(
        *seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![Some(first), Some(second)],
        "every replacement moves the version exactly once"
    );
}

// ---------------------------------------------------------------------------
// A post-publication retry keeps the staged local state (runtime-general-r5-03)
// ---------------------------------------------------------------------------

#[view(source)]
pub struct SlotMode {
    pub mode: u64,
}

#[view(derived)]
pub struct RetrySlot(Map<u32, u64>);

fn input_unit() -> UnitKey {
    UnitKey::field(
        <Input as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
    )
}

fn retry_slot_unit() -> UnitKey {
    UnitKey::entity(
        <RetrySlot as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
        irrigation::any_key(1u32),
    )
}

/// Defines one slot at the first revision and retracts it at the second.
struct SlotProd;

impl GeneralComponent for SlotProd {
    type State = u64;
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C0_0020);
    const NAME: &'static str = "tests::SlotProd";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<SlotMode as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] =
        &[<RetrySlot as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let mode: u64 = cx.read_typed(UnitKey::field(
            <SlotMode as ViewSchema>::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        if mode < 2 {
            cx.output().entry(
                <RetrySlot as ViewSchema>::view_id(),
                FieldId(0),
                1u32,
                10u64,
            );
        }
        Ok((mode, ()))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        let mode: u64 = cx.read_typed(UnitKey::field(
            <SlotMode as ViewSchema>::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        cx.output().retract(retry_slot_unit());
        Ok((mode, ()))
    }
}

#[irrigation::irrigation]
mod slot_probe {
    use super::RetrySlot;

    #[component]
    pub fn read_slot(tag: u64) -> Option<u64> {
        let _ = tag;
        RetrySlot.field0().get(&1)
    }
}

/// Lengths of the retained state each `update` was handed.
static LOCAL_RETRY_LENS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

struct LocalRetry;

impl irrigation::LocalGeneralComponent for LocalRetry {
    type State = Vec<u64>;
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00C0_0021);
    const NAME: &'static str = "tests::LocalRetry";
    const VIEWS: &'static [&'static ViewDescriptor] = &[
        <Input as ViewSchema>::DESCRIPTOR,
        <RetrySlot as ViewSchema>::DESCRIPTOR,
    ];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let mode: u64 = cx.read_typed(input_unit())?;
        let slot = match cx.read_typed::<u64>(retry_slot_unit()) {
            Ok(value) => value,
            Err(irrigation::ComputeError::Absent { .. }) => 0,
            Err(error) => return Err(error),
        };
        let state = vec![mode, slot];
        let output = state.len() as u64;
        Ok((state, output))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        LOCAL_RETRY_LENS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(state.len());
        let mode: u64 = cx.read_typed(input_unit())?;
        let slot = match cx.read_typed::<u64>(retry_slot_unit()) {
            Ok(value) => value,
            Err(irrigation::ComputeError::Absent { .. }) => 0,
            Err(error) => return Err(error),
        };
        let mut next = state.clone();
        next.push(mode + slot);
        let output = next.len() as u64;
        Ok((next, output))
    }
}

fn local_retry_lens() -> Vec<usize> {
    LOCAL_RETRY_LENS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[test]
fn a_local_component_keeps_the_state_its_own_publication_staged() {
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 0,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.install_general_producer(SlotProd).expect("slot");
    runtime.install_local_general(LocalRetry).expect("local");
    runtime
        .edit(|edit| {
            edit.view::<Input>().set_value(1);
            edit.view::<SlotMode>().set_mode(1);
        })
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::LocalAdapter<LocalRetry>>()
            .expect("initialize"),
        2
    );

    // The slot's producer runs first and retracts the unit, so the local
    // component observes an absent unit whose reader record is already dirty.
    // Its attempt observes nothing that commits: the commit publishes the
    // value and trace, then has to acknowledge that observation.
    runtime
        .edit(|edit| {
            edit.view::<Input>().set_value(2);
            edit.view::<SlotMode>().set_mode(2);
        })
        .expect("edit");
    let retired = runtime
        .call::<slot_probe::__ReadSlotSpec>(slot_probe::__ReadSlotArgs { tag: 0 })
        .expect("probe");
    assert_eq!(retired, None, "the producer retracted the slot");
    let retries = runtime.stats().demand_retries;
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::LocalAdapter<LocalRetry>>()
            .expect("the attempt published before it was asked to retry"),
        3,
        "the update staged a third element and published it"
    );
    assert!(
        runtime.stats().demand_retries > retries,
        "the published attempt asks for a retry"
    );
    assert_eq!(
        local_retry_lens(),
        vec![2],
        "one update attempt published; the retry reuses it"
    );

    // The next revision updates from the state the published attempt staged; a
    // discarded state would claim the pre-publication state is still current.
    runtime
        .edit(|edit| edit.view::<Input>().set_value(3))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::LocalAdapter<LocalRetry>>()
            .expect("the state survives the published retry"),
        4
    );
    assert_eq!(
        local_retry_lens(),
        vec![2, 3],
        "the retried publication's staged state is the next update's basis"
    );
}

// ---------------------------------------------------------------------------
// The local slot registry never runs user code under its borrow
// (runtime-general-r5-07)
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct FeedOut {
    pub value: u64,
}

#[view(derived)]
pub struct SinkOut {
    pub value: u64,
}

fn feed_out_unit() -> UnitKey {
    UnitKey::field(
        <FeedOut as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
    )
}

struct LocalFeed;

impl irrigation::LocalGeneralComponent for LocalFeed {
    type State = u64;
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00C0_001A);
    const NAME: &'static str = "tests::LocalFeed";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Input as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] = &[<FeedOut as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(UnitKey::field(
            <Input as ViewSchema>::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        cx.output()
            .field(<FeedOut as ViewSchema>::view_id(), FieldId(0), value + 10);
        Ok((value, value + 10))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

struct LocalSink;

impl irrigation::LocalGeneralComponent for LocalSink {
    type State = u64;
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00C0_001B);
    const NAME: &'static str = "tests::LocalSink";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<FeedOut as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] = &[<SinkOut as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        // Reading the other local producer demands it from inside this body.
        let feed: u64 = cx.read_typed(feed_out_unit())?;
        cx.output()
            .field(<SinkOut as ViewSchema>::view_id(), FieldId(0), feed + 1);
        Ok((feed, feed + 1))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[irrigation::irrigation]
mod local_chain {
    use super::SinkOut;

    #[component]
    pub fn root(sink: &SinkOut) -> u64 {
        sink.value()
    }
}

#[test]
fn a_local_component_may_demand_a_nested_local_component() {
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 0,
        ..irrigation::RuntimeConfig::default()
    });
    runtime
        .install_general_producer(irrigation::LocalAdapter::new(LocalFeed))
        .expect("feed");
    runtime
        .install_general_producer(irrigation::LocalAdapter::new(LocalSink))
        .expect("sink");
    runtime.install(local_chain::Root).expect("root");
    runtime
        .edit(|edit| edit.view::<Input>().set_value(4))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<local_chain::Root>()
            .expect("an enclosing local activation must not keep the registry borrowed"),
        15,
        "the sink reads the feed's published output"
    );
}

/// A component registered from user code while another slot is being dropped.
struct ProbeComponent;

impl irrigation::LocalGeneralComponent for ProbeComponent {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C0_001C);
    const NAME: &'static str = "tests::ProbeComponent";

    fn initialize(&self, _cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        Ok(((), ()))
    }

    fn update(
        &self,
        _cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        Ok(((), ()))
    }
}

/// Number of retained-state destructors that ran.
static REGISTRY_DROPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Retained state whose destructor touches the local slot registry.
#[derive(Clone)]
struct RegistryEntrant;

impl Drop for RegistryEntrant {
    fn drop(&mut self) {
        REGISTRY_DROPS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        drop(irrigation::LocalAdapter::new(ProbeComponent));
    }
}

struct DropOnDrop;

impl irrigation::LocalGeneralComponent for DropOnDrop {
    type State = RegistryEntrant;
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00C0_001D);
    const NAME: &'static str = "tests::DropOnDrop";

    fn initialize(&self, _cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        Ok((RegistryEntrant, 0))
    }

    fn update(
        &self,
        _cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        Ok((RegistryEntrant, 1))
    }
}

/// Whether the next local body panics before returning.
static LOCAL_PANIC_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct PanickingLocal;

impl irrigation::LocalGeneralComponent for PanickingLocal {
    type State = u64;
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00C0_0024);
    const NAME: &'static str = "tests::PanickingLocal";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Input as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(UnitKey::field(
            <Input as ViewSchema>::view_id(),
            FieldId(0),
            UnitKind::Value,
        ))?;
        assert!(
            !LOCAL_PANIC_ARMED.load(std::sync::atomic::Ordering::Acquire),
            "local body panic"
        );
        Ok((value, value))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &irrigation::ChangeBatch,
    ) -> irrigation::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn a_panicking_local_body_restores_its_slot() {
    use std::sync::atomic::Ordering;

    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 0,
        ..irrigation::RuntimeConfig::default()
    });
    runtime
        .install_local_general(PanickingLocal)
        .expect("install");
    runtime
        .edit(|edit| edit.view::<Input>().set_value(1))
        .expect("seed");
    LOCAL_PANIC_ARMED.store(true, Ordering::Release);
    let panicked = runtime.read_outputs_general::<irrigation::LocalAdapter<PanickingLocal>>();
    LOCAL_PANIC_ARMED.store(false, Ordering::Release);
    assert!(panicked.is_err(), "a panicking body cannot publish");

    runtime
        .edit(|edit| edit.view::<Input>().set_value(2))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::LocalAdapter<PanickingLocal>>()
            .expect("the slot survived the unwind"),
        2
    );
}

#[test]
fn a_local_state_destructor_may_register_a_component() {
    let runtime = Runtime::with_config(irrigation::RuntimeConfig {
        workers: 0,
        ..irrigation::RuntimeConfig::default()
    });
    runtime.install_local_general(DropOnDrop).expect("install");
    assert_eq!(
        runtime
            .read_outputs_general::<irrigation::LocalAdapter<DropOnDrop>>()
            .expect("initialize"),
        0
    );
    // The slot owns the retained state: dropping the runtime drops it, and its
    // destructor must run with the registry released.
    REGISTRY_DROPS.store(0, std::sync::atomic::Ordering::Release);
    drop(runtime);
    assert_eq!(
        REGISTRY_DROPS.load(std::sync::atomic::Ordering::Acquire),
        1,
        "the retained state's destructor ran"
    );
}

// ---------------------------------------------------------------------------
// A replaced host object is dropped outside the registry lock (runtime-api-r11-05)
// ---------------------------------------------------------------------------

/// Re-installs itself for another View when dropped, which is the natural
/// teardown for a provider being swapped out -- and the thing that deadlocks a
/// non-reentrant registry lock if the runtime drops it while holding that lock.
struct SelfReplacing {
    runtime: std::sync::Weak<Runtime>,
}

impl std::fmt::Debug for SelfReplacing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SelfReplacing")
    }
}

impl irrigation::RouteSelector for SelfReplacing {
    fn select(
        &self,
        _unit: &irrigation::UnitKey,
        first: irrigation::NodeId,
        _second: irrigation::NodeId,
    ) -> irrigation::NodeId {
        first
    }
}

impl Drop for SelfReplacing {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.upgrade() {
            // The same registry the replacement holds, re-entered from a
            // destructor. With the fix this runs after the guard is released.
            runtime.set_route_selector(std::sync::Arc::new(SelfReplacing {
                runtime: std::sync::Weak::new(),
            }));
        }
    }
}

#[test]
fn a_replaced_selector_is_dropped_outside_the_registry_lock() {
    let runtime = std::sync::Arc::new(Runtime::new());
    runtime.set_route_selector(std::sync::Arc::new(SelfReplacing {
        runtime: std::sync::Arc::downgrade(&runtime),
    }));

    // The swap drops the first selector, whose destructor re-enters the setter.
    // Parked on a thread so a regression reports a deadlock instead of hanging
    // the whole suite.
    let (done, finished) = std::sync::mpsc::channel::<()>();
    let swapping = std::sync::Arc::clone(&runtime);
    let handle = std::thread::spawn(move || {
        swapping.set_route_selector(std::sync::Arc::new(SelfReplacing {
            runtime: std::sync::Weak::new(),
        }));
        let _ = done.send(());
    });
    let outcome = finished.recv_timeout(std::time::Duration::from_secs(5));
    assert!(
        outcome.is_ok(),
        "a destroyed selector re-entering the registry deadlocked, so the runtime dropped it under the lock"
    );
    handle.join().expect("the swapping thread finished");
}
