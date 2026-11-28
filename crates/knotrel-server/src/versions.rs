//! Two copies of the graph state, so read-only jobs never wait for a writer
//! (a "left-right" scheme; prototype of the 2026-11-28 performance
//! experiments).
//!
//! - Readers take a shared guard on the **current** copy with `try_read`.
//!   Writers only ever hold the other copy exclusively, so a reader that
//!   loads the current index sees an unlocked copy; in the narrow window
//!   where its index went stale it retries on the new current copy. A reader
//!   never parks on a writer, and one guard serves a whole read job, so all
//!   queries of a batch see one version.
//! - Writers serialize on a mutex. A write job runs on the **inactive** copy
//!   under its exclusive guard (waiting only for readers still on that older
//!   copy), is published by swapping the current index when the guard ends,
//!   and the other copy is then brought up to date: by a replay of the same
//!   job when the writer supplies one, else by a full clone. A reader
//!   therefore observes a write job entirely or not at all.
//! - Memory: two copies of the state. A reader that holds the old copy for a
//!   long time delays the next writer (which must update that copy), not
//!   other readers, and adds no third copy.

use parking_lot::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Brings the old copy (first argument) up to date with the published one
/// (second argument) after a write job.
pub(crate) type Replay<T> = Box<dyn FnOnce(&mut T, &T) + Send>;

/// The two copies, the index of the current one, and the writer mutex.
pub(crate) struct Versions<T> {
    copies: [RwLock<T>; 2],
    current: AtomicUsize,
    writer: Mutex<()>,
}

impl<T: Clone> Versions<T> {
    pub(crate) fn new(state: T) -> Self {
        let other = state.clone();
        Self {
            copies: [RwLock::new(state), RwLock::new(other)],
            current: AtomicUsize::new(0),
            writer: Mutex::new(()),
        }
    }

    /// A shared guard on the current version; never parks on a writer.
    pub(crate) fn read(&self) -> RwLockReadGuard<'_, T> {
        let mut spins = 0_u32;
        loop {
            let index = self.current.load(Ordering::Acquire);
            if let Some(guard) = self.copies[index].try_read() {
                return guard;
            }
            spins += 1;
            if spins < 64 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
    }

    /// Exclusive access to the next version; published when the guard drops.
    pub(crate) fn write(&self) -> VersionWrite<'_, T> {
        let writer = self.writer.lock();
        self.begin(writer)
    }

    /// Test shim for the single-lock observations of older tests: `None`
    /// while a writer runs, else a reader guard.
    #[cfg(test)]
    pub(crate) fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        if self.writer.is_locked() {
            None
        } else {
            Some(self.read())
        }
    }

    /// [`Self::write`] unless another writer is running.
    #[cfg(test)]
    pub(crate) fn try_write(&self) -> Option<VersionWrite<'_, T>> {
        self.writer.try_lock().map(|writer| self.begin(writer))
    }

    fn begin<'a>(&'a self, writer: MutexGuard<'a, ()>) -> VersionWrite<'a, T> {
        // The current index only changes under the writer mutex.
        let inactive = 1 - self.current.load(Ordering::Acquire);
        let guard = self.copies[inactive].write();
        VersionWrite {
            versions: self,
            inactive,
            guard: Some(guard),
            replay: None,
            _writer: writer,
        }
    }
}

/// A write job's exclusive guard on the next version.
pub(crate) struct VersionWrite<'a, T: Clone> {
    versions: &'a Versions<T>,
    inactive: usize,
    guard: Option<RwLockWriteGuard<'a, T>>,
    replay: Option<Replay<T>>,
    // Dropped after `Drop::drop` ran, so the next writer starts only once
    // both copies hold this job.
    _writer: MutexGuard<'a, ()>,
}

impl<T: Clone> VersionWrite<'_, T> {
    /// Brings the old copy up to date with `replay` instead of a full clone.
    pub(crate) fn replay_with(&mut self, replay: Replay<T>) {
        self.replay = Some(replay);
    }
}

impl<T: Clone> Deref for VersionWrite<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard.as_ref().expect("guard held until drop")
    }
}

impl<T: Clone> DerefMut for VersionWrite<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard.as_mut().expect("guard held until drop")
    }
}

impl<T: Clone> Drop for VersionWrite<'_, T> {
    fn drop(&mut self) {
        let versions = self.versions;
        // Publish before unlocking: a reader whose index went stale can read
        // the new copy only once later loads also return it, so no reader
        // ever sees a version and then an older one.
        versions.current.store(self.inactive, Ordering::Release);
        drop(self.guard.take());
        let published = versions.copies[self.inactive].read();
        // Waits for readers still on the previous version.
        let mut old = versions.copies[1 - self.inactive].write();
        match self.replay.take() {
            Some(replay) if !std::thread::panicking() => replay(&mut old, &published),
            _ => old.clone_from(&published),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    /// Writers alternate two-field batches (both set, both cleared), applied
    /// by replay on the second copy; readers read both fields under one
    /// guard. A reader must never see one field without the other, and a
    /// reader's versions never go backwards.
    #[test]
    fn readers_see_whole_write_jobs_and_one_version_per_guard() {
        #[derive(Clone, Default)]
        struct State {
            a: bool,
            b: bool,
            version: u64,
        }
        let versions = Arc::new(Versions::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let mut readers = Vec::new();
        for _ in 0..6 {
            let (versions, stop) = (Arc::clone(&versions), Arc::clone(&stop));
            readers.push(std::thread::spawn(move || {
                let (mut seen, mut last) = (0_u64, 0_u64);
                while !stop.load(Ordering::Relaxed) {
                    let state = versions.read();
                    assert_eq!(state.a, state.b, "torn write at version {}", state.version);
                    assert_eq!(state.a, state.version % 2 == 1, "fields match the version");
                    assert!(state.version >= last, "version went backwards");
                    last = state.version;
                    seen += 1;
                }
                seen
            }));
        }
        for round in 1..=20_000_u64 {
            let mut state = versions.write();
            let set = round % 2 == 1;
            state.a = set;
            state.b = set;
            state.version = round;
            if round % 3 != 0 {
                state.replay_with(Box::new(move |old: &mut State, _: &State| {
                    old.a = set;
                    old.b = set;
                    old.version = round;
                }));
            }
        }
        stop.store(true, Ordering::Relaxed);
        let total: u64 = readers.into_iter().map(|r| r.join().unwrap()).sum();
        assert!(total > 0);
        let state = versions.read();
        assert_eq!((state.a, state.b, state.version), (false, false, 20_000));
    }

    /// A reader holding the old version blocks no reader and does not delay
    /// publication: the writer publishes, then waits for that reader before
    /// updating the old copy and returning.
    #[test]
    fn a_held_old_version_delays_only_the_writer_return() {
        let versions = Arc::new(Versions::new(0_u64));
        *versions.write() = 1;
        let old = versions.read();
        assert_eq!(*old, 1);
        assert_eq!(*versions.read(), 1, "a second reader gets in");
        let writer = {
            let versions = Arc::clone(&versions);
            std::thread::spawn(move || {
                *versions.write() = 2;
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(*versions.read(), 2, "published while the old reader runs");
        assert_eq!(*old, 1, "the old reader keeps its version");
        assert!(
            !writer.is_finished(),
            "the writer waits to update the old copy"
        );
        drop(old);
        writer.join().unwrap();
        assert_eq!(*versions.read(), 2);
        *versions.write() = 3;
        assert_eq!(*versions.read(), 3, "both copies were brought up to date");
    }
}
