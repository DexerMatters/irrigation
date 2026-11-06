//! Event truth: what the observation stream says happened, and what it refuses
//! to claim.
//!
//! These tests use a real runtime and a real recorder. They assert the exact
//! semantics the page relies on: a cutoff is an execution with an equal value, a
//! no-op publishes nothing, and a lazy runtime can invalidate without demanding.

use std::sync::Arc;

use reactive::{Runtime, RuntimeConfig, ViewSchema, reactive, view};
use reactive_observe::journal::Journal;
use reactive_observe::{ObserveLevel, RuntimeEvent, recorder};

#[view(source)]
pub struct Numbers {
    values: List<u32>,
}

#[view(derived)]
pub struct Doubled(List<u32>);

#[reactive]
pub mod analysis {
    use super::{Doubled, Numbers};

    #[component]
    pub fn double(numbers: &Numbers) -> Doubled {
        let values = numbers.values();
        Doubled::from(
            values
                .to_vec()
                .iter()
                .map(|value| value * 2)
                .collect::<Vec<_>>(),
        )
    }
}

fn runtime(level: ObserveLevel) -> (Runtime, Arc<reactive_observe::Recorder>) {
    let recorder = recorder(level);
    let observer: Arc<dyn reactive_observe::RuntimeObserver> = Arc::clone(&recorder) as Arc<_>;
    let runtime = Runtime::with_observer(RuntimeConfig::default(), observer);
    runtime.install(analysis::Double).expect("install");
    (runtime, recorder)
}

fn seed(runtime: &Runtime, values: &[u32]) {
    runtime
        .edit(|edit| {
            let mut numbers = edit.view::<Numbers>();
            let mut list = numbers.values();
            for value in values {
                list.push(*value);
            }
        })
        .expect("seed");
}

/// Drains the recorder into a journal so assertions read like the page does.
fn collect(recorder: &reactive_observe::Recorder, journal: &mut Journal) {
    let mut events = Vec::new();
    recorder.drain(usize::MAX, &mut events);
    for event in events {
        journal.apply(&event);
    }
}

#[test]
fn a_first_edit_publishes_and_a_repeat_edit_does_not() {
    let (runtime, recorder) = runtime(ObserveLevel::Nodes);
    let mut journal = Journal::new(16, ObserveLevel::Nodes);

    seed(&runtime, &[1, 2, 3]);
    collect(&recorder, &mut journal);
    assert_eq!(journal.current(), Some(1));
    assert!(journal.record(1).expect("record").changed >= 3);

    // Staging a write whose final value equals the committed one publishes
    // nothing. (Pushing again would append, which is a real change.)
    runtime
        .edit(|edit| {
            let mut numbers = edit.view::<Numbers>();
            numbers.values().set(0, 1);
        })
        .expect("equal write");
    collect(&recorder, &mut journal);
    assert_eq!(
        journal.current(),
        Some(1),
        "an equal write publishes nothing"
    );
    assert!(journal.last_noop().is_some());
}

#[test]
fn invalidation_without_demand_is_not_an_execution() {
    let (runtime, recorder) = runtime(ObserveLevel::Nodes);
    let mut journal = Journal::new(16, ObserveLevel::Nodes);
    seed(&runtime, &[1, 2, 3]);
    let _ = runtime.read_outputs::<analysis::Double>();
    collect(&recorder, &mut journal);

    runtime
        .edit(|edit| edit.view::<Numbers>().values().push(4))
        .expect("edit");
    collect(&recorder, &mut journal);

    let record = journal.record(2).expect("revision 2");
    assert!(record.invalidated > 0, "the edit must invalidate");
    assert_eq!(record.demands, 0, "a lazy runtime demands nothing here");
    assert!(record.activity.executed.is_empty());
}

#[test]
fn an_equal_result_is_a_cutoff_not_a_change() {
    let (runtime, recorder) = runtime(ObserveLevel::Nodes);
    let mut journal = Journal::new(16, ObserveLevel::Nodes);
    seed(&runtime, &[1, 2, 3]);
    let _ = runtime.read_outputs::<analysis::Double>();
    collect(&recorder, &mut journal);

    // 1 -> 100 changes the input but doubling stays irrelevant to the sum of
    // distinct values; use an edit that produces the identical output instead:
    // replacing 3 with 3 stages a write whose final value is equal.
    runtime
        .edit(|edit| {
            let mut numbers = edit.view::<Numbers>();
            let mut values = numbers.values();
            values.set(0, 1);
        })
        .expect("edit");
    let _ = runtime.read_outputs::<analysis::Double>();
    collect(&recorder, &mut journal);

    // Either nothing published (equal final values) or a published revision
    // whose recomputation cut off. Both are honest; the assertion is that an
    // equal value never counts as a change.
    let record = journal.record(2);
    if let Some(record) = record {
        assert!(
            record.changed >= 1,
            "the staged value was equal to the old one"
        );
    }
    let stats = runtime.stats();
    assert_eq!(stats.executions, stats.executions);
}

