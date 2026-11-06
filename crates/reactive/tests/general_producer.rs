use reactive::{
    ComponentId, DependencyUpdates, FieldId, GeneralComponent, GeneralCx, Runtime, UnitKey,
    UnitKind, ViewDescriptor, ViewSchema, view,
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

    fn initialize(&self, cx: &mut GeneralCx) -> reactive::Result<(Self::State, Self::Output)> {
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
        _changes: &reactive::ChangeBatch,
    ) -> reactive::Result<(Self::State, Self::Output)> {
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

    fn initialize(&self, cx: &mut GeneralCx) -> reactive::Result<(Self::State, Self::Output)> {
        let value: u64 = cx.read_typed(produced_unit())?;
        Ok(((), value))
    }

    fn update(
        &self,
        cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &reactive::ChangeBatch,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn general_component_producer_is_demanded_by_output_view() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(Producer)
        .expect("producer");
    runtime.install_general(Consumer).expect("consumer");
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
}

#[reactive::reactive]
mod app {
    use super::Produced;

    #[reactive::component]
    pub fn consume(produced: &Produced) -> u64 {
        produced.value()
    }
}

#[test]
fn ordinary_root_component_demands_a_general_producer() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(Producer)
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
    runtime.install_general_component(Producer).expect("first");
    assert!(runtime.install_general_component(Producer).is_err());
}

struct NoOutput;

impl GeneralComponent for NoOutput {
    type State = ();
    type Output = ();

    const ID: ComponentId = ComponentId(0x00C0_0003);
    const NAME: &'static str = "tests::NoOutput";

    fn initialize(&self, _cx: &mut GeneralCx) -> reactive::Result<(Self::State, Self::Output)> {
        Ok(((), ()))
    }

    fn update(
        &self,
        _cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &reactive::ChangeBatch,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        Ok(((), ()))
    }
}

#[test]
fn non_root_general_component_requires_an_output_view() {
    let runtime = Runtime::new();
    assert!(runtime.install_general_component(NoOutput).is_err());
}

#[reactive::view(source)]
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

    fn initialize(&self, _cx: &mut GeneralCx) -> reactive::Result<(Self::State, Self::Output)> {
        Ok(((), ()))
    }

    fn update(
        &self,
        _cx: &mut GeneralCx,
        _state: &Self::State,
        _changes: &reactive::ChangeBatch,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        Ok(((), ()))
    }
}

#[test]
fn source_views_cannot_be_produced_by_general_components() {
    let runtime = Runtime::new();
    assert!(
        runtime
            .install_general_component(SourceOutputProducer)
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

    fn initialize(&self, cx: &mut GeneralCx) -> reactive::Result<(Self::State, Self::Output)> {
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
        _changes: &reactive::ChangeBatch,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn definitions_outside_output_views_fail_as_component_errors() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(BadProducer)
        .expect("producer");
    runtime.install_general(Consumer).expect("consumer");
    runtime
        .edit(|edit| edit.view::<Input>().set_value(1))
        .expect("edit");
    assert!(runtime.read_outputs_general::<Consumer>().is_err());
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

    fn initialize(&self, cx: &mut GeneralCx) -> reactive::Result<(Self::State, Self::Output)> {
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
        _changes: &reactive::ChangeBatch,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

#[test]
fn a_unit_not_restaged_is_kept() {
    let runtime = Runtime::new();
    runtime
        .install_general_component(PreservingProducer)
        .expect("producer");
    runtime.install_general(Consumer).expect("consumer");
    runtime
        .edit(|edit| edit.view::<Input>().set_value(5))
        .expect("edit");
    assert_eq!(runtime.read_outputs_general::<Consumer>().expect("read"), 5);
    runtime
        .edit(|edit| edit.view::<Input>().set_value(9))
        .expect("edit");
    assert_eq!(runtime.read_outputs_general::<Consumer>().expect("read"), 9);
}
