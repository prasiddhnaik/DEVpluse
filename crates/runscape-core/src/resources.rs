//! Bounded resource history (task T2.5).
//!
//! Holds the most recent CPU/memory samples per service. Capacity is a fixed
//! constant (`ARCHITECTURE.md` does not want second-by-second samples kept
//! forever — that is Milestone 5 storage's retention job). Everything here is
//! O(1) amortised and bounded by construction.

use std::collections::BTreeMap;
use std::collections::VecDeque;

use crate::ids::ServiceId;
use crate::model::{HostSample, ResourceSample};

/// Samples retained per service. At a 1 Hz sample rate this is 5 minutes of
/// history in memory — enough for a sparkline, small enough to never matter.
pub const SAMPLES_PER_SERVICE: usize = 300;

/// Ring buffer of [`ResourceSample`] per service, newest at the tail.
#[derive(Debug, Default)]
pub struct ResourceHistory {
    by_service: BTreeMap<ServiceId, VecDeque<ResourceSample>>,
}

impl ResourceHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push one sample. A sample older than the buffer's newest timestamp is
    /// rejected rather than allowed to reorder history.
    pub fn record(&mut self, service: &ServiceId, sample: ResourceSample) {
        let buffer = self.by_service.entry(service.clone()).or_default();
        if buffer.back().is_some_and(|last| last.at >= sample.at) {
            return;
        }
        buffer.push_back(sample);
        if buffer.len() > SAMPLES_PER_SERVICE {
            buffer.pop_front();
        }
    }

    /// A snapshot of a service's history, oldest first, for serialisation.
    pub fn history(&self, service: &ServiceId) -> Vec<ResourceSample> {
        self.by_service
            .get(service)
            .map(|buffer| buffer.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Drop all history for a service. Called when the registry evicts it.
    pub fn forget(&mut self, service: &ServiceId) {
        self.by_service.remove(service);
    }

    /// Non-empty service ids. Exposed so the server can reconcile in-memory
    /// history against the registry in one pass.
    pub fn services(&self) -> Vec<ServiceId> {
        self.by_service.keys().cloned().collect()
    }
}

/// Sum samples that share a tick timestamp.
///
/// The snapshot loop stamps every service in a tick with the same `at`. A
/// service that was not running that tick is absent, not zeroed — this does
/// not invent a reading.
pub fn sum_aligned(
    histories: impl IntoIterator<Item = impl IntoIterator<Item = ResourceSample>>,
) -> Vec<ResourceSample> {
    let mut by_tick: BTreeMap<std::time::SystemTime, ResourceSample> = BTreeMap::new();
    for history in histories {
        for sample in history {
            by_tick
                .entry(sample.at)
                .and_modify(|acc| {
                    acc.cpu_percent += sample.cpu_percent;
                    acc.memory_bytes = acc.memory_bytes.saturating_add(sample.memory_bytes);
                    acc.virtual_memory_bytes = acc
                        .virtual_memory_bytes
                        .saturating_add(sample.virtual_memory_bytes);
                    acc.thread_count = acc.thread_count.saturating_add(sample.thread_count);
                    acc.disk_read_bytes =
                        acc.disk_read_bytes.saturating_add(sample.disk_read_bytes);
                    acc.disk_write_bytes =
                        acc.disk_write_bytes.saturating_add(sample.disk_write_bytes);
                    acc.connection_count =
                        acc.connection_count.saturating_add(sample.connection_count);
                })
                .or_insert(sample);
        }
    }
    by_tick.into_values().collect()
}

/// Ring of host-wide samples, same capacity as a service sparkline.
#[derive(Debug, Default)]
pub struct HostHistory {
    samples: VecDeque<HostSample>,
}

impl HostHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, sample: HostSample) {
        if self.samples.back().is_some_and(|last| last.at >= sample.at) {
            return;
        }
        self.samples.push_back(sample);
        if self.samples.len() > SAMPLES_PER_SERVICE {
            self.samples.pop_front();
        }
    }

    pub fn history(&self) -> Vec<HostSample> {
        self.samples.iter().copied().collect()
    }

    pub fn latest(&self) -> Option<HostSample> {
        self.samples.back().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn sid() -> ServiceId {
        ServiceId::derived("svc")
    }

    fn sample(step: u64) -> ResourceSample {
        ResourceSample::cpu_and_memory(
            SystemTime::UNIX_EPOCH + Duration::from_secs(step),
            step as f32,
            step,
        )
    }

    #[test]
    fn never_holds_more_than_capacity() {
        let mut history = ResourceHistory::new();
        let id = sid();
        for step in 0..(SAMPLES_PER_SERVICE as u64 * 3) {
            history.record(&id, sample(step));
        }
        assert_eq!(history.history(&id).len(), SAMPLES_PER_SERVICE);
    }

    #[test]
    fn keeps_the_newest_samples() {
        let mut history = ResourceHistory::new();
        let id = sid();
        for step in 0..(SAMPLES_PER_SERVICE as u64 + 20) {
            history.record(&id, sample(step));
        }
        let history = history.history(&id);
        assert_eq!(history.first().unwrap().at, sample(20).at);
        assert_eq!(
            history.last().unwrap().at,
            sample(SAMPLES_PER_SERVICE as u64 + 19).at
        );
    }

    #[test]
    fn rejects_out_of_order_samples() {
        let mut history = ResourceHistory::new();
        let id = sid();
        history.record(&id, sample(10));
        history.record(&id, sample(5));
        history.record(&id, sample(11));
        assert_eq!(history.history(&id).len(), 2);
    }

    #[test]
    fn records_per_service_separately() {
        let mut history = ResourceHistory::new();
        history.record(&ServiceId::derived("a"), sample(1));
        history.record(&ServiceId::derived("b"), sample(1));
        assert_eq!(history.services().len(), 2);
        assert!(history.history(&ServiceId::derived("unseen")).is_empty());
    }

    #[test]
    fn forget_drops_one_service_only() {
        let mut history = ResourceHistory::new();
        let a = ServiceId::derived("a");
        let b = ServiceId::derived("b");
        history.record(&a, sample(1));
        history.record(&b, sample(1));
        history.forget(&a);
        assert!(history.history(&a).is_empty());
        assert_eq!(history.history(&b).len(), 1);
    }

    #[test]
    fn sum_aligned_adds_services_on_the_same_tick() {
        let a = ResourceSample::cpu_and_memory(sample(1).at, 1.5, 10);
        let b = ResourceSample::cpu_and_memory(sample(1).at, 2.5, 20);
        let later = ResourceSample::cpu_and_memory(sample(2).at, 3.0, 5);
        let summed = sum_aligned([vec![a, later], vec![b]]);
        assert_eq!(summed.len(), 2);
        assert_eq!(summed[0].cpu_percent, 4.0);
        assert_eq!(summed[0].memory_bytes, 30);
        assert_eq!(summed[1].cpu_percent, 3.0);
        assert_eq!(summed[1].memory_bytes, 5);
    }

    #[test]
    fn sum_aligned_does_not_invent_a_zero_for_a_missing_tick() {
        let only_second = ResourceSample::cpu_and_memory(sample(2).at, 1.0, 8);
        let summed = sum_aligned([vec![sample(1)], vec![only_second]]);
        assert_eq!(summed.len(), 2);
        assert_eq!(summed[0].memory_bytes, 1);
        assert_eq!(summed[1].memory_bytes, 8);
    }

    fn host(step: u64) -> HostSample {
        HostSample {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(step),
            load_avg_1: step as f64,
            load_avg_5: 0.5,
            load_avg_15: 0.25,
            process_count: 10 + step as u32,
        }
    }

    #[test]
    fn host_history_never_holds_more_than_capacity() {
        let mut history = HostHistory::new();
        for step in 0..(SAMPLES_PER_SERVICE as u64 * 2) {
            history.record(host(step));
        }
        assert_eq!(history.history().len(), SAMPLES_PER_SERVICE);
        assert_eq!(
            history.latest().map(|s| s.process_count),
            Some(10 + SAMPLES_PER_SERVICE as u32 * 2 - 1)
        );
    }

    #[test]
    fn host_history_rejects_out_of_order_samples() {
        let mut history = HostHistory::new();
        history.record(host(10));
        history.record(host(5));
        history.record(host(11));
        assert_eq!(history.history().len(), 2);
    }
}
