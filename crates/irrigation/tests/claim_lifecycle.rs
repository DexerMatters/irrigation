//! Claim ownership and the unit-read protocol, end to end.
//!
//! Regressions from the audit round:
//!
//! - a failure during ordered validation must release the claim it interrupted,
//!   for a pooled and for an inline demand;
//! - an async `request` receipt must demand a derived unit's producer, follow
//!   the producer's invalidations, and record it as a dependency;
//! - a receipt must materialize an untouched unit from the committed View
//!   content before the node is visible to any other reader;
//! - a parallel member that fails on its scoped thread must release the claim
//!   it took, so a later demand does not wait for an owner that cannot run;
//! - every installed component owns its own scope, whatever the install order;
//! - every adapter forwards the wrapped component's `OUTPUT_VIEWS`, so an
//!   adapted producer is authorized, registered, and restricted like a direct
//!   one;
//! - a retraction is authorized like a definition, and it never removes a unit
//!   another live provider owns;
//! - a general component's write group is atomic: a group that cannot install
//!   fails the attempt instead of publishing the state around it;
//! - a nested inline demand restores the enclosing task as the cancellation
//!   target, so a cancel after it still stops the enclosing attempt's publish;
//! - a deferred write group stays with the demand that drains it: a stale
//!   group's siblings are re-adopted, a teardown reopens its owner, a
//!   slot-less evaluation never stages one, and a nested demand is never a
//!   demand root while a live task encloses it, whatever the worker count.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use irrigation::{
    AsyncAdapter, AsyncGeneralComponent, ChangeBatch, ComponentFuture, ComponentId,
    DependencyUpdates, FieldId, GeneralComponent, GeneralCx, GeneralInputs, LocalAdapter,
    LocalGeneralComponent, Runtime, RuntimeConfig, TypedAdapter, TypedGeneralComponent, UnitKey,
    UnitKind, ViewDescriptor, ViewSchema, view,
};

#[view(source)]
pub struct Source {
    pub value: u64,
}

#[view(derived)]
pub struct Derived {
    pub value: u64,
}

fn source_unit() -> UnitKey {
    UnitKey::field(
        <Source as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
    )
}

fn derived_unit() -> UnitKey {
    UnitKey::field(
        <Derived as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
    )
}

struct DerivedProducer;

impl GeneralComponent for DerivedProducer {
    type State = u64;
    type Output = ();

    const ID: ComponentId = ComponentId(0x00E0_0001);
    const NAME: &'static str = "claim_lifecycle::DerivedProducer";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Source as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] = &[<Derived as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(source_unit())?;
        cx.output()
            .field(<Derived as ViewSchema>::view_id(), FieldId(0), value);
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

/// A producer that rejects one revision, so a consumer's ordered validation
/// fails while the consumer's own claim is held.
struct FragileProducer;

impl GeneralComponent for FragileProducer {
    type State = u64;
    type Output = ();

    const ID: ComponentId = ComponentId(0x00E0_0002);
    const NAME: &'static str = "claim_lifecycle::FragileProducer";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Source as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] = &[<Derived as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(source_unit())?;
        if value == 11 {
            return Err(irrigation::ComputeError::internal(
                "the producer rejected this revision",
            ));
        }
        cx.output()
            .field(<Derived as ViewSchema>::view_id(), FieldId(0), value);
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

struct DerivedConsumer;

impl GeneralComponent for DerivedConsumer {
    type State = ();
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00E0_0003);
    const NAME: &'static str = "claim_lifecycle::DerivedConsumer";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Derived as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        Ok(((), cx.read_typed(derived_unit())?))
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

/// How long a demand may take before the test reports a parked claim.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Demands the consumer on its own thread, so a claim no task owns surfaces as
/// a timeout instead of hanging the gate run.
fn read_consumer(runtime: &Runtime) -> irrigation::Result<u64> {
    let (sender, receiver) = mpsc::channel();
    let runtime = runtime.clone();
    std::thread::spawn(move || {
        let _ = sender.send(runtime.read_outputs_general::<DerivedConsumer>());
    });
    match receiver.recv_timeout(READ_TIMEOUT) {
        Ok(result) => result,
        Err(_) => panic!("the demand parked on a claim no task owns"),
    }
}

/// Runs the failing-validation scenario on a runtime with `workers` workers.
fn producer_failure_releases_the_claim(workers: usize) {
    let runtime = Runtime::with_config(RuntimeConfig {
        workers,
        stack_size: 16 * 1024 * 1024,
        ..RuntimeConfig::default()
    });
    runtime
        .install_general_component(FragileProducer)
        .expect("producer");
    runtime.install_general(DerivedConsumer).expect("consumer");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(7))
        .expect("seed");
    assert_eq!(read_consumer(&runtime).expect("first read"), 7);

    // The producer fails while the consumer's ordered validation demands it,
    // after the consumer has already been claimed.
    runtime
        .edit(|edit| edit.view::<Source>().set_value(11))
        .expect("edit");
    let error = read_consumer(&runtime).expect_err("the producer's failure must surface");
    assert!(
        format!("{error}").contains("rejected"),
        "unexpected error: {error}"
    );

