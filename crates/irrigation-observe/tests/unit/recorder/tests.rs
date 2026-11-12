//! Unit tests for `src/recorder.rs`.

use super::*;
use irrigation_core::ids::NodeId;

#[test]
fn records_up_to_capacity_and_reports_drops() {
    let recorder = Recorder::new(ObserveLevel::Full);
    for _ in 0..RING_CAPACITY + 3 {
        recorder.record(RuntimeEvent::NodeReused {
            node: NodeId::new(0, 0),
        });
    }
    assert_eq!(recorder.recorded(), (RING_CAPACITY + 3) as u64);
    assert_eq!(recorder.dropped(), 3);
    let mut events = Vec::new();
    recorder.drain(usize::MAX, &mut events);
    assert_eq!(events.len(), RING_CAPACITY);
    assert!(recorder.is_empty());
}
