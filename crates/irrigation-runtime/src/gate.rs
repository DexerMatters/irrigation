//! Readers-writer gate with permits that outlive a stack frame.
//!
//! A demand holds a read permit for its whole task, including while that task
//! is suspended waiting for another task. An edit therefore cannot publish a
//! new revision in the middle of a demand, which is what makes a demand observe
//! exactly one revision.
//!
//! [`std::sync::RwLock`] cannot express that: its read guard borrows the lock
//! and cannot be moved to another thread, yet a suspended task resumes on
//! whichever worker picks it up. Permits here are counters, so acquisition and
//! release do not have to happen on the same thread.

use parking_lot::{Condvar, Mutex, MutexGuard};

/// Gate bookkeeping.
#[derive(Debug, Default)]
struct GateState {
    readers: usize,
    writer: bool,
    writers_waiting: usize,
}

/// Protects the revision protocol: many demands or one edit.
#[derive(Debug, Default)]
pub struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

impl Gate {
    /// Creates an open gate.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state.lock()
    }

    /// Acquires a read permit, waiting while an edit is in progress.
    ///
    /// Waiting writers take priority so a stream of demands cannot starve an
    /// edit indefinitely.
    pub fn enter_read(&self) {
        let mut state = self.lock();
        while state.writer || state.writers_waiting > 0 {
            self.changed.wait(&mut state);
        }
        state.readers += 1;
    }

    /// Releases a read permit acquired by [`Gate::enter_read`].
    pub fn exit_read(&self) {
        let mut state = self.lock();
        debug_assert!(state.readers > 0, "unbalanced read permit");
        state.readers = state.readers.saturating_sub(1);
        if state.readers == 0 {
            self.changed.notify_all();
        }
    }

    /// Acquires a read permit for the duration of the returned guard.
    pub fn read(&self) -> ReadGuard<'_> {
        self.enter_read();
        ReadGuard { gate: self }
    }

    /// Acquires the write side, waiting for every demand to finish.
    pub fn write(&self) -> WriteGuard<'_> {
        let mut state = self.lock();
        state.writers_waiting += 1;
        while state.writer || state.readers > 0 {
            self.changed.wait(&mut state);
        }
        state.writers_waiting -= 1;
        state.writer = true;
        WriteGuard { gate: self }
    }

    fn exit_write(&self) {
        let mut state = self.lock();
        state.writer = false;
        self.changed.notify_all();
    }
}

/// A read permit released when the guard drops.
#[derive(Debug)]
pub struct ReadGuard<'a> {
    gate: &'a Gate,
}

impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        self.gate.exit_read();
    }
}

/// The exclusive side of the gate.
#[derive(Debug)]
pub struct WriteGuard<'a> {
    gate: &'a Gate,
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        self.gate.exit_write();
    }
}

#[cfg(test)]
mod tests {
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
}