    // The claim must be gone: this revision demands the consumer again instead
    // of parking on the dead task that failed validation.
    runtime
        .edit(|edit| edit.view::<Source>().set_value(13))
        .expect("edit");
    assert_eq!(read_consumer(&runtime).expect("recovered read"), 13);
}

#[test]
fn a_producer_failure_during_ordered_validation_releases_the_claim() {
    producer_failure_releases_the_claim(2);
}

#[test]
fn a_producer_failure_releases_an_inline_claim_too() {
    producer_failure_releases_the_claim(0);
}

struct AsyncDerivedConsumer;

impl AsyncGeneralComponent for AsyncDerivedConsumer {
    type State = u64;
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00E0_0004);
    const NAME: &'static str = "claim_lifecycle::AsyncDerivedConsumer";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Derived as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize<'a>(
        &'a self,
        cx: &'a mut GeneralCx,
    ) -> ComponentFuture<'a, (Self::State, Self::Output)> {
        Box::pin(async move {
            let value: u64 = cx.request_typed(derived_unit()).await?;
            Ok((value, value))
        })
    }

    fn update<'a>(
        &'a self,
        cx: &'a mut GeneralCx,
        _state: &'a Self::State,
        _changes: &'a ChangeBatch,
    ) -> ComponentFuture<'a, (Self::State, Self::Output)> {
        Box::pin(async move {
            let value: u64 = cx.request_typed(derived_unit()).await?;
            Ok((value, value))
        })
    }
}

#[test]
fn an_async_receipt_demands_and_follows_the_derived_producer() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(DerivedProducer)
        .expect("producer");
    runtime
        .install_general(AsyncAdapter::new(AsyncDerivedConsumer))
        .expect("consumer");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(7))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs_general::<AsyncAdapter<AsyncDerivedConsumer>>()
            .expect("initialize"),
        7
    );
    // The producer dependency the receipt records must carry the edit into the
    // consumer, and the re-read must see the producer's new output.
    runtime
        .edit(|edit| edit.view::<Source>().set_value(11))
        .expect("edit");
    assert_eq!(
        runtime
            .read_outputs_general::<AsyncAdapter<AsyncDerivedConsumer>>()
            .expect("update"),
        11
    );
}

#[view(derived)]
pub struct First {
    pub value: u64,
}

#[view(derived)]
pub struct Second {
    pub value: u64,
}

fn first_unit() -> UnitKey {
    UnitKey::field(
        <First as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
    )
}

fn second_unit() -> UnitKey {
    UnitKey::field(
        <Second as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
    )
}

struct FirstProducer;

impl GeneralComponent for FirstProducer {
    type State = u64;
    type Output = ();

    const ID: ComponentId = ComponentId(0x00E0_0005);
    const NAME: &'static str = "claim_lifecycle::FirstProducer";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Source as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] = &[<First as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(source_unit())?;
        cx.output()
            .field(<First as ViewSchema>::view_id(), FieldId(0), value);
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

struct SecondProducer;

impl GeneralComponent for SecondProducer {
    type State = u64;
    type Output = ();

    const ID: ComponentId = ComponentId(0x00E0_0006);
    const NAME: &'static str = "claim_lifecycle::SecondProducer";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Source as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] = &[<Second as ViewSchema>::DESCRIPTOR];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(source_unit())?;
        cx.output()
            .field(<Second as ViewSchema>::view_id(), FieldId(0), value);
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

#[irrigation::irrigation]
mod scoped {
    use super::{First, Second, Source};

    #[irrigation::component]
    pub fn helper(seed: &Source) -> u64 {
        seed.value()
    }

    #[irrigation::component]
    pub fn root(seed: &Source, first: &First, second: &Second) -> u64 {
        let base = helper(seed);
        first.value() + second.value() - base
    }
}

#[test]
fn every_installed_component_owns_its_own_scope() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(FirstProducer)
        .expect("first");
    runtime
        .install_general_component(SecondProducer)
        .expect("second");
    // Each producer may define only its own View, which the runtime checks
    // against the planned provider component taken from the producer's scope.
    runtime.plan_route([
        (first_unit(), FirstProducer::ID),
        (second_unit(), SecondProducer::ID),
    ]);
    runtime.install(scoped::Root).expect("root");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(7))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs::<scoped::Root>()
            .expect("both producers keep their scopes"),
        7
    );
}

#[view(source)]
pub struct Unwritten {
    pub count: u64,
}

#[view(derived)]
pub struct Registry(irrigation::Map<u32, u32>);

fn count_unit() -> UnitKey {
    UnitKey::field(
        <Unwritten as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
    )
}

fn registry_structure_unit() -> UnitKey {
    irrigation::structure_unit(<Registry as ViewSchema>::view_id(), FieldId(0))
}

/// Requests a source scalar and a collection structure unit that no edit and no
/// definition has ever written, then reads the scalar back synchronously.
///
/// The first touch of each unit is the receipt: a receipt that creates an empty
/// node instead of materializing the committed content fails here, and would
/// poison every later reader of the same node.
struct RequestUnwritten;

impl AsyncGeneralComponent for RequestUnwritten {
    type State = (u64, u64);
    type Output = (u64, u64);

