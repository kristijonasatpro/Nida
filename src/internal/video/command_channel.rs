//! Interruptible deadline waits without changing process-wide timer resolution.
use std::sync::mpsc;
use std::time::Duration;
#[cfg(any(windows, test))]
use std::time::Instant;

pub(super) struct Sender<T> {
    channel: Option<mpsc::Sender<T>>,
    #[cfg(windows)]
    signal: Option<std::sync::Arc<NativeWait>>,
}

pub(super) struct Receiver<T> {
    channel: mpsc::Receiver<T>,
    #[cfg(windows)]
    signal: Option<std::sync::Arc<NativeWait>>,
}

pub(super) fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = mpsc::channel();
    #[cfg(windows)]
    let signal = NativeWait::new().map(std::sync::Arc::new);
    (
        Sender {
            channel: Some(tx),
            #[cfg(windows)]
            signal: signal.clone(),
        },
        Receiver {
            channel: rx,
            #[cfg(windows)]
            signal,
        },
    )
}

impl<T> Sender<T> {
    pub(super) fn send(&self, value: T) -> Result<(), mpsc::SendError<T>> {
        let result = self.channel.as_ref().expect("live sender").send(value);
        #[cfg(windows)]
        if let Some(signal) = &self.signal {
            signal.notify();
        }
        result
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        // Publish disconnection before waking the receiver.
        drop(self.channel.take());
        #[cfg(windows)]
        if let Some(signal) = &self.signal {
            signal.notify();
        }
    }
}

impl<T> Receiver<T> {
    pub(super) fn try_recv(&self) -> Result<T, mpsc::TryRecvError> {
        self.channel.try_recv()
    }

    pub(super) fn recv_timeout(&self, duration: Duration) -> Result<T, mpsc::RecvTimeoutError> {
        #[cfg(not(windows))]
        {
            self.channel.recv_timeout(duration)
        }
        #[cfg(windows)]
        {
            let deadline = Instant::now() + duration;
            loop {
                match self.channel.try_recv() {
                    Ok(value) => return Ok(value),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(mpsc::RecvTimeoutError::Disconnected);
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(mpsc::RecvTimeoutError::Timeout);
                }
                #[cfg(windows)]
                if let Some(signal) = &self.signal {
                    if signal.wait(left) {
                        continue;
                    }
                }
                return self
                    .channel
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()));
            }
        }
    }
}

#[cfg(windows)]
pub(crate) struct NativeWait {
    event: std::os::windows::io::OwnedHandle,
    timer: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
impl NativeWait {
    pub(crate) fn new() -> Option<Self> {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::Threading::{
            CreateEventW, CreateWaitableTimerExW, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
            TIMER_ALL_ACCESS,
        };
        fn own(raw: windows_sys::Win32::Foundation::HANDLE) -> Option<OwnedHandle> {
            if raw.is_null() {
                None
            } else {
                // SAFETY: successful creation transfers this unique handle to us.
                Some(unsafe { OwnedHandle::from_raw_handle(raw) })
            }
        }
        #[cfg(test)]
        if std::env::var("NIDA_VIDEO_BENCH_LEGACY_WAIT").is_ok_and(|value| value == "1") {
            return None;
        }
        // Auto-reset event: no drain/reset race with a concurrent command sender.
        // SAFETY: unnamed, non-inherited handles, no caller-owned pointers.
        let event = own(unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) })?;
        let timer = own(unsafe {
            CreateWaitableTimerExW(
                std::ptr::null(),
                std::ptr::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS,
            )
        })?;
        Some(Self { event, timer })
    }

    pub(crate) fn notify(&self) {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: the shared owned event stays alive through this call.
        unsafe {
            windows_sys::Win32::System::Threading::SetEvent(self.event.as_raw_handle());
        }
    }

    /// One waiting thread; other threads may call `notify` concurrently.
    pub(crate) fn wait(&self, left: Duration) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Threading::{
            SetWaitableTimer, WaitForMultipleObjects, INFINITE,
        };
        // Negative means relative time, in 100 ns units; round up to avoid early expiry.
        let due = -i64::try_from(left.as_nanos().div_ceil(100)).unwrap_or(i64::MAX);
        let handles = [self.event.as_raw_handle(), self.timer.as_raw_handle()];
        // SAFETY: handles remain owned for the whole wait; due and array are valid.
        unsafe {
            if SetWaitableTimer(handles[1], &raw const due, 0, None, std::ptr::null(), 0) == 0 {
                return false;
            }
            // Both success cases recheck channel and monotonic deadline above.
            WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) <= 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_commands_are_ordered_and_disconnect_is_observed() {
        let (tx, rx) = channel();
        #[cfg(windows)]
        assert!(
            rx.signal.is_some(),
            "this Windows test must exercise the native wait"
        );
        for value in 0..100 {
            tx.send(value).unwrap();
        }
        for value in 0..100 {
            assert_eq!(rx.recv_timeout(Duration::ZERO), Ok(value));
        }
        drop(tx);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }

    #[test]
    fn command_interrupts_a_long_wait_and_timeout_never_returns_early() {
        let (tx, rx) = channel();
        let thread = std::thread::spawn(move || {
            let started = Instant::now();
            assert_eq!(
                rx.recv_timeout(Duration::from_millis(2)),
                Err(mpsc::RecvTimeoutError::Timeout)
            );
            assert!(started.elapsed() >= Duration::from_millis(2));
            rx.recv_timeout(Duration::from_secs(10))
        });
        std::thread::sleep(Duration::from_millis(20));
        tx.send(42).unwrap();
        assert_eq!(thread.join().unwrap(), Ok(42));
    }

    #[test]
    fn dropping_sender_wakes_a_waiting_receiver() {
        let (tx, rx) = channel::<()>();
        let thread = std::thread::spawn(move || rx.recv_timeout(Duration::from_secs(10)));
        std::thread::sleep(Duration::from_millis(10));
        drop(tx);
        assert_eq!(
            thread.join().unwrap(),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }

    #[cfg(windows)]
    #[test]
    fn fallback_keeps_command_and_timeout_semantics() {
        let (tx, mut rx) = channel();
        rx.signal = None;
        tx.send(7).unwrap();
        assert_eq!(rx.recv_timeout(Duration::ZERO), Ok(7));
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(1)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        drop(tx);
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected));
    }
}
