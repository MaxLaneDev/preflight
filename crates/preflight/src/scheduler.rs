//! Atomic, FIFO admission for declared resources, not OS process limits.
use anyhow::{Result, ensure};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;

pub struct Scheduler {
    state: Mutex<State>,
    changed: Notify,
}
struct State {
    capacity: usize,
    budget: Option<u64>,
    used_slots: usize,
    used_memory: u64,
    queue: VecDeque<Arc<()>>,
}
pub struct Permit {
    scheduler: Arc<Scheduler>,
    slots: usize,
    memory: u64,
}
struct Waiting {
    scheduler: Arc<Scheduler>,
    ticket: Arc<()>,
}
impl Drop for Waiting {
    fn drop(&mut self) {
        self.scheduler
            .state
            .lock()
            .expect("scheduler lock")
            .queue
            .retain(|t| !Arc::ptr_eq(t, &self.ticket));
        self.scheduler.changed.notify_waiters();
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = self.scheduler.state.lock().expect("scheduler lock");
        state.used_slots -= self.slots;
        state.used_memory -= self.memory;
        drop(state);
        self.scheduler.changed.notify_waiters();
    }
}
impl Scheduler {
    pub fn new(capacity: usize, memory: Option<u64>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                capacity,
                budget: memory,
                used_slots: 0,
                used_memory: 0,
                queue: VecDeque::new(),
            }),
            changed: Notify::new(),
        })
    }
    pub async fn acquire(self: &Arc<Self>, slots: usize, memory: u64) -> Result<Permit> {
        self.reserve(slots, memory, false).await
    }
    /// Already admitted runs keep their resource requests across a configuration change.
    pub(crate) async fn acquire_admitted(
        self: &Arc<Self>,
        slots: usize,
        memory: u64,
    ) -> Result<Permit> {
        self.reserve(slots, memory, true).await
    }
    pub fn reconfigure(&self, capacity: usize, memory: Option<u64>) {
        let mut state = self.state.lock().expect("scheduler lock");
        state.capacity = capacity;
        state.budget = memory;
        drop(state);
        self.changed.notify_waiters();
    }
    async fn reserve(
        self: &Arc<Self>,
        slots: usize,
        memory: u64,
        admitted: bool,
    ) -> Result<Permit> {
        let waiting = Waiting {
            scheduler: self.clone(),
            ticket: Arc::new(()),
        };
        {
            let mut state = self.state.lock().expect("scheduler lock");
            ensure!(
                slots > 0 && (admitted || slots <= state.capacity),
                "impossible slot reservation"
            );
            ensure!(
                admitted || memory <= state.budget.unwrap_or(0),
                "impossible memory reservation"
            );
            state.queue.push_back(waiting.ticket.clone());
        }
        loop {
            // Register before inspecting capacity so release cannot race with notification.
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let acquired = {
                let mut state = self.state.lock().expect("scheduler lock");
                if state
                    .queue
                    .front()
                    .is_some_and(|t| Arc::ptr_eq(t, &waiting.ticket))
                    && (state.used_slots.saturating_add(slots) <= state.capacity
                        || (admitted && state.used_slots == 0))
                    && state.used_memory.checked_add(memory).is_some()
                    && state.budget.is_none_or(|budget| {
                        state.used_memory.saturating_add(memory) <= budget
                            || (admitted && state.used_slots == 0)
                    })
                {
                    state.queue.pop_front();
                    state.used_slots += slots;
                    state.used_memory += memory;
                    true
                } else {
                    false
                }
            };
            if acquired {
                drop(waiting);
                return Ok(Permit {
                    scheduler: self.clone(),
                    slots,
                    memory,
                });
            }
            notified.await;
        }
    }
    pub fn status(&self) -> serde_json::Value {
        let state = self.state.lock().expect("scheduler lock");
        serde_json::json!({"capacity_slots":state.capacity,"available_slots":state.capacity.saturating_sub(state.used_slots),"reserved_slots":state.used_slots,"memory_budget_mib":state.budget,"available_memory_mib":state.budget.map(|m|m.saturating_sub(state.used_memory)),"reserved_memory_mib":state.budget.map(|_|state.used_memory),"draining":state.used_slots>state.capacity||state.budget.is_some_and(|m|state.used_memory>m),"waiting_reservations":state.queue.len(),"scope":"server","reservation":"atomic FIFO slots and declared memory; snapshot preparation reserves one slot and default memory","enforcement":"admission only; no operating-system CPU or memory limit","stage_limit":32,"default_stage_slots":1})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn reload_keeps_permits_and_grandfathers_queued_work_without_underflow() {
        let scheduler = Scheduler::new(2, Some(100));
        let held = scheduler.acquire(2, 100).await.unwrap();
        let mut old = Box::pin(scheduler.acquire_admitted(2, 100));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut old)
                .await
                .is_err()
        );
        scheduler.reconfigure(1, Some(50));
        assert_eq!(scheduler.status()["reserved_slots"], 2);
        assert_eq!(scheduler.status()["available_slots"], 0);
        assert_eq!(scheduler.status()["draining"], true);
        assert!(scheduler.acquire(2, 100).await.is_err());
        drop(held);
        let old = tokio::time::timeout(std::time::Duration::from_secs(1), old)
            .await
            .unwrap()
            .unwrap();
        let mut next = Box::pin(scheduler.acquire(1, 20));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut next)
                .await
                .is_err()
        );
        scheduler.reconfigure(3, None);
        let next = tokio::time::timeout(std::time::Duration::from_secs(1), next)
            .await
            .unwrap()
            .unwrap();
        drop(old);
        scheduler.reconfigure(2, Some(10));
        assert_eq!(scheduler.status()["reserved_memory_mib"], 20);
        assert_eq!(scheduler.status()["available_memory_mib"], 0);
        drop(next);
        assert_eq!(scheduler.status()["available_slots"], 2);
        assert_eq!(scheduler.status()["available_memory_mib"], 10);
    }
    #[tokio::test]
    async fn multi_resource_wait_cancel_and_release_are_atomic() {
        let scheduler = Scheduler::new(3, Some(100));
        let first = scheduler.acquire(1, 80).await.unwrap();
        let mut waiting = Box::pin(scheduler.acquire(2, 30));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        assert_eq!(scheduler.status()["available_slots"], 2);
        assert_eq!(scheduler.status()["available_memory_mib"], 20);
        let mut behind = Box::pin(scheduler.acquire(1, 10));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut behind)
                .await
                .is_err()
        );
        drop(waiting);
        let second = tokio::time::timeout(std::time::Duration::from_secs(1), behind)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(scheduler.status()["reserved_memory_mib"], 90);
        drop(first);
        drop(second);
        assert_eq!(scheduler.status()["available_slots"], 3);
        assert_eq!(scheduler.status()["available_memory_mib"], 100);
        assert_eq!(scheduler.status()["waiting_reservations"], 0);
        assert!(scheduler.acquire(4, 0).await.is_err());
        assert!(scheduler.acquire(1, 101).await.is_err());
    }
    #[tokio::test]
    async fn releasing_capacity_wakes_all_eligible_fifo_waiters() {
        let scheduler = Scheduler::new(2, Some(100));
        let held = scheduler.acquire(2, 100).await.unwrap();
        let mut first = Box::pin(scheduler.acquire(1, 50));
        let mut second = Box::pin(scheduler.acquire(1, 50));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut first)
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut second)
                .await
                .is_err()
        );
        drop(held);
        let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(first, second)
        })
        .await
        .unwrap();
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_eq!(scheduler.status()["reserved_slots"], 2);
        drop(a);
        drop(b);
        assert_eq!(scheduler.status()["available_memory_mib"], 100);
    }
}