    const ID: ComponentId = ComponentId(0x00E0_0007);
    const NAME: &'static str = "claim_lifecycle::RequestUnwritten";
    const VIEWS: &'static [&'static ViewDescriptor] = &[
        <Unwritten as ViewSchema>::DESCRIPTOR,
        <Registry as ViewSchema>::DESCRIPTOR,
    ];

    fn dependency_updates(&self) -> DependencyUpdates {
        DependencyUpdates::Incremental
    }

    fn initialize<'a>(
        &'a self,
        cx: &'a mut GeneralCx,
    ) -> ComponentFuture<'a, (Self::State, Self::Output)> {
        Box::pin(async move {
            let requested: u64 = cx.request_typed(count_unit()).await?;
            let _structure = cx.request(registry_structure_unit()).await?;
            let read: u64 = cx.read_typed(count_unit())?;
            Ok(((requested, read), (requested, read)))
        })
    }

    fn update<'a>(
        &'a self,
        cx: &'a mut GeneralCx,
        _state: &'a Self::State,
        _changes: &'a ChangeBatch,
    ) -> ComponentFuture<'a, (Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[irrigation::irrigation]
mod unwritten_read {
    use super::{Registry, Unwritten};

    #[irrigation::component]
    pub fn read(tag: u64) -> (u64, usize) {
        let _ = tag;
        (Unwritten.count(), Registry.field0().len())
    }
}

#[test]
fn a_receipt_materializes_the_committed_content_of_untouched_units() {
    let runtime = Runtime::new();
    runtime
        .install_general(AsyncAdapter::new(RequestUnwritten))
        .expect("consumer");
    assert_eq!(
        runtime
            .read_outputs_general::<AsyncAdapter<RequestUnwritten>>()
            .expect("a receipt must see the committed defaults"),
        (0, 0)
    );
    // The receipt created both nodes. An accessor of the same units must still
    // see the committed content rather than an absent value.
    assert_eq!(
        runtime
            .call::<unwritten_read::__ReadSpec>(unwritten_read::__ReadArgs { tag: 0 })
            .expect("a later synchronous read must agree with the receipt"),
        (0, 0)
    );
}

#[view(source)]
pub struct Work {
    pub items: irrigation::List<u32>,
}

#[irrigation::irrigation]
mod parallel_members {
    use super::{Derived, Work};

    /// Reads the derived unit, so a member's ordered validation demands the
    /// (fragile) producer on the member's scoped thread.
    #[irrigation::component]
    pub fn probe(n: u32) -> u64 {
        Derived.value() + u64::from(n)
    }

    #[irrigation::component]
    pub fn total(work: &Work) -> u64 {
        work.items().map(|n| probe(*n)).iter().sum()
    }
}

/// Demands the root on its own thread, so a claim no task owns surfaces as a
/// timeout instead of hanging the gate run.
fn read_total(runtime: &Runtime) -> irrigation::Result<u64> {
    let (sender, receiver) = mpsc::channel();
    let runtime = runtime.clone();
    std::thread::spawn(move || {
        let _ = sender.send(runtime.read_outputs::<parallel_members::Total>());
    });
    match receiver.recv_timeout(READ_TIMEOUT) {
        Ok(result) => result,
        Err(_) => panic!("the demand parked on a claim no task owns"),
    }
}

#[test]
fn a_failing_parallel_member_releases_its_claim() {
    let runtime = Runtime::with_config(RuntimeConfig {
        workers: 2,
        stack_size: 32 * 1024 * 1024,
        ..RuntimeConfig::default()
    });
    runtime
        .install_general_component(FragileProducer)
        .expect("producer");
    runtime.install(parallel_members::Total).expect("root");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(7))
        .expect("seed");
    runtime
        .edit(|edit| {
            let mut work = edit.view::<Work>();
            let mut items = work.items();
            for n in 1..=12 {
                items.push(n);
            }
        })
        .expect("members");
    let expected: u64 = (1..=12).map(|n| 7 + n).sum();
    assert_eq!(read_total(&runtime).expect("initial read"), expected);

    // Every member has a value but a stale derived dependency, so its ordered
    // validation demands the producer and fails on the member's own thread.
    runtime
        .edit(|edit| edit.view::<Source>().set_value(11))
        .expect("edit");
    let error = read_total(&runtime).expect_err("the producer's failure must surface");
    assert!(
        format!("{error}").contains("rejected"),
        "unexpected error: {error}"
    );

    // The scoped threads are joined and gone: an unreleased claim would make
    // this demand wait for an owner that can never run again.
    runtime
        .edit(|edit| edit.view::<Source>().set_value(13))
        .expect("edit");
    let expected: u64 = (1..=12).map(|n| 13 + n).sum();
    assert_eq!(read_total(&runtime).expect("recovered read"), expected);
}

// ---------------------------------------------------------------------------
// Adapted producers keep their output Views (runtime-general-r4-01)
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct TypedOutput {
    pub value: u64,
}

#[view(derived)]
pub struct AsyncOutput {
    pub value: u64,
}

#[view(derived)]
pub struct LocalOutput {
    pub value: u64,
}

#[derive(Clone)]
struct SourceInputs {
    value: u64,
}

impl GeneralInputs for SourceInputs {
    fn read(cx: &mut GeneralCx) -> irrigation::Result<Self> {
        Ok(Self {
            value: cx.read_typed(source_unit())?,
        })
    }
}

struct TypedProducer;

impl TypedGeneralComponent for TypedProducer {
    type Inputs = SourceInputs;
    type Params = ();
    type State = u64;
    type Returns = u64;