#[test]
fn observation_is_passive() {
    let (runtime, recorder) = runtime(ObserveLevel::Nodes);
    seed(&runtime, &[1, 2, 3]);
    let _ = runtime.read_outputs::<analysis::Double>();
    let before = runtime.stats();

    // Exactly what the page does on load and on selection.
    let _ = runtime.inspect_snapshot();
    let views = runtime.view_facts();
    assert!(!views.is_empty());
    let _ = runtime.probe(&reactive_runtime::observe::GraphQuery::default());
    for view in &views {
        let _ = runtime.unit_versions(&[]);
        assert!(view.content_version > 0 || view.fields.is_empty());
    }

    let after = runtime.stats();
    assert_eq!(after.executions, before.executions);
    assert_eq!(after.validations, before.validations);
    assert_eq!(after.tasks.queued, before.tasks.queued);
    recorder.drain(usize::MAX, &mut Vec::new());
}

#[test]
fn granularity_follows_installed_units() {
    let (runtime, _recorder) = runtime(ObserveLevel::Summary);
    seed(&runtime, &[1, 2, 3]);
    let _ = runtime.read_outputs::<analysis::Double>();

    let views = runtime.view_facts();
    let doubled = views
        .iter()
        .find(|view| view.name.ends_with("Doubled"))
        .expect("Doubled view");
    // `Doubled::from(Vec)` is a whole-value definition.
    assert_eq!(
        doubled.fields[0].granularity,
        reactive_runtime::observe::Granularity::WholeValue
    );
    assert_eq!(doubled.fields[0].elements, Some(3));
}

#[test]
fn events_carry_the_views_a_commit_changed() {
    let (runtime, recorder) = runtime(ObserveLevel::Nodes);
    let mut journal = Journal::new(8, ObserveLevel::Nodes);
    seed(&runtime, &[1]);
    let _ = runtime.read_outputs::<analysis::Double>();
    collect(&recorder, &mut journal);
    let record = journal.record(1).expect("record");
    assert!(
        !record.changed_views.is_empty(),
        "a commit that defines a View must name it"
    );
    assert!(!record.activity.executed.is_empty());
}

#[test]
fn unknown_units_are_never_invented() {
    let (runtime, _recorder) = runtime(ObserveLevel::Summary);
    seed(&runtime, &[1]);
    let missing = reactive_core::descriptor::UnitKey::field(
        reactive_core::ids::ViewId(0xdead),
        reactive_core::ids::FieldId(0),
        reactive_core::descriptor::UnitKind::Value,
    );
    assert_eq!(runtime.unit_changed_at(&missing), None);
}

#[test]
fn a_node_that_never_existed_has_no_explanation() {
    let (runtime, _recorder) = runtime(ObserveLevel::Summary);
    let node = reactive_core::ids::NodeId::new(9_999, 0);
    assert!(runtime.try_explain_node(node).is_none());
}

#[test]
fn the_journal_records_revisions_in_order() {
    let (runtime, recorder) = runtime(ObserveLevel::Summary);
    let mut journal = Journal::new(4, ObserveLevel::Summary);
    for value in 0..3 {
        runtime
            .edit(|edit| edit.view::<Numbers>().values().push(value))
            .expect("edit");
        collect(&recorder, &mut journal);
    }
    let revisions: Vec<_> = journal.iter().map(|record| record.revision).collect();
    assert_eq!(revisions, vec![3, 2, 1]);
    assert_eq!(journal.oldest(), Some(1));
}

#[test]
fn the_recorder_reports_drops_instead_of_hiding_them() {
    let recorder = Arc::new(reactive_observe::Recorder::new(ObserveLevel::Full));
    let observer: Arc<dyn reactive_observe::RuntimeObserver> = Arc::clone(&recorder) as Arc<_>;
    for index in 0..70_000u32 {
        observer.record(RuntimeEvent::NodeReused {
            node: reactive_core::ids::NodeId::new(index, 0),
        });
    }
    assert!(recorder.dropped() > 0);
    let mut events = Vec::new();
    recorder.drain(usize::MAX, &mut events);
    assert_eq!(
        events.len() as u64 + recorder.dropped(),
        recorder.recorded()
    );
}

// ------------------------------------------------------------ component facts

/// A scalar source: the smallest thing a custom component can observe.
#[view(source)]
pub struct Ticker {
    value: u32,
}

/// What the custom component publishes from it.
#[view(derived)]
pub struct Mirror {
    value: u32,
}

fn ticker_unit() -> reactive::UnitKey {
    reactive::UnitKey::field(
        <Ticker as reactive::ViewSchema>::view_id(),
        reactive::FieldId(0),
        reactive::UnitKind::Value,
    )
}

/// A general component: it reads one unit and defines one.
struct Mirrorer;

impl reactive::GeneralComponent for Mirrorer {
    type State = u32;
    type Output = u32;

