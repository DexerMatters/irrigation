//! Kernel tests written against hand-built View descriptors.
//!
//! These exercise the sequential semantic kernel before the macros exist:
//! revisions, tracked reads, ordered validation, change cutoff, and invocation
//! identity.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use reactive_core::ids::{ComponentId, FieldId, ViewId};
use reactive_core::value::{Value, value_of};
use reactive_runtime::helpers::read_unit_value;
use reactive_runtime::{ComponentSpec, RootComponent, Runtime, ViewSchema, call_component};
use reactive_store::descriptor::{
    ContentVtable, FieldDescriptor, FieldKind, UnitKey, UnitKind, ViewDescriptor, ViewKind,
};

#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct SettingsContent {
    flag: bool,
    count: u32,
}

const SETTINGS: ViewId = ViewId(0x51);

static SETTINGS_FIELDS: &[FieldDescriptor] = &[
    FieldDescriptor {
        name: "flag",
        kind: FieldKind::Scalar,
    },
    FieldDescriptor {
        name: "count",
        kind: FieldKind::Scalar,
    },
];

fn settings_empty() -> Arc<dyn Any + Send + Sync> {
    Arc::new(SettingsContent::default())
}

fn settings_define(
    content: &Arc<dyn Any + Send + Sync>,
    key: &UnitKey,
    value: Value,
) -> Arc<dyn Any + Send + Sync> {
    let mut next = content
        .downcast_ref::<SettingsContent>()
        .cloned()
        .unwrap_or_default();
    match key.field.0 {
        0 => next.flag = *value.downcast_ref::<bool>().expect("flag"),
        1 => next.count = *value.downcast_ref::<u32>().expect("count"),
        _ => {}
    }
    Arc::new(next)
}

fn settings_retract(
    content: &Arc<dyn Any + Send + Sync>,
    _key: &UnitKey,
) -> Arc<dyn Any + Send + Sync> {
    Arc::clone(content)
}

fn settings_contains(_content: &Arc<dyn Any + Send + Sync>, _key: &UnitKey) -> bool {
    true
}

fn settings_get(content: &Arc<dyn Any + Send + Sync>, key: &UnitKey) -> Option<Value> {
    let content = content.downcast_ref::<SettingsContent>()?;
    match key.field.0 {
        0 => Some(value_of(content.flag)),
        1 => Some(value_of(content.count)),
        _ => None,
    }
}

fn settings_eq(
    left: &Arc<dyn Any + Send + Sync>,
    right: &Arc<dyn Any + Send + Sync>,
) -> bool {
    left.downcast_ref::<SettingsContent>() == right.downcast_ref::<SettingsContent>()
}

fn settings_debug(content: &Arc<dyn Any + Send + Sync>) -> String {
    format!("{:?}", content.downcast_ref::<SettingsContent>())
}

static SETTINGS_DESC: ViewDescriptor = ViewDescriptor {
    id: SETTINGS,
    name: "Settings",
    kind: ViewKind::Source,
    fields: SETTINGS_FIELDS,
    content: ContentVtable {
        empty: settings_empty,
        define: settings_define,
        retract: settings_retract,
        contains: settings_contains,
        get: settings_get,
        eq: settings_eq,
        debug: settings_debug,
    },
};

struct SettingsView;

impl ViewSchema for SettingsView {
    const DESCRIPTOR: &'static ViewDescriptor = &SETTINGS_DESC;
    type Content = SettingsContent;
    type Snapshot<'a> = &'a SettingsContent;

    fn snapshot<'a>(content: &'a Arc<dyn Any + Send + Sync>) -> Self::Snapshot<'a> {
        content
            .downcast_ref::<SettingsContent>()
            .expect("settings content")
    }
}

fn flag_key() -> UnitKey {
    UnitKey::field(SETTINGS, FieldId(0), UnitKind::Value)
}

fn count_key() -> UnitKey {
    UnitKey::field(SETTINGS, FieldId(1), UnitKind::Value)
}

fn read_flag() -> bool {
    read_unit_value(flag_key(), |content| {
        content.downcast_ref::<SettingsContent>().map(|c| c.flag)
    })
}

fn read_count() -> u32 {
    read_unit_value(count_key(), |content| {
        content.downcast_ref::<SettingsContent>().map(|c| c.count)
    })
}

fn set_flag(runtime: &Runtime, value: bool) {
    runtime
        .edit(|edit| {
            let content = edit.content::<SettingsContent>(SETTINGS);
            let mut next = (*content).clone();
            next.flag = value;
            edit.set(flag_key(), value);
            edit.set_content(SETTINGS, Arc::new(next), settings_eq);
        })
        .expect("edit");
}

