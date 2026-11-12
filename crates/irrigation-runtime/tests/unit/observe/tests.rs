//! Unit tests for `src/observe.rs`.

use std::sync::{Arc, Mutex};

use irrigation_core::descriptor::ViewDescriptor;
use irrigation_core::ids::ComponentId;
use irrigation_core::value::{Value, value_of};

use super::{ObserveLevel, RuntimeEvent, RuntimeObserver};

#[derive(Default)]
struct Recorded(Mutex<Vec<RuntimeEvent>>);

struct TestObserver {
    level: ObserveLevel,
    events: Arc<Recorded>,
}

impl RuntimeObserver for TestObserver {
    fn level(&self) -> ObserveLevel {
        self.level
    }

    fn record(&self, event: RuntimeEvent) {
        self.events.0.lock().expect("lock").push(event);
    }
}

struct ConstantRoot;

impl crate::component::RootComponent for ConstantRoot {
    type Output = u32;

    const ID: ComponentId = ComponentId(0x00C2_0001);
    const NAME: &'static str = "tests::ConstantRoot";
    const ARITY: usize = 1;
    const VIEWS: &'static [&'static ViewDescriptor] = &[];

    fn run() -> u32 {
        7
    }

    fn project(output: &u32, _index: usize) -> Value {
        value_of(*output)
    }

    fn reconstruct(parts: &[Value]) -> u32 {
        *parts[0].downcast_ref::<u32>().expect("output")
    }

    fn projection_eq(_index: usize, left: &Value, right: &Value) -> bool {
        left.downcast_ref::<u32>() == right.downcast_ref::<u32>()
    }
}

/// Demands a root so the observer has a transaction and a demand to report.
fn demand(runtime: &crate::Runtime) {
    runtime.install(ConstantRoot).expect("install");
    assert_eq!(runtime.read_outputs::<ConstantRoot>().expect("read"), 7);
}

fn observed(level: ObserveLevel) -> (crate::Runtime, Arc<Recorded>) {
    let events = Arc::new(Recorded::default());
    let observer: Arc<dyn RuntimeObserver> = Arc::new(TestObserver {
        level,
        events: Arc::clone(&events),
    });
    (
        crate::Runtime::with_observer(crate::inner::RuntimeConfig::default(), observer),
        events,
    )
}

#[test]
fn observation_disabled_delivers_nothing() {
    let (runtime, events) = observed(ObserveLevel::Off);
    demand(&runtime);
    assert!(
        events.0.lock().expect("lock").is_empty(),
        "an off observer must receive no events"
    );
}

#[test]
fn a_summary_observer_does_not_see_node_events() {
    let (runtime, events) = observed(ObserveLevel::Summary);
    demand(&runtime);
    let recorded = events.0.lock().expect("lock");
    assert!(
        !recorded.is_empty(),
        "a summary observer still sees the demand it caused"
    );
    let levels: Vec<ObserveLevel> = recorded.iter().map(RuntimeEvent::level).collect();
    assert!(
        levels.iter().all(|level| *level <= ObserveLevel::Summary),
        "a summary observer must not see node or task events: {levels:?}"
    );
}

#[test]
fn a_full_observer_sees_the_whole_stream() {
    let (runtime, events) = observed(ObserveLevel::Full);
    demand(&runtime);
    let recorded = events.0.lock().expect("lock");
    assert!(
        recorded
            .iter()
            .any(|event| event.level() == ObserveLevel::Full),
        "a full observer still sees task events"
    );
}
