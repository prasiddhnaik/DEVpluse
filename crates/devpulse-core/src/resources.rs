//! Bounded resource history (task T2.5).
//!
//! Holds the most recent CPU/memory samples per service. Capacity is a fixed
//! constant (`ARCHITECTURE.md` does not want second-by-second samples kept
//! forever — that is Milestone 5 storage's retention job). Everything here is
//! O(1) amortised and bounded by construction.

use std::collections::BTreeMap;
use std::collections::VecDeque;

use crate::ids::ServiceId;
use crate::model::ResourceSample;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn sid() -> ServiceId {
        ServiceId::derived("svc")
    }

    fn sample(step: u64) -> ResourceSample {
        ResourceSample {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(step),
            cpu_percent: step as f32,
            memory_bytes: step,
        }
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
}