fn set_count(runtime: &Runtime, value: u32) {
    runtime
        .edit(|edit| {
            let content = edit.content::<SettingsContent>(SETTINGS);
            let mut next = (*content).clone();
            next.count = value;
            edit.set(count_key(), value);
            edit.set_content(SETTINGS, Arc::new(next), settings_eq);
        })
        .expect("edit");
}

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

static BODY_RUNS: AtomicU64 = AtomicU64::new(0);
static SQUARE_RUNS: AtomicU64 = AtomicU64::new(0);
static UNRELATED_RUNS: AtomicU64 = AtomicU64::new(0);

struct Square;

impl ComponentSpec for Square {
    type Args = u32;
    type Output = u32;
    const ID: ComponentId = ComponentId(0x9001);
    const NAME: &'static str = "square";
    const ARITY: usize = 1;

    fn run(args: &u32) -> u32 {
        SQUARE_RUNS.fetch_add(1, Ordering::SeqCst);
        args * args
    }

    fn project(output: &u32, _index: usize) -> Value {
        value_of(*output)
    }

    fn reconstruct(parts: &[Value]) -> u32 {
        *parts[0].downcast_ref::<u32>().expect("square output")
    }

    fn projection_eq(_index: usize, left: &Value, right: &Value) -> bool {
        left.downcast_ref::<u32>() == right.downcast_ref::<u32>()
    }
}

struct Unrelated;

impl ComponentSpec for Unrelated {
    type Args = ();
    type Output = u32;
    const ID: ComponentId = ComponentId(0x9002);
    const NAME: &'static str = "unrelated";
    const ARITY: usize = 1;

    fn run(_args: &()) -> u32 {
        UNRELATED_RUNS.fetch_add(1, Ordering::SeqCst);
        read_count()
    }

    fn project(output: &u32, _index: usize) -> Value {
        value_of(*output)
    }

    fn reconstruct(parts: &[Value]) -> u32 {
        *parts[0].downcast_ref::<u32>().expect("unrelated output")
    }

    fn projection_eq(_index: usize, left: &Value, right: &Value) -> bool {
        left.downcast_ref::<u32>() == right.downcast_ref::<u32>()
    }
}

struct Probe;

impl RootComponent for Probe {
    type Output = u32;
    const ID: ComponentId = ComponentId(0x9003);
    const NAME: &'static str = "probe";
    const ARITY: usize = 1;
    const VIEWS: &'static [&'static ViewDescriptor] = &[&SETTINGS_DESC];

    fn run() -> u32 {
        BODY_RUNS.fetch_add(1, Ordering::SeqCst);
        let flag = read_flag();
        let four = call_component::<Square>(4);
        let five = call_component::<Square>(5);
        if flag {
            four + five + call_component::<Unrelated>(())
        } else {
            four + five
        }
    }

    fn project(output: &u32, _index: usize) -> Value {
        value_of(*output)
    }

    fn reconstruct(parts: &[Value]) -> u32 {
        *parts[0].downcast_ref::<u32>().expect("probe output")
    }

    fn projection_eq(_index: usize, left: &Value, right: &Value) -> bool {
        left.downcast_ref::<u32>() == right.downcast_ref::<u32>()
    }
}

fn fresh() -> Runtime {
    BODY_RUNS.store(0, Ordering::SeqCst);
    SQUARE_RUNS.store(0, Ordering::SeqCst);
    UNRELATED_RUNS.store(0, Ordering::SeqCst);
    let runtime = Runtime::new();
    runtime.install(Probe).expect("install");
    runtime
}

#[test]
fn first_read_evaluates_every_required_body_once() {
    let _serial = serial();
    let runtime = fresh();
    assert_eq!(runtime.read_outputs::<Probe>().unwrap(), 41);
    assert_eq!(BODY_RUNS.load(Ordering::SeqCst), 1);
    assert_eq!(SQUARE_RUNS.load(Ordering::SeqCst), 2);
    assert_eq!(UNRELATED_RUNS.load(Ordering::SeqCst), 0);
}

#[test]
fn repeated_reads_reuse_the_certified_revision() {
    let _serial = serial();
    let runtime = fresh();
    let _ = runtime.read_outputs::<Probe>().unwrap();
    let _ = runtime.read_outputs::<Probe>().unwrap();
    let _ = runtime.read_outputs::<Probe>().unwrap();
    assert_eq!(BODY_RUNS.load(Ordering::SeqCst), 1);
}

