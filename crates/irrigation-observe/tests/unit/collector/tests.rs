//! Unit tests for `src/collector.rs`.

use super::*;
use std::sync::Arc;

#[test]
fn slow_subscribers_do_not_block_broadcasts() {
    let collector = test_collector();
    // A subscriber that never drains only fills its own channel.
    let _idle = collector.subscribe();
    let frame = Arc::new(Frame {
        seq: 1,
        events: Vec::new(),
    });
    for seq in 1..=1_000 {
        collector.broadcast(&Arc::new(Frame {
            seq,
            events: frame.events.clone(),
        }));
    }
    assert_eq!(collector.seq(), 0);
    assert!(collector.lagged.load(Ordering::Relaxed) > 0);
}

fn test_collector() -> Arc<Collector> {
    let recorder = crate::recorder::recorder(irrigation_runtime::observe::ObserveLevel::Nodes);
    let runtime = irrigation_runtime::Runtime::with_observer(
        irrigation_runtime::RuntimeConfig::default(),
        recorder.clone(),
    );
    Collector::new(runtime, recorder, 8)
}
