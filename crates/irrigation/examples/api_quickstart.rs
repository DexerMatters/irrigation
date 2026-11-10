//! The README quick start, compiled as a gate.

use irrigation::prelude::*;

#[view(source)]
pub struct Inputs {
    numbers: List<u32>,
    strict: bool,
}

#[view(derived)]
pub struct Squares(Map<u32, u32>);

#[component(root)]
pub fn analyze(inputs: &Inputs) -> (Squares, u64) {
    let numbers = inputs.numbers();
    let doubled = numbers.map(|n| u64::from(*n) * 2);
    let total = doubled.iter().sum::<u64>();
    let squares = Squares::from(numbers.map(|n| (*n * 2, *n * *n)));
    (squares, total)
}

fn main() -> irrigation::Result<()> {
    let runtime = Runtime::new();
    let app = runtime.install_root(Analyze)?;

    runtime.edit_view::<Inputs>(|inputs| {
        let mut numbers = inputs.numbers();
        numbers.push(1);
        numbers.push(2);
    })?;

    let snapshot = runtime.read()?;
    let entries = snapshot.view::<Squares>().value();
    assert_eq!(entries, vec![(2, 1), (4, 4)]);
    let (_, total) = app.read()?;
    assert_eq!(total, 6);

    let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&seen);
    let _subscription = runtime.observe_view::<Squares>(move |squares| {
        counter.fetch_add(squares.value().len(), std::sync::atomic::Ordering::Relaxed);
    })?;

    runtime.edit_view::<Inputs>(|inputs| {
        let mut numbers = inputs.numbers();
        numbers.push(3);
    })?;
    assert_eq!(
        seen.load(std::sync::atomic::Ordering::Relaxed),
        3,
        "the observer sees the three-entry map"
    );
    Ok(())
}