    const ID: ComponentId = ComponentId(0x00E0_0011);
    const NAME: &'static str = "claim_lifecycle::TypedProducer";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Source as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] =
        &[<TypedOutput as ViewSchema>::DESCRIPTOR];

    fn params(&self) -> Self::Params {}

    fn initialize(
        &self,
        cx: &mut GeneralCx,
        inputs: &Self::Inputs,
        _params: &Self::Params,
    ) -> irrigation::Result<(Self::State, Self::Returns)> {
        cx.output().field(
            <TypedOutput as ViewSchema>::view_id(),
            FieldId(0),
            inputs.value,
        );
        Ok((inputs.value, inputs.value))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        inputs: &Self::Inputs,
        _changes: &ChangeBatch,
        _params: &Self::Params,
    ) -> irrigation::Result<(Self::State, Self::Returns)> {
        self.initialize(cx, inputs, &())
    }
}

struct AsyncProducer;

impl AsyncGeneralComponent for AsyncProducer {
    type State = u64;
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00E0_0012);
    const NAME: &'static str = "claim_lifecycle::AsyncProducer";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Source as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] =
        &[<AsyncOutput as ViewSchema>::DESCRIPTOR];

    fn initialize<'a>(
        &'a self,
        cx: &'a mut GeneralCx,
    ) -> ComponentFuture<'a, (Self::State, Self::Output)> {
        Box::pin(async move {
            let value: u64 = cx.read_typed(source_unit())?;
            cx.output()
                .field(<AsyncOutput as ViewSchema>::view_id(), FieldId(0), value);
            Ok((value, value))
        })
    }

    fn update<'a>(
        &'a self,
        cx: &'a mut GeneralCx,
        _state: &'a Self::State,
        _changes: &'a ChangeBatch,
    ) -> ComponentFuture<'a, (Self::State, Self::Output)> {
        Box::pin(async move {
            let value: u64 = cx.read_typed(source_unit())?;
            cx.output()
                .field(<AsyncOutput as ViewSchema>::view_id(), FieldId(0), value);
            Ok((value, value))
        })
    }
}

struct LocalProducer;

impl LocalGeneralComponent for LocalProducer {
    type State = std::rc::Rc<u64>;
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00E0_0013);
    const NAME: &'static str = "claim_lifecycle::LocalProducer";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Source as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] =
        &[<LocalOutput as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(source_unit())?;
        cx.output()
            .field(<LocalOutput as ViewSchema>::view_id(), FieldId(0), value);
        Ok((std::rc::Rc::new(value), value))
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

#[derive(Clone)]
struct ReadyInputs;

impl GeneralInputs for ReadyInputs {
    fn read(_cx: &mut GeneralCx) -> irrigation::Result<Self> {
        Ok(Self)
    }
}

/// Declares one output View and writes another one.
struct TypedMisplaced;

impl TypedGeneralComponent for TypedMisplaced {
    type Inputs = ReadyInputs;
    type Params = ();
    type State = u64;
    type Returns = u64;

    const ID: ComponentId = ComponentId(0x00E0_0017);
    const NAME: &'static str = "claim_lifecycle::TypedMisplaced";
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] =
        &[<TypedOutput as ViewSchema>::DESCRIPTOR];

    fn params(&self) -> Self::Params {}

    fn initialize(
        &self,
        cx: &mut GeneralCx,
        _inputs: &Self::Inputs,
        _params: &Self::Params,
    ) -> irrigation::Result<(Self::State, Self::Returns)> {
        cx.output()
            .field(<AsyncOutput as ViewSchema>::view_id(), FieldId(0), 1u64);
        Ok((1, 1))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        inputs: &Self::Inputs,
        _changes: &ChangeBatch,
        _params: &Self::Params,
    ) -> irrigation::Result<(Self::State, Self::Returns)> {
        self.initialize(cx, inputs, &())
    }
}

#[irrigation::irrigation]
mod adapted_producers {
    use super::{AsyncOutput, TypedOutput};

    #[irrigation::component]
    pub fn root() -> (u64, u64) {
        (TypedOutput.value(), AsyncOutput.value())
    }
}

#[test]
fn typed_and_async_adapters_publish_their_declared_output_views() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(TypedAdapter::new(TypedProducer))
        .expect("a typed adapter declares its output views");
    runtime
        .install_general_component(AsyncAdapter::new(AsyncProducer))
        .expect("an async adapter declares its output views");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(7))
        .expect("seed");
    runtime.install(adapted_producers::Root).expect("consumer");
    assert_eq!(
        runtime
            .read_outputs::<adapted_producers::Root>()
            .expect("read"),
        (7, 7)
    );
}

#[test]
fn a_typed_root_adapter_registers_its_output_views() {
    let runtime = Runtime::new();
    runtime
        .install_typed_general(TypedProducer)
        .expect("typed root");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(3))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs_general::<TypedAdapter<TypedProducer>>()
            .expect("read"),
        3
    );
    assert_eq!(
        runtime.snapshot().view::<TypedOutput>().value(),
        3,
        "the declared output view must be registered and published"
    );
}

#[test]
fn a_local_root_adapter_registers_its_output_views() {
    let runtime = Runtime::with_config(RuntimeConfig {
        workers: 0,
        ..RuntimeConfig::default()
    });
    runtime
        .install_local_general(LocalProducer)
        .expect("local root");
    runtime
        .edit(|edit| edit.view::<Source>().set_value(5))
        .expect("seed");
    assert_eq!(
        runtime
            .read_outputs_general::<LocalAdapter<LocalProducer>>()
            .expect("read"),
        5
    );
    assert_eq!(runtime.snapshot().view::<LocalOutput>().value(), 5);
}

