//! End-to-end test of the macro surface: views, a root component, edits,
//! snapshots, and typed outputs.

use reactive::{component, reactive, view, Runtime};

#[view(source)]
pub struct Inputs {
    numbers: List<u32>,
    strict: bool,
}

#[view(derived)]
pub struct Squares(Map<u32, u32>);

#[reactive]
mod analysis {
    use super::{Inputs, Squares};

    #[component]
    pub fn analyze(inputs: &Inputs) -> (Squares, u32) {
        let numbers = inputs.numbers();
        let doubled = numbers.map(|n| *n * 2);
        let total = doubled.iter().sum::<u32>();
        let squares = Squares::from(numbers.map(|n| (*n * 2, *n * *n)));
        (squares, total)
    }
}

fn runtime() -> Runtime {
    let runtime = Runtime::new();
    runtime.install(analysis::Analyze).expect("install");
    runtime
}

#[test]
fn views_components_and_edits_work_end_to_end() {
    let runtime = runtime();
    runtime
        .edit(|edit| {
            let mut inputs = edit.view::<Inputs>();
            let mut numbers = inputs.numbers();
            numbers.push(1);
            numbers.push(2);
            numbers.push(3);
        })
        .expect("edit");

    let snapshot = runtime.read().expect("read");
    let mut squares = snapshot.view::<Squares>().field0();
    squares.sort();
    assert_eq!(squares, vec![(2, 1), (4, 4), (6, 9)]);

    let (_, total) = runtime.read_outputs::<analysis::Analyze>().expect("outputs");
    assert_eq!(total, 12);
}

#[test]
fn a_point_edit_repairs_only_the_affected_member() {
    let runtime = runtime();
    runtime
        .edit(|edit| {
            let mut inputs = edit.view::<Inputs>();
            let mut numbers = inputs.numbers();
            numbers.push(1);
            numbers.push(2);
        })
        .expect("edit");
    let _ = runtime.read().expect("read");

    runtime
        .edit(|edit| {
            let mut inputs = edit.view::<Inputs>();
            let mut numbers = inputs.numbers();
            numbers.set(0, 10);
        })
        .expect("edit");

    let (_, total) = runtime.read_outputs::<analysis::Analyze>().expect("outputs");
    assert_eq!(total, 2 * 10 + 2 * 2);

    let snapshot = runtime.read().expect("read");
    let mut squares = snapshot.view::<Squares>().field0();
    squares.sort();
    assert_eq!(squares, vec![(4, 4), (20, 100)]);
}

#[test]
fn unrelated_edits_do_not_publish_a_revision() {
    let runtime = runtime();
    runtime
        .edit(|edit| {
            edit.view::<Inputs>().set_strict(true);
        })
        .expect("edit");
    let revision = runtime.revision();
    runtime
        .edit(|edit| {
            edit.view::<Inputs>().set_strict(true);
        })
        .expect("edit");
    assert_eq!(runtime.revision(), revision);
}

#[test]
fn snapshots_keep_their_revision_after_edits() {
    let runtime = runtime();
    runtime
        .edit(|edit| {
            let mut inputs = edit.view::<Inputs>();
            let mut numbers = inputs.numbers();
            numbers.push(4);
        })
        .expect("edit");
    let snapshot = runtime.read().expect("read");
    runtime
        .edit(|edit| {
            let mut inputs = edit.view::<Inputs>();
            let mut numbers = inputs.numbers();
            numbers.push(5);
        })
        .expect("edit");
    assert_eq!(snapshot.view::<Inputs>().numbers(), vec![4]);
    let next = runtime.read().expect("read");
    assert_eq!(next.view::<Inputs>().numbers(), vec![4, 5]);
}