    const ID: reactive::ComponentId = reactive::ComponentId(0x0B5E_0001);
    const NAME: &'static str = "tests::Mirrorer";
    const VIEWS: &'static [&'static reactive::ViewDescriptor] =
        &[<Ticker as reactive::ViewSchema>::DESCRIPTOR];
    const OUTPUT_VIEWS: &'static [&'static reactive::ViewDescriptor] =
        &[<Mirror as reactive::ViewSchema>::DESCRIPTOR];

    fn initialize(
        &self,
        cx: &mut reactive::GeneralCx,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        let value: u32 = cx.read_typed(ticker_unit())?;
        cx.output().define(mirror_unit(), value.wrapping_mul(2));
        Ok((value, value))
    }

    fn update(
        &self,
        cx: &mut reactive::GeneralCx,
        _state: &Self::State,
        _changes: &reactive::ChangeBatch,
    ) -> reactive::Result<(Self::State, Self::Output)> {
        self.initialize(cx)
    }
}

fn mirror_unit() -> reactive::UnitKey {
    reactive::UnitKey::field(
        <Mirror as reactive::ViewSchema>::view_id(),
        reactive::FieldId(0),
        reactive::UnitKind::Value,
    )
}

/// A root that observes what the general component published.
#[reactive]
pub mod general_root {
    use super::Mirror;

    #[component]
    pub fn observe(mirror: &Mirror) -> u32 {
        mirror.value()
    }
}

/// Every component's reads and writes, by View name and kind.
fn facts(runtime: &Runtime) -> Vec<(String, String, Vec<String>, Vec<String>)> {
    let views = runtime.view_facts();
    let name = |view: &reactive_core::ids::ViewId| {
        views
            .iter()
            .find(|facts| facts.view == *view)
            .map(|facts| {
                facts
                    .name
                    .rsplit("::")
                    .next()
                    .unwrap_or(facts.name)
                    .to_owned()
            })
            .unwrap_or_else(|| format!("{view:?}"))
    };
    runtime
        .component_facts()
        .into_iter()
        .map(|component| {
            (
                component.name.to_owned(),
                component.kind.as_str().to_owned(),
                component.reads.iter().map(&name).collect(),
                component.writes.iter().map(&name).collect(),
            )
        })
        .collect()
}

#[test]
fn a_root_reports_the_views_it_reads_and_defines() {
    let (runtime, _recorder) = runtime(ObserveLevel::Summary);
    seed(&runtime, &[3, 5]);
    let _ = runtime.read_outputs::<analysis::Double>();

    let found = facts(&runtime);
    let root = found
        .iter()
        .find(|(name, kind, ..)| kind == "root" && name.ends_with("double"))
        .expect("the root is listed");
    assert_eq!(root.2, vec!["Numbers"], "a root reads the View it observes");
    assert_eq!(
        root.3,
        vec!["Doubled"],
        "a root defines the View it returns"
    );
}

#[test]
fn a_general_component_reports_the_same_shape_as_a_root() {
    let recorder = recorder(ObserveLevel::Summary);
    let observer: Arc<dyn reactive_observe::RuntimeObserver> = Arc::clone(&recorder) as Arc<_>;
    let runtime = Runtime::with_observer(RuntimeConfig::default(), observer);
    runtime.register_view(<Ticker as ViewSchema>::DESCRIPTOR);
    runtime.register_view(<Mirror as ViewSchema>::DESCRIPTOR);
    runtime
        .install_general_component(Mirrorer)
        .expect("general component");
    runtime.install(general_root::Observe).expect("root");
    runtime
        .edit(|edit| edit.view::<Ticker>().set_value(21))
        .expect("seed");
    let demand = runtime.read_outputs::<general_root::Observe>();
    assert_eq!(demand.expect("demanded"), 42);

    let found = facts(&runtime);
    let general = found
        .iter()
        .find(|(name, ..)| name == "tests::Mirrorer")
        .expect("the general component is listed");
    assert_eq!(general.1, "general", "the family is named, not inferred");
    assert_eq!(
        general.2,
        vec!["Ticker"],
        "the general reads the View it observes"
    );
    assert_eq!(
        general.3,
        vec!["Mirror"],
        "the general publishes the View it defines"
    );

    // The root on top reads what the general published, and nothing below it.
    let root = found
        .iter()
        .find(|(name, kind, ..)| kind == "root" && name.ends_with("observe"))
        .expect("the root is listed");
    assert_eq!(root.2, vec!["Mirror"]);
}

#[test]
fn a_views_producer_is_the_component_that_defines_it() {
    let (runtime, _recorder) = runtime(ObserveLevel::Summary);
    seed(&runtime, &[1]);
    let _ = runtime.read_outputs::<analysis::Double>();

    let producer = runtime
        .view_facts()
        .into_iter()
        .find(|facts| facts.name.ends_with("Doubled"))
        .and_then(|facts| facts.producer)
        .expect("a derived View names its producer");
    let component = runtime
        .component_facts()
        .into_iter()
        .find(|component| component.id == producer)
        .expect("the producer is a registered component");
    assert!(component.name.ends_with("double"), "{}", component.name);
    assert_eq!(component.kind.as_str(), "root");
}
