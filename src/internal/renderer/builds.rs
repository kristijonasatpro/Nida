//! Lifetime tracking for background GPU/shader construction.
//!
//! A ticket is reserved before handing work to another thread. Closing rejects
//! new tickets and prevents queued tickets from starting. Active tickets keep
//! the owner waiting until all compiler calls (including unwinding) have ended.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

#[derive(Default)]
struct State {
    closed: bool,
    pending: usize,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    finished: Condvar,
}

/// Shared by every clone of one GPU context; independent applications/tests
/// never close each other's build admission.
#[derive(Clone, Default)]
pub struct Builds(Arc<Shared>);

/// Owned by queued/running work, released even when the work panics.
pub struct Ticket {
    shared: Arc<Shared>,
}

impl Builds {
    /// Reserve before spawning/enqueuing, so shutdown cannot miss a worker
    /// that has not yet been scheduled by the OS.
    pub fn reserve(&self) -> Option<Ticket> {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return None;
        }
        state.pending += 1;
        Some(Ticket {
            shared: Arc::clone(&self.0),
        })
    }

    pub fn close(&self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
    }

    pub fn pending(&self) -> usize {
        self.0
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending
    }

    /// Used after the OS has ended the event loop. Normal Quit polls instead,
    /// keeping the window responsive. A timeout never authorizes unsafe exit.
    pub fn wait(&self, timeout: Duration) -> bool {
        let state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (state, _) = self
            .0
            .finished
            .wait_timeout_while(state, timeout, |s| s.pending != 0)
            .unwrap_or_else(PoisonError::into_inner);
        state.pending == 0
    }
}

impl Ticket {
    /// Called immediately before work. A ticket still waiting when admission
    /// closed is cancelled. Once admitted, it must finish before teardown.
    pub fn start(&self) -> bool {
        !self
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.pending -= 1;
        self.shared.finished.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_cancels_queued_work_and_refuses_new_work() {
        let builds = Builds::default();
        let queued = builds.reserve().unwrap();
        builds.close();
        builds.close();
        assert!(!queued.start());
        assert!(builds.reserve().is_none());
        assert!(!builds.wait(Duration::ZERO));
        drop(queued);
        assert!(builds.wait(Duration::ZERO));
    }

    #[test]
    fn active_work_keeps_shutdown_waiting_and_unwinding_releases_it() {
        let builds = Builds::default();
        let ticket = builds.reserve().unwrap();
        assert!(ticket.start());
        builds.close();
        assert_eq!(builds.pending(), 1);
        let _ = std::panic::catch_unwind(move || {
            let _ticket = ticket;
            panic!("compiler failure");
        });
        assert!(builds.wait(Duration::ZERO));
    }

    #[test]
    fn completion_wakes_a_waiter_and_instances_are_independent() {
        let builds = Builds::default();
        let ticket = builds.reserve().unwrap();
        let other = Builds::default();
        builds.close();
        assert!(other.reserve().is_some());
        let waiter = builds.clone();
        let worker = std::thread::spawn(move || waiter.wait(Duration::from_secs(5)));
        drop(ticket);
        assert!(worker.join().unwrap());
    }
}
