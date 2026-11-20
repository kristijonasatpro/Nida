//! Latest-frame ownership with a seek/reconfiguration generation barrier.
use std::sync::{
    atomic::{AtomicU64, Ordering},
    LockResult, Mutex, MutexGuard,
};

pub(crate) struct Mailbox<T> {
    frame: Mutex<Option<T>>,
    pub generation: AtomicU64,
    #[cfg(windows)]
    ready: std::sync::OnceLock<Option<super::command_channel::NativeWait>>,
}
impl<T> Default for Mailbox<T> {
    fn default() -> Self {
        Self {
            frame: Mutex::new(None),
            generation: AtomicU64::new(0),
            #[cfg(windows)]
            ready: std::sync::OnceLock::new(),
        }
    }
}
impl<T> Mailbox<T> {
    pub fn lock(&self) -> LockResult<MutexGuard<'_, Option<T>>> {
        self.frame.lock()
    }
    pub fn invalidate(&self) {
        if let Ok(mut frame) = self.frame.lock() {
            self.generation.fetch_add(1, Ordering::AcqRel);
            super::pacing_trace::emit(
                "invalidate",
                self.generation.load(Ordering::Acquire) as f64,
                f64::from(frame.is_some()),
            );
            *frame = None;
            self.notify_ready();
        }
    }
    /// Return displaced ownership for recycling, or reject stale work intact.
    pub fn publish(&self, frame: T, generation: u64) -> Result<Option<T>, T> {
        if let Ok(mut slot) = self.frame.lock() {
            if generation == self.generation.load(Ordering::Acquire) {
                let old = slot.replace(frame);
                super::pacing_trace::emit("publish", generation as f64, f64::from(old.is_some()));
                self.notify_ready();
                return Ok(old);
            }
        }
        Err(frame)
    }

    fn notify_ready(&self) {
        #[cfg(windows)]
        if let Some(Some(signal)) = self.ready.get() {
            signal.notify();
        }
    }

    /// Only an empty mailbox may wait. A parked CPU picture whose staging
    /// buffer is busy stays owned here. A seek cancels this generation's wait.
    pub fn take_until(
        &self,
        generation: u64,
        deadline: std::time::Instant,
        ready: impl FnOnce(&T) -> bool,
    ) -> Option<T> {
        let mut slot = self.frame.lock().ok()?;
        if generation != self.generation.load(Ordering::Acquire) {
            return None;
        }
        if slot.is_none() {
            if deadline <= std::time::Instant::now() {
                return None;
            }
            #[cfg(windows)]
            {
                // Initialized while holding frame: publication cannot slip
                // between the empty check and installing the notification.
                let signal = self
                    .ready
                    .get_or_init(super::command_channel::NativeWait::new)
                    .as_ref()?;
                loop {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    drop(slot);
                    let succeeded = signal.wait(left);
                    slot = self.frame.lock().ok()?;
                    if generation != self.generation.load(Ordering::Acquire) {
                        return None;
                    }
                    if slot.is_some() || !succeeded {
                        break;
                    }
                }
            }
        }
        if slot.as_ref().is_some_and(ready) {
            slot.take()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_preserves_parked_pictures_and_seek_generations() {
        let mailbox = Mailbox::default();
        mailbox.publish("parked", 0).unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        assert_eq!(mailbox.take_until(0, until, |_| false), None);
        assert_eq!(*mailbox.lock().unwrap(), Some("parked"));
        mailbox.invalidate();
        mailbox.publish("new seek", 1).unwrap();
        assert_eq!(mailbox.take_until(0, until, |_| true), None);
        assert_eq!(mailbox.take_until(1, until, |_| true), Some("new seek"));
    }

    #[cfg(windows)]
    #[test]
    fn expired_or_unavailable_native_wait_does_not_block() {
        let mailbox = Mailbox::<u32>::default();
        assert_eq!(
            mailbox.take_until(0, std::time::Instant::now(), |_| true),
            None
        );
        assert!(
            mailbox.ready.get().is_none(),
            "expired budget must not install a wait"
        );
        assert!(mailbox.ready.set(None).is_ok());
        assert_eq!(
            mailbox.take_until(
                0,
                std::time::Instant::now() + std::time::Duration::from_secs(10),
                |_| true
            ),
            None
        );
        mailbox.publish(42, 0).unwrap();
        assert_eq!(
            mailbox.take_until(0, std::time::Instant::now(), |_| true),
            Some(42)
        );
    }

    #[cfg(windows)]
    #[test]
    fn publication_and_seek_interrupt_the_native_readiness_wait() {
        for invalidated in [false, true] {
            let mailbox = std::sync::Arc::new(Mailbox::default());
            let consumer = mailbox.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                tx.send(consumer.take_until(
                    0,
                    std::time::Instant::now() + std::time::Duration::from_secs(10),
                    |_| true,
                ))
                .unwrap();
            });
            let until = std::time::Instant::now() + std::time::Duration::from_secs(1);
            while mailbox.ready.get().is_none() {
                assert!(
                    std::time::Instant::now() < until,
                    "consumer did not install its wait"
                );
                std::thread::yield_now();
            }
            assert!(
                mailbox.ready.get().unwrap().is_some(),
                "native wait required on this PC"
            );
            if invalidated {
                mailbox.invalidate();
            }
            mailbox.publish(42, u64::from(invalidated)).unwrap();
            assert_eq!(
                rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
                if invalidated { None } else { Some(42) }
            );
            worker.join().unwrap();
            if invalidated {
                assert_eq!(*mailbox.lock().unwrap(), Some(42));
            }
        }
    }
    #[test]
    fn seek_invalidates_queued_and_already_decoding_frames() {
        let mailbox = Mailbox::default();
        let old = mailbox.generation.load(Ordering::Acquire);
        assert_eq!(mailbox.publish(1, old), Ok(None));
        mailbox.invalidate();
        assert!(mailbox.lock().unwrap().is_none());
        assert_eq!(mailbox.publish(2, old), Err(2));
        assert!(mailbox.lock().unwrap().is_none());
        let new = mailbox.generation.load(Ordering::Acquire);
        assert_eq!(mailbox.publish(3, new), Ok(None));
        assert_eq!(*mailbox.lock().unwrap(), Some(3));
    }
    #[test]
    fn overwriting_and_invalidating_release_owned_payloads() {
        let mailbox = Mailbox::default();
        let picture = std::sync::Arc::new(());
        assert!(mailbox.publish(picture.clone(), 0).unwrap().is_none());
        let displaced = mailbox.publish(picture.clone(), 0).unwrap().unwrap();
        assert_eq!(std::sync::Arc::strong_count(&picture), 3);
        drop(displaced);
        mailbox.invalidate();
        assert_eq!(std::sync::Arc::strong_count(&picture), 1);
    }
    #[test]
    fn in_flight_worker_cannot_overwrite_the_new_seek_picture() {
        let mailbox = std::sync::Arc::new(Mailbox::default());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let worker_barrier = barrier.clone();
        let producer = mailbox.clone();
        let worker = std::thread::spawn(move || {
            let generation = producer.generation.load(Ordering::Acquire);
            worker_barrier.wait();
            worker_barrier.wait();
            assert_eq!(producer.publish("stale", generation), Err("stale"));
        });
        barrier.wait();
        mailbox.invalidate();
        let current = mailbox.generation.load(Ordering::Acquire);
        assert_eq!(mailbox.publish("requested", current), Ok(None));
        barrier.wait();
        worker.join().unwrap();
        assert_eq!(*mailbox.lock().unwrap(), Some("requested"));
    }
}
