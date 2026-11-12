//! Unit tests for `src/gate.rs`.

use super::*;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn a_writer_waits_for_every_reader() {
    let gate = Arc::new(Gate::new());
    let reader = gate.read();
    let writer = {
        let gate = Arc::clone(&gate);
        std::thread::spawn(move || {
            let _writer = gate.write();
        })
    };
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(gate.lock().readers, 1, "the writer must still be waiting");
    drop(reader);
    writer.join().expect("writer");
    assert_eq!(gate.lock().readers, 0);
}

#[test]
fn a_permit_can_be_released_by_another_thread() {
    let gate = Arc::new(Gate::new());
    gate.enter_read();
    let owner = Arc::clone(&gate);
    std::thread::spawn(move || owner.exit_read())
        .join()
        .expect("release");
    let _writer = gate.write();
}