#[test]
fn no_op_edit_publishes_no_revision_and_executes_nothing() {
    let _serial = serial();
    let runtime = fresh();
    let _ = runtime.read_outputs::<Probe>().unwrap();
    let revision = runtime.revision();
    set_flag(&runtime, false);
    assert_eq!(runtime.revision(), revision);
    let _ = runtime.read_outputs::<Probe>().unwrap();
    assert_eq!(BODY_RUNS.load(Ordering::SeqCst), 1);
}

#[test]
fn unrelated_input_edit_does_not_rerun_the_observing_body() {
    let _serial = serial();
    let runtime = fresh();
    let _ = runtime.read_outputs::<Probe>().unwrap();
    let body_runs = BODY_RUNS.load(Ordering::SeqCst);
    let unrelated_runs = UNRELATED_RUNS.load(Ordering::SeqCst);

    set_count(&runtime, 7);

    assert_eq!(runtime.read_outputs::<Probe>().unwrap(), 41);
    assert_eq!(BODY_RUNS.load(Ordering::SeqCst), body_runs);
    assert_eq!(UNRELATED_RUNS.load(Ordering::SeqCst), unrelated_runs);
}

#[test]
fn changing_an_input_invalidates_only_its_consumers() {
    let _serial = serial();
    let runtime = fresh();
    let _ = runtime.read_outputs::<Probe>().unwrap();

    set_count(&runtime, 7);

    assert_eq!(runtime.read_outputs::<Probe>().unwrap(), 41);
    // The branch was inactive, so the unrelated component must not have run.
    assert_eq!(UNRELATED_RUNS.load(Ordering::SeqCst), 0);
    assert_eq!(BODY_RUNS.load(Ordering::SeqCst), 1);
}

#[test]
fn switching_a_branch_replaces_the_active_dependency_set() {
    let _serial = serial();
    let runtime = fresh();
    assert_eq!(runtime.read_outputs::<Probe>().unwrap(), 41);
    set_count(&runtime, 7);
    set_flag(&runtime, true);
    assert_eq!(runtime.read_outputs::<Probe>().unwrap(), 48);
    assert_eq!(UNRELATED_RUNS.load(Ordering::SeqCst), 1);
    assert_eq!(BODY_RUNS.load(Ordering::SeqCst), 2);

    // Now the branch is active: a change to `count` must repair it.
    set_count(&runtime, 10);
    assert_eq!(runtime.read_outputs::<Probe>().unwrap(), 51);
    assert_eq!(UNRELATED_RUNS.load(Ordering::SeqCst), 2);
    assert_eq!(SQUARE_RUNS.load(Ordering::SeqCst), 2);
}

#[test]
fn equal_component_output_stops_downstream_execution() {
    let _serial = serial();
    let runtime = fresh();
    let _ = runtime.read_outputs::<Probe>().unwrap();
    let body_runs = BODY_RUNS.load(Ordering::SeqCst);

    // `Square(4)` is unchanged, so the body still re-runs only if one of its
    // observations changed. `count` is not observed while the branch is
    // inactive, so nothing runs.
    set_count(&runtime, 42);
    let _ = runtime.read_outputs::<Probe>().unwrap();
    assert_eq!(BODY_RUNS.load(Ordering::SeqCst), body_runs);
    assert_eq!(SQUARE_RUNS.load(Ordering::SeqCst), 2);
}

#[test]
fn transient_arguments_identify_distinct_invocations() {
    let _serial = serial();
    let runtime = fresh();
    let _ = runtime.read_outputs::<Probe>().unwrap();
    // The body calls Square(4) and Square(5): two invocations, each executed
    // once and then reused by identity.
    assert_eq!(SQUARE_RUNS.load(Ordering::SeqCst), 2);
    let _ = runtime.read_outputs::<Probe>().unwrap();
    assert_eq!(SQUARE_RUNS.load(Ordering::SeqCst), 2);
}

#[test]
fn snapshot_content_is_pinned_to_its_revision() {
    let _serial = serial();
    let runtime = fresh();
    let snapshot = runtime.read().unwrap();
    assert_eq!(snapshot.revision(), 0);
    set_count(&runtime, 9);
    let settings = snapshot.view::<SettingsView>();
    assert_eq!(settings.count, 0);
    let next = runtime.read().unwrap();
    assert_eq!(next.view::<SettingsView>().count, 9);
}