#[test]
fn a_typed_root_adapter_enforces_its_output_views() {
    let runtime = Runtime::new();
    runtime.register_view(<AsyncOutput as ViewSchema>::DESCRIPTOR);
    runtime
        .install_typed_general(TypedMisplaced)
        .expect("typed root");
    let error = runtime
        .read_outputs_general::<TypedAdapter<TypedMisplaced>>()
        .expect_err("a write outside the declared output views must be refused");
    assert!(
        matches!(error, irrigation::ComputeError::DefinitionDenied { .. }),
        "expected a definition denial, got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Retraction is authorized like installation (runtime-general-r4-03)
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct Victim(irrigation::Map<u32, u32>);

#[view(derived)]
pub struct Owned {
    pub value: u64,
}

fn victim_value_unit() -> UnitKey {
    UnitKey::entity(
        <Victim as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
        irrigation::any_key(1u32),
    )
}

/// Publishes one entry in `Victim` as a non-root producer.
struct VictimProducer;

impl GeneralComponent for VictimProducer {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00E0_0014);
    const NAME: &'static str = "claim_lifecycle::VictimProducer";
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] = &[<Victim as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        cx.output()
            .entry(<Victim as ViewSchema>::view_id(), FieldId(0), 1u32, 10u32);
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

/// Declares `Owned` and retracts a unit in `Victim`, which it does not own.
struct ForeignRetractor;

impl GeneralComponent for ForeignRetractor {
    type State = ();
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00E0_0015);
    const NAME: &'static str = "claim_lifecycle::ForeignRetractor";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<Victim as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] = &[<Owned as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        // Read the unit first: its owner is live and the retraction has a
        // committed unit to remove.
        let _: u32 = cx.read_typed(victim_value_unit())?;
        cx.output()
            .field(<Owned as ViewSchema>::view_id(), FieldId(0), 1u64);
        cx.output().retract(victim_value_unit());
        Ok(((), 0))
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
fn a_retraction_outside_the_output_views_is_denied() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(VictimProducer)
        .expect("victim producer");
    runtime
        .install_general(ForeignRetractor)
        .expect("retractor");
    let error = runtime
        .read_outputs_general::<ForeignRetractor>()
        .expect_err("a retraction outside the output views must be refused");
    assert!(
        matches!(error, irrigation::ComputeError::DefinitionDenied { .. }),
        "expected a definition denial, got {error:?}"
    );
    assert_eq!(
        runtime.snapshot().view::<Victim>().field0(),
        vec![(1, 10)],
        "a refused retraction must leave the owner's unit intact"
    );
}

#[irrigation::irrigation]
mod foreign_owner {
    use super::{Victim, victim_value_unit};

    #[irrigation::component]
    pub fn owner(tag: u64) -> u32 {
        let _ = tag;
        Victim.define(1, 10);
        1
    }

    #[irrigation::component]
    pub fn intruder(tag: u64) -> u32 {
        let _ = tag;
        irrigation::ensure_view(<Victim as irrigation::ViewSchema>::DESCRIPTOR);
        irrigation::retract_unit(victim_value_unit(), "foreign");
        1
    }
}

#[test]
fn another_live_provider_unit_cannot_be_retracted() {
    let runtime = Runtime::new();
    runtime.register_view(<Victim as ViewSchema>::DESCRIPTOR);
    assert_eq!(
        runtime
            .call::<foreign_owner::__OwnerSpec>(foreign_owner::__OwnerArgs { tag: 0 })
            .expect("owner"),
        1
    );
    assert_eq!(runtime.snapshot().view::<Victim>().field0(), vec![(1, 10)]);

    let error = runtime
        .call::<foreign_owner::__IntruderSpec>(foreign_owner::__IntruderArgs { tag: 0 })
        .expect_err("a live provider's unit is not another component's to remove");
    assert!(
        matches!(error, irrigation::ComputeError::DefinitionDenied { .. }),
        "expected a definition denial, got {error:?}"
    );
    assert_eq!(
        runtime.snapshot().view::<Victim>().field0(),
        vec![(1, 10)],
        "the refused retraction must leave the owner's value intact"
    );
}

// ---------------------------------------------------------------------------
// A general write group commits atomically (runtime-kernel-03)
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct RequiredSlot {
    pub value: u64,
}

fn required_unit() -> UnitKey {
    UnitKey::field(
        <RequiredSlot as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
    )
}

/// Stages a definition and a requirement that cannot hold in this revision.
struct DeferredGeneral;

impl GeneralComponent for DeferredGeneral {
    type State = ();
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00E0_0016);
    const NAME: &'static str = "claim_lifecycle::DeferredGeneral";
    const VIEWS: &'static [&'static ViewDescriptor] = &[<RequiredSlot as ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static ViewDescriptor] = &[<Owned as ViewSchema>::DESCRIPTOR];

    fn initialize(&self, cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        cx.output()
            .field(<Owned as ViewSchema>::view_id(), FieldId(0), 1u64);
        irrigation::require_condition(false, &[required_unit()], "required port present");
        Ok(((), 0))
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
fn a_general_write_group_is_never_deferred() {
    let runtime = Runtime::new();
    runtime.install_general(DeferredGeneral).expect("install");
    let first = runtime
        .read_outputs_general::<DeferredGeneral>()
        .expect_err("the requirement can never hold");
    assert!(
        matches!(first, irrigation::ComputeError::PreconditionUnmet { .. }),
        "expected the unmet requirement to fail the attempt, got {first:?}"
    );
    assert_eq!(
        runtime.stats().deferred_writes,
        0,
        "a general component's group must not be held for a later drain"
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<Owned>().value(),
        0,
        "the failed group must not publish its definition"
    );
    let second = runtime
        .read_outputs_general::<DeferredGeneral>()
        .expect_err("the failed attempt must not have committed its output");
    assert!(
        matches!(second, irrigation::ComputeError::PreconditionUnmet { .. }),
        "a split commit left a certified output behind: {second:?}"
    );
}

/// A nested demand whose only work is to be a demand of its own.
#[irrigation::irrigation]
mod nested_demand {
    #[irrigation::component]
    pub fn probe(tag: u64) -> u64 {
        tag
    }
}

/// Asks to cancel the enclosing demand after running a nested one.
///
/// `Runtime::call` from a worker lands on the nested inline path, which
/// installs itself as the cancel target for the duration of the call. Once the
/// nested demand is over, `cancel` must target this demand again, so the
/// attempt cannot publish.
struct NestedDemandRoot {
    runtime: Runtime,
}

impl GeneralComponent for NestedDemandRoot {
    type State = u64;
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00E0_0018);
    const NAME: &'static str = "claim_lifecycle::NestedDemandRoot";

    fn initialize(&self, _cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let nested = self
            .runtime
            .call::<nested_demand::__ProbeSpec>(nested_demand::__ProbeArgs { tag: 7 })?;
        self.runtime.cancel();
        Ok((nested, nested))
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
fn a_nested_demand_restores_the_enclosing_cancel_target() {
    let runtime = Runtime::new();
    runtime
        .install_general(NestedDemandRoot {
            runtime: runtime.clone(),
        })
        .expect("root");
    let error = runtime
        .read_outputs_general::<NestedDemandRoot>()
        .expect_err("a cancelled attempt must not publish");
    assert!(
        matches!(error, irrigation::ComputeError::Cancelled),
        "the cancel after the nested demand did not reach the enclosing demand: {error:?}"
    );
}

// ---------------------------------------------------------------------------
// A deferred write group has one owner and one drain (runtime-kernel-r6-01,
// r6-runtime-state-01/02/03)
// ---------------------------------------------------------------------------

#[view(derived)]
pub struct Staged(pub irrigation::Map<u32, u32>);

fn staged_unit(key: u32) -> UnitKey {
    UnitKey::entity(
        <Staged as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
        irrigation::any_key(key),
    )
}

#[irrigation::irrigation]
mod staged_groups {
    use super::{Staged, staged_unit};
    use irrigation::ViewSchema;

    // Each helper stages one group from one call, so the requirement and the
    // definition share the call's frame and commit as one group; a lowered
    // component statement would give each its own site frame.

    /// Defers a group whose requirement a later sibling satisfies.
    fn stage_first() {
        irrigation::ensure_view(<Staged as ViewSchema>::DESCRIPTOR);
        irrigation::require_condition(false, &[staged_unit(9)], "driver present");
        irrigation::define_unit(staged_unit(1), 42u32, "first");
    }

    /// Reads the unit `first` installs, so installing `first` invalidates this
    /// group's trace.
    fn stage_middle() -> u32 {
        irrigation::ensure_view(<Staged as ViewSchema>::DESCRIPTOR);
        let seen = irrigation::try_read_unit_value(staged_unit(1), |_| None::<u32>);
        irrigation::require_condition(false, &[staged_unit(1)], "first's unit present");
        irrigation::define_unit(staged_unit(3), seen.unwrap_or(0), "middle");
        u32::from(seen.is_some())
    }

    /// Installs the unit `first` requires, so its group resolves at the drain.
    fn stage_driver() {
        irrigation::ensure_view(<Staged as ViewSchema>::DESCRIPTOR);
        irrigation::define_unit(staged_unit(9), 1u32, "driver");
    }

    /// Defers a group that stays current: it reads nothing and requires the
    /// unit `first` installs.
    fn stage_last() {
        irrigation::ensure_view(<Staged as ViewSchema>::DESCRIPTOR);
        irrigation::require_condition(false, &[staged_unit(1)], "first's unit present");
        irrigation::define_unit(staged_unit(4), 9u32, "last");
    }

    #[irrigation::component]
    pub fn first() -> u32 {
        stage_first();
        1
    }

    #[irrigation::component]
    pub fn middle() -> u32 {
        stage_middle()
    }

    #[irrigation::component]
    pub fn provide() -> u32 {
        stage_driver();
        9
    }

    #[irrigation::component]
    pub fn last() -> u32 {
        stage_last();
        4
    }

    #[irrigation::component]
    pub fn root() -> u32 {
        let _ = first();
        let _ = middle();
        let _ = provide();
        let _ = last();
        0
    }
}

#[test]
fn every_group_staged_after_a_stale_one_is_still_drained() {
    let runtime = Runtime::new();
    runtime.register_view(<Staged as ViewSchema>::DESCRIPTOR);
    runtime.install(staged_groups::Root).expect("install");
    runtime
        .read_outputs::<staged_groups::Root>()
        .expect("the retried demand must resolve every group it staged");
    let entries = runtime.inspect_snapshot().view::<Staged>().field0();
    assert!(entries.contains(&(1, 42)), "first's group: {entries:?}");
    assert!(entries.contains(&(3, 42)), "middle's group: {entries:?}");
    assert!(
        entries.contains(&(4, 9)),
        "the group staged after the stale one was dropped: {entries:?}"
    );
}

#[view(derived)]
pub struct NestedSlot(pub irrigation::Map<u32, u32>);

fn nested_slot_unit(key: u32) -> UnitKey {
    UnitKey::entity(
        <NestedSlot as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
        irrigation::any_key(key),
    )
}

#[irrigation::irrigation]
mod nested_deferral {
    use super::{NestedSlot, nested_slot_unit};
    use irrigation::ViewSchema;

    /// Stages one group from one call; see the note in `staged_groups`.
    fn stage_group() {
        irrigation::ensure_view(<NestedSlot as ViewSchema>::DESCRIPTOR);
        irrigation::require_condition(false, &[nested_slot_unit(2)], "port provided later");
        irrigation::define_unit(nested_slot_unit(1), 7u32, "stage");
    }

    fn provide_group() {
        irrigation::ensure_view(<NestedSlot as ViewSchema>::DESCRIPTOR);
        irrigation::define_unit(nested_slot_unit(2), 1u32, "provide");
    }

    /// Stages a group its enclosing demand can only resolve after this call.
    #[irrigation::component]
    pub fn stage(tag: u64) -> u32 {
        let _ = tag;
        stage_group();
        0
    }

    /// Provides the port `stage` requires, after the nested demand returned.
    #[irrigation::component]
    pub fn provide(tag: u64) -> u32 {
        let _ = tag;
        provide_group();
        0
    }
}

/// Calls a deferring component and then provides its port, from inside a
/// general root: the deferred group belongs to the enclosing attempt, which is
/// the only boundary that can drain it.
struct NestedDeferralRoot {
    runtime: Runtime,
}

impl GeneralComponent for NestedDeferralRoot {
    type State = u64;
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00E0_0019);
    const NAME: &'static str = "claim_lifecycle::NestedDeferralRoot";

    fn initialize(&self, _cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let staged = self
            .runtime
            .call::<nested_deferral::__StageSpec>(nested_deferral::__StageArgs { tag: 0 })?;
        let _ = self
            .runtime
            .call::<nested_deferral::__ProvideSpec>(nested_deferral::__ProvideArgs { tag: 0 })?;
        Ok((u64::from(staged), u64::from(staged)))
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

/// Runs the nested deferral program on a runtime with `workers` workers.
fn nested_deferral_resolves(workers: usize) {
    let runtime = Runtime::with_config(RuntimeConfig {
        workers,
        stack_size: 16 * 1024 * 1024,
        ..RuntimeConfig::default()
    });
    runtime.register_view(<NestedSlot as ViewSchema>::DESCRIPTOR);
    runtime
        .install_general(NestedDeferralRoot {
            runtime: runtime.clone(),
        })
        .expect("root");
    assert_eq!(
        runtime
            .read_outputs_general::<NestedDeferralRoot>()
            .unwrap_or_else(|error| panic!("workers = {workers}: {error:?}")),
        0
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<NestedSlot>().field0(),
        vec![(1, 7), (2, 1)],
        "the enclosing demand must drain the nested call's group (workers = {workers})"
    );
}

#[test]
fn a_nested_deferral_resolves_on_a_workerless_runtime() {
    nested_deferral_resolves(0);
}

#[test]
fn a_nested_deferral_resolves_with_a_worker() {
    nested_deferral_resolves(1);
}

#[view(derived)]
pub struct CancelSlot(pub irrigation::Map<u32, u32>);

fn cancel_slot_unit(key: u32) -> UnitKey {
    UnitKey::entity(
        <CancelSlot as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
        irrigation::any_key(key),
    )
}

#[irrigation::irrigation]
mod cancel_after_stage {
    use super::{CancelSlot, cancel_slot_unit};
    use irrigation::ViewSchema;

    /// Stages one group from one call; see the note in `staged_groups`.
    fn stage_group() {
        irrigation::ensure_view(<CancelSlot as ViewSchema>::DESCRIPTOR);
        irrigation::require_condition(false, &[cancel_slot_unit(2)], "port staged later");
        irrigation::define_unit(cancel_slot_unit(1), 7u32, "stage");
    }

    fn provide_group() {
        irrigation::ensure_view(<CancelSlot as ViewSchema>::DESCRIPTOR);
        irrigation::define_unit(cancel_slot_unit(2), 1u32, "provide");
    }

    #[irrigation::component]
    pub fn stage(tag: u64) -> u32 {
        let _ = tag;
        stage_group();
        0
    }

    #[irrigation::component]
    pub fn provide(tag: u64) -> u32 {
        let _ = tag;
        provide_group();
        0
    }
}

/// Number of attempts of [`CancelAfterStage`] that ran.
static CANCEL_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

/// Defers a group through a nested call, then cancels the enclosing attempt.
struct CancelAfterStage {
    runtime: Runtime,
}

impl GeneralComponent for CancelAfterStage {
    type State = u64;
    type Output = u64;

    const ID: ComponentId = ComponentId(0x00E0_001A);
    const NAME: &'static str = "claim_lifecycle::CancelAfterStage";

    fn initialize(&self, _cx: &mut GeneralCx) -> irrigation::Result<(Self::State, Self::Output)> {
        let staged = self
            .runtime
            .call::<cancel_after_stage::__StageSpec>(cancel_after_stage::__StageArgs { tag: 0 })?;
        let _ = self.runtime.call::<cancel_after_stage::__ProvideSpec>(
            cancel_after_stage::__ProvideArgs { tag: 0 },
        )?;
        if CANCEL_ATTEMPTS.fetch_add(1, Ordering::SeqCst) == 0 {
            // The attempt ends with `stage`'s group still pending, so its
            // teardown is the only owner the group has left.
            self.runtime.cancel();
        }
        Ok((u64::from(staged), u64::from(staged)))
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
fn a_teardown_reopens_the_owner_of_its_pending_group() {
    let runtime = Runtime::new();
    runtime.register_view(<CancelSlot as ViewSchema>::DESCRIPTOR);
    runtime
        .install_general(CancelAfterStage {
            runtime: runtime.clone(),
        })
        .expect("root");
    let first = runtime
        .read_outputs_general::<CancelAfterStage>()
        .expect_err("the cancelled attempt must not publish");
    assert!(
        matches!(first, irrigation::ComputeError::Cancelled),
        "expected the cancel to end the attempt, got {first:?}"
    );
    assert!(
        !runtime
            .inspect_snapshot()
            .view::<CancelSlot>()
            .field0()
            .iter()
            .any(|(key, _)| *key == 1),
        "the cancelled attempt must not install its group"
    );

    // The next demand runs at the same revision. The teardown could not check
    // the group's requirement, so it reopens the owner instead of failing it:
    // the owner re-executes, stages a fresh group, and this demand drains it.
    assert_eq!(
        runtime
            .read_outputs_general::<CancelAfterStage>()
            .expect("the reopened owner must stage its group again"),
        0
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<CancelSlot>().field0(),
        vec![(1, 7), (2, 1)],
        "the group a teardown dropped must be staged again by the next attempt"
    );
}

#[view(derived)]
pub struct MemberSlot(pub irrigation::Map<u32, u32>);

fn member_slot_unit(key: u32) -> UnitKey {
    UnitKey::entity(
        <MemberSlot as ViewSchema>::view_id(),
        FieldId(0),
        UnitKind::Value,
        irrigation::any_key(key),
    )
}

#[irrigation::irrigation]
mod deferring_members {
    use super::{MemberSlot, Work};
    use irrigation::ViewSchema;

    /// Stages one write group from a list member. With several workers the
    /// member runs on a scoped thread that belongs to no demand task, so no
    /// task will ever drain what it defers.
    #[irrigation::component]
    pub fn scan(work: &Work) -> u64 {
        let staged = work.items().map(|item| {
            let key: u32 = *item;
            irrigation::ensure_view(<MemberSlot as ViewSchema>::DESCRIPTOR);
            irrigation::require_condition(
                false,
                &[super::member_slot_unit(99)],
                "member port present",
            );
            irrigation::define_unit(super::member_slot_unit(key), key + 100, "member");
            u64::from(key)
        });
        staged.iter().sum()
    }

    /// Defines one of the units the members tried to stage, from a real task.
    #[irrigation::component]
    pub fn define_one(tag: u64) -> u32 {
        let _ = tag;
        irrigation::ensure_view(<MemberSlot as ViewSchema>::DESCRIPTOR);
        irrigation::define_unit(super::member_slot_unit(1), 9u32, "define_one");
        1
    }
}

#[test]
fn a_slotless_evaluation_may_not_defer_a_write_group() {
    let runtime = Runtime::with_config(RuntimeConfig {
        workers: 4,
        stack_size: 32 * 1024 * 1024,
        ..RuntimeConfig::default()
    });
    runtime.register_view(<MemberSlot as ViewSchema>::DESCRIPTOR);
    runtime.install(deferring_members::Scan).expect("root");
    runtime
        .edit(|edit| {
            let mut work = edit.view::<Work>();
            let mut items = work.items();
            for n in 1..=8 {
                items.push(n);
            }
        })
        .expect("members");
    let error = runtime
        .read_outputs::<deferring_members::Scan>()
        .expect_err("a member with no demand boundary must not defer its write");
    assert!(
        matches!(error, irrigation::ComputeError::PreconditionUnmet { .. }),
        "expected the requirement to fail the member's commit, got {error:?}"
    );
    assert_eq!(
        runtime.stats().deferred_writes,
        0,
        "a member has no demand boundary to defer to"
    );

    // Nothing was held back: the claims the members took are gone, so a real
    // demand can define the same unit instead of reporting a phantom writer.
    assert_eq!(
        runtime
            .call::<deferring_members::__DefineOneSpec>(deferring_members::__DefineOneArgs {
                tag: 0
            })
            .expect("a refused deferral must leave no claim behind"),
        1
    );
    assert_eq!(
        runtime.inspect_snapshot().view::<MemberSlot>().field0(),
        vec![(1, 9)]
    );
}
