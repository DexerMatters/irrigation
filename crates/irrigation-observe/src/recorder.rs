//! The passive recorder: a bounded ring between the runtime and the page.
//!
//! The runtime calls [`Recorder::record`] on whichever thread produced the
//! event. That path must not allocate beyond the event itself, must not block,
//! and must never touch the runtime again. A full ring drops the event and
//! bumps a counter; the collector reports the drop instead of hiding it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_queue::ArrayQueue;
use irrigation_runtime::observe::{ObserveLevel, RuntimeEvent, RuntimeObserver};

/// Capacity of the recorder ring.
///
/// Sized for a burst of node events between collector passes; the collector
/// drains every few milliseconds.
const RING_CAPACITY: usize = 65_536;

/// A bounded event sink shared by the runtime and the collector.
#[derive(Debug)]
pub struct Recorder {
    level: ObserveLevel,
    ring: ArrayQueue<RuntimeEvent>,
    dropped: AtomicU64,
    recorded: AtomicU64,
}

impl Recorder {
    /// Builds a recorder at `level`.
    #[must_use]
    pub fn new(level: ObserveLevel) -> Self {
        Self {
            level,
            ring: ArrayQueue::new(RING_CAPACITY),
            dropped: AtomicU64::new(0),
            recorded: AtomicU64::new(0),
        }
    }

    /// The level this recorder asked the runtime for.
    #[must_use]
    pub fn level(&self) -> ObserveLevel {
        self.level
    }

    /// Events lost because the ring was full.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Events accepted since construction.
    #[must_use]
    pub fn recorded(&self) -> u64 {
        self.recorded.load(Ordering::Relaxed)
    }

    /// Removes up to `limit` events, oldest first.
    pub fn drain(&self, limit: usize, out: &mut Vec<RuntimeEvent>) {
        out.clear();
        for _ in 0..limit {
            match self.ring.pop() {
                Some(event) => out.push(event),
                None => break,
            }
        }
    }

    /// Whether anything is waiting to be drained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

impl RuntimeObserver for Recorder {
    fn level(&self) -> ObserveLevel {
        self.level
    }

    fn record(&self, event: RuntimeEvent) {
        self.recorded.fetch_add(1, Ordering::Relaxed);
        if self.ring.push(event).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Builds a recorder already wrapped for `Runtime::with_observer`.
#[must_use]
pub fn recorder(level: ObserveLevel) -> Arc<Recorder> {
    Arc::new(Recorder::new(level))
}

#[cfg(test)]
#[path = "../tests/unit/recorder/tests.rs"]
mod tests;
