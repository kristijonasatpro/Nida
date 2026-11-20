//! Bounded GPU ownership, tested without creating a GPU device.
use super::delivery::DeliveryCounters;
use anyhow::Result;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub(crate) const CAPACITY: usize = 3;

struct Slot<T> {
    resource: T,
    busy: AtomicBool,
}

pub(crate) struct Lease<T> {
    slot: Arc<Slot<T>>,
    counters: Arc<DeliveryCounters>,
}
impl<T> Lease<T> {
    pub fn resource(&self) -> &T {
        &self.slot.resource
    }
}
impl<T> Drop for Lease<T> {
    fn drop(&mut self) {
        self.counters.occupied_slots.fetch_sub(1, Ordering::Relaxed);
        self.slot.busy.store(false, Ordering::Release);
    }
}

pub(crate) struct Pool<T> {
    key: Option<(u32, u32, bool)>,
    slots: Vec<Arc<Slot<T>>>,
    counters: Arc<DeliveryCounters>,
}
impl<T> Pool<T> {
    pub fn new(counters: Arc<DeliveryCounters>) -> Self {
        Self {
            key: None,
            slots: Vec::new(),
            counters,
        }
    }
    pub fn acquire(
        &mut self,
        key: (u32, u32, bool),
        make: impl FnOnce() -> Result<T>,
    ) -> Result<Option<Lease<T>>> {
        if self.key != Some(key) {
            if self.slots.iter().any(|s| s.busy.load(Ordering::Acquire)) {
                return Ok(None);
            }
            self.slots.clear();
            self.key = Some(key);
        }
        let slot = match self
            .slots
            .iter()
            .find(|s| !s.busy.load(Ordering::Acquire))
            .cloned()
        {
            Some(slot) => slot,
            None if self.slots.len() < CAPACITY => {
                let slot = Arc::new(Slot {
                    resource: make()?,
                    busy: AtomicBool::new(false),
                });
                self.slots.push(slot.clone());
                slot
            }
            None => return Ok(None),
        };
        slot.busy.store(true, Ordering::Release);
        let occupied = self.counters.occupied_slots.fetch_add(1, Ordering::Relaxed) + 1;
        self.counters
            .peak_slots
            .fetch_max(occupied, Ordering::Relaxed);
        Ok(Some(Lease {
            slot,
            counters: self.counters.clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn three_in_flight_leases_apply_backpressure_and_reuse_a_released_slot() {
        let counters = Arc::new(DeliveryCounters::default());
        let mut pool = Pool::new(counters.clone());
        let key = (1920, 1080, false);
        let a = pool.acquire(key, || Ok(1)).unwrap().unwrap();
        let b = pool.acquire(key, || Ok(2)).unwrap().unwrap();
        let c = pool.acquire(key, || Ok(3)).unwrap().unwrap();
        assert!(pool
            .acquire(key, || panic!("must not allocate slot four"))
            .unwrap()
            .is_none());
        drop(b);
        let again = pool
            .acquire(key, || panic!("must reuse slot two"))
            .unwrap()
            .unwrap();
        assert_eq!(*again.resource(), 2);
        drop((a, c, again));
        assert_eq!(counters.snapshot().occupied_slots, 0);
        assert_eq!(counters.snapshot().peak_slots, 3);
    }
    #[test]
    fn format_changes_wait_for_old_leases_and_pool_drop_does_not_destroy_in_flight_resources() {
        let counters = Arc::new(DeliveryCounters::default());
        let resource = Arc::new(());
        let mut pool = Pool::new(counters.clone());
        let old = pool
            .acquire((64, 64, false), || Ok(resource.clone()))
            .unwrap()
            .unwrap();
        assert!(pool
            .acquire((128, 64, true), || panic!("old GPU work is alive"))
            .unwrap()
            .is_none());
        drop(old);
        let new = pool
            .acquire((128, 64, true), || Ok(resource.clone()))
            .unwrap()
            .unwrap();
        drop(pool);
        assert_eq!(Arc::strong_count(&resource), 2);
        drop(new);
        assert_eq!(Arc::strong_count(&resource), 1);
        assert_eq!(counters.snapshot().occupied_slots, 0);
    }
    #[test]
    fn allocation_errors_do_not_leak_a_slot() {
        let counters = Arc::new(DeliveryCounters::default());
        let mut pool: Pool<()> = Pool::new(counters.clone());
        assert!(pool
            .acquire((1, 1, false), || anyhow::bail!("allocation failed"))
            .is_err());
        assert_eq!(counters.snapshot().occupied_slots, 0);
        assert!(pool.acquire((1, 1, false), || Ok(())).unwrap().is_some());
    }
}
