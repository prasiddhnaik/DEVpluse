//! Container collection over the Docker API (tasks T6.2 and T6.4).

use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use bollard::Docker;
use bollard::models::ContainerStatsResponse;
use bollard::query_parameters::{ListContainersOptionsBuilder, StatsOptionsBuilder};
use futures_util::StreamExt;
use futures_util::stream;

use crate::container::{ContainerSnapshot, ContainerState, ObservedContainer};
use crate::error::DockerError;

/// How many stats requests may be in flight at once.
///
/// Each request occupies the daemon for about a second (see
/// [`BollardCollector::with_stats`]), so they have to overlap to be usable on a
/// machine with a dozen containers — but the fan-out has to stay bounded, or
/// Runscape becomes the resource problem (`AGENTS.md` rule 7).
const STATS_CONCURRENCY: usize = 8;

/// Platform-independent container collection, mirroring the process and socket
/// collectors so the daemon polls everything the same way.
#[async_trait]
pub trait ContainerCollector: Send + Sync {
    async fn snapshot(&self) -> Result<ContainerSnapshot, DockerError>;
}

/// Bollard-backed collector.
///
/// Obtained from [`DockerAvailability::detect`](crate::DockerAvailability::detect),
/// which is the only way to get one: a collector exists only if a daemon
/// answered a ping.
#[derive(Debug, Clone)]
pub struct BollardCollector {
    docker: Docker,
    stats: bool,
}

impl BollardCollector {
    pub(crate) fn new(docker: Docker) -> Self {
        Self {
            docker,
            stats: false,
        }
    }

    /// Turn per-container CPU/memory sampling on or off. Off by default.
    ///
    /// # Cost
    ///
    /// This is the expensive part of Docker inspection, which is why it is
    /// opt-in:
    ///
    /// * one extra HTTP request per *running* container, per snapshot;
    /// * the daemon computes CPU usage from two samples taken about a second
    ///   apart, and the request does not return until it has both. A snapshot
    ///   with stats enabled therefore takes at least ~1s regardless of how fast
    ///   the machine is. Requests overlap up to [`STATS_CONCURRENCY`], so the
    ///   cost is ~1s per batch of eight containers, not per container.
    ///
    /// Runscape deliberately does not use Docker's `one-shot` flag: it skips
    /// the second sample, which makes the CPU percentage uncomputable, and a
    /// fabricated percentage is worse than none.
    ///
    /// While this is off,
    /// [`cpu_percent`](ObservedContainer::cpu_percent) and
    /// [`memory_bytes`](ObservedContainer::memory_bytes) are `None`. That is
    /// the correct reading of "not measured".
    pub fn with_stats(mut self, enabled: bool) -> Self {
        self.stats = enabled;
        self
    }

    /// Whether stats sampling is enabled.
    pub fn stats_enabled(&self) -> bool {
        self.stats
    }

    /// Sample CPU and memory for every running container, in bounded parallel.
    ///
    /// A failure to sample one container is logged and leaves its fields
    /// `None`; it never fails the snapshot, because a container that exits
    /// between the list and the stats call is normal.
    async fn fill_stats(&self, containers: &mut [ObservedContainer]) {
        // Collected up front, and owning its ids, so that no borrow of
        // `containers` is held across the awaits below.
        let targets: Vec<(usize, String)> = containers
            .iter()
            .enumerate()
            // A container that is not running has no CPU to measure, and asking
            // costs a round-trip.
            .filter(|(_, container)| container.state == ContainerState::Running)
            .map(|(index, container)| (index, container.id.clone()))
            .collect();

        let samples: Vec<(usize, StatsSample)> = stream::iter(targets)
            .map(|(index, id)| async move { (index, self.sample(&id).await) })
            .buffer_unordered(STATS_CONCURRENCY)
            .collect()
            .await;

        for (index, sample) in samples {
            if let Some(container) = containers.get_mut(index) {
                container.cpu_percent = sample.cpu_percent;
                container.memory_bytes = sample.memory_bytes;
            }
        }
    }

    async fn sample(&self, id: &str) -> StatsSample {
        // `stream(false)` still returns a stream of exactly one element.
        let options = StatsOptionsBuilder::new().stream(false).build();
        let mut stats = std::pin::pin!(self.docker.stats(id, Some(options)));

        match stats.next().await {
            Some(Ok(response)) => StatsSample::from_response(&response),
            Some(Err(error)) => {
                tracing::debug!(container = %id, %error, "docker stats unavailable");
                StatsSample::UNMEASURED
            }
            None => {
                tracing::debug!(container = %id, "docker returned no stats sample");
                StatsSample::UNMEASURED
            }
        }
    }
}

#[async_trait]
impl ContainerCollector for BollardCollector {
    async fn snapshot(&self) -> Result<ContainerSnapshot, DockerError> {
        let started = Instant::now();
        let captured_at = SystemTime::now();

        // `all(true)`: a stopped container is exactly what a developer wants to
        // see, so the list must not be limited to running ones.
        let options = ListContainersOptionsBuilder::new().all(true).build();
        let summaries = self.docker.list_containers(Some(options)).await?;

        let mut containers: Vec<_> = summaries
            .iter()
            .map(ObservedContainer::from_summary)
            .collect();
        if self.stats {
            self.fill_stats(&mut containers).await;
        }
        containers.sort_unstable_by(|a, b| a.identity.name.cmp(&b.identity.name));

        let snapshot = ContainerSnapshot {
            captured_at,
            duration: started.elapsed(),
            containers,
        };

        tracing::debug!(
            containers = snapshot.containers.len(),
            stats = self.stats,
            duration_us = snapshot.duration.as_micros(),
            "container snapshot"
        );

        Ok(snapshot)
    }
}

/// One resource measurement, or the absence of one.
#[derive(Debug, Clone, Copy, PartialEq)]
struct StatsSample {
    cpu_percent: Option<f32>,
    memory_bytes: Option<u64>,
}

impl StatsSample {
    const UNMEASURED: Self = Self {
        cpu_percent: None,
        memory_bytes: None,
    };

    fn from_response(response: &ContainerStatsResponse) -> Self {
        Self {
            cpu_percent: cpu_percent(response),
            memory_bytes: memory_bytes(response),
        }
    }
}

/// CPU usage as a percentage of one core, the same arithmetic `docker stats`
/// does: the container's CPU-time delta over the machine's CPU-time delta,
/// scaled by the number of online cores.
///
/// Every input is optional on the wire and several are Linux-only, so anything
/// missing yields `None` instead of a partially computed number. A zero system
/// delta also yields `None`: the two samples covered no time, so there is no
/// rate to report.
fn cpu_percent(response: &ContainerStatsResponse) -> Option<f32> {
    let current = response.cpu_stats.as_ref()?;
    let previous = response.precpu_stats.as_ref()?;

    let usage = current.cpu_usage.as_ref()?;
    let cpu_delta = usage
        .total_usage?
        .checked_sub(previous.cpu_usage.as_ref()?.total_usage?)?;
    let system_delta = current
        .system_cpu_usage?
        .checked_sub(previous.system_cpu_usage?)?;
    if system_delta == 0 {
        return None;
    }

    let cores = current
        .online_cpus
        .or_else(|| {
            usage
                .percpu_usage
                .as_ref()
                .map(|per_cpu| u32::try_from(per_cpu.len()).unwrap_or(u32::MAX))
        })
        .filter(|cores| *cores > 0)?;

    let percent = cpu_delta as f64 / system_delta as f64 * f64::from(cores) * 100.0;
    Some(percent as f32)
}

/// Resident memory, excluding the page cache.
///
/// `docker stats` subtracts the inactive file cache so the number matches what
/// the developer sees in the CLI; the cgroup v2 key is `inactive_file`, v1
/// reports `total_inactive_file` and `cache`.
fn memory_bytes(response: &ContainerStatsResponse) -> Option<u64> {
    let memory = response.memory_stats.as_ref()?;
    let usage = memory.usage?;

    let cache = memory
        .stats
        .as_ref()
        .and_then(|stats| {
            ["inactive_file", "total_inactive_file", "cache"]
                .iter()
                .find_map(|key| stats.get(*key))
        })
        .copied()
        .unwrap_or(0);

    Some(usage.saturating_sub(cache))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ContainerCpuStats, ContainerCpuUsage, ContainerMemoryStats};

    fn cpu(total: u64, system: u64, cores: Option<u32>) -> ContainerCpuStats {
        ContainerCpuStats {
            cpu_usage: Some(ContainerCpuUsage {
                total_usage: Some(total),
                ..Default::default()
            }),
            system_cpu_usage: Some(system),
            online_cpus: cores,
            throttling_data: None,
        }
    }

    fn response(current: ContainerCpuStats, previous: ContainerCpuStats) -> ContainerStatsResponse {
        ContainerStatsResponse {
            cpu_stats: Some(current),
            precpu_stats: Some(previous),
            ..Default::default()
        }
    }

    #[test]
    fn computes_cpu_percent_from_two_samples() {
        // 50ms of CPU out of 1000ms of machine time on 4 cores = 20%.
        let stats = response(cpu(50_000_000, 1_000_000_000, Some(4)), cpu(0, 0, Some(4)));

        let percent = cpu_percent(&stats).expect("percent is computable");
        assert!((percent - 20.0).abs() < 0.01, "got {percent}");
    }

    #[test]
    fn cpu_percent_scales_with_core_count() {
        let one = cpu_percent(&response(cpu(1_000, 10_000, Some(1)), cpu(0, 0, Some(1))))
            .expect("computable");
        let eight = cpu_percent(&response(cpu(1_000, 10_000, Some(8)), cpu(0, 0, Some(8))))
            .expect("computable");

        assert!((eight - one * 8.0).abs() < 0.01, "{one} vs {eight}");
    }

    #[test]
    fn cpu_percent_falls_back_to_the_per_core_usage_length() {
        let mut current = cpu(1_000, 10_000, None);
        if let Some(usage) = current.cpu_usage.as_mut() {
            usage.percpu_usage = Some(vec![0; 2]);
        }

        let percent = cpu_percent(&response(current, cpu(0, 0, None))).expect("computable");
        assert!((percent - 20.0).abs() < 0.01, "got {percent}");
    }

    #[test]
    fn cpu_percent_is_none_when_the_system_usage_is_missing() {
        let mut current = cpu(1_000, 10_000, Some(1));
        current.system_cpu_usage = None;

        assert_eq!(
            cpu_percent(&response(current, cpu(0, 0, Some(1)))),
            None,
            "windows containers omit system_cpu_usage; that is not zero percent"
        );
    }

    #[test]
    fn cpu_percent_is_none_when_the_samples_cover_no_time() {
        assert_eq!(
            cpu_percent(&response(
                cpu(1_000, 5_000, Some(1)),
                cpu(0, 5_000, Some(1))
            )),
            None
        );
    }

    #[test]
    fn cpu_percent_is_none_when_the_counters_went_backwards() {
        assert_eq!(
            cpu_percent(&response(cpu(500, 10_000, Some(1)), cpu(1_000, 0, Some(1)))),
            None,
            "a restarted container resets the counters; a negative rate is not a rate"
        );
    }

    #[test]
    fn cpu_percent_is_none_without_a_previous_sample() {
        let stats = ContainerStatsResponse {
            cpu_stats: Some(cpu(1_000, 10_000, Some(1))),
            precpu_stats: None,
            ..Default::default()
        };

        assert_eq!(cpu_percent(&stats), None);
    }

    #[test]
    fn cpu_percent_is_none_when_no_cores_are_reported() {
        assert_eq!(
            cpu_percent(&response(cpu(1_000, 10_000, Some(0)), cpu(0, 0, Some(0)))),
            None
        );
    }

    #[test]
    fn memory_excludes_the_inactive_file_cache() {
        let stats = ContainerStatsResponse {
            memory_stats: Some(ContainerMemoryStats {
                usage: Some(100 * 1024 * 1024),
                stats: Some(
                    [("inactive_file".to_owned(), 40 * 1024 * 1024)]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert_eq!(memory_bytes(&stats), Some(60 * 1024 * 1024));
    }

    #[test]
    fn memory_uses_the_raw_usage_when_no_cache_is_reported() {
        let stats = ContainerStatsResponse {
            memory_stats: Some(ContainerMemoryStats {
                usage: Some(4096),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert_eq!(memory_bytes(&stats), Some(4096));
    }

    #[test]
    fn memory_never_underflows() {
        let stats = ContainerStatsResponse {
            memory_stats: Some(ContainerMemoryStats {
                usage: Some(10),
                stats: Some([("cache".to_owned(), 999)].into_iter().collect()),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert_eq!(memory_bytes(&stats), Some(0));
    }

    #[test]
    fn memory_is_none_when_docker_reports_no_usage() {
        assert_eq!(memory_bytes(&ContainerStatsResponse::default()), None);
    }

    #[test]
    fn an_empty_stats_response_measures_nothing() {
        assert_eq!(
            StatsSample::from_response(&ContainerStatsResponse::default()),
            StatsSample::UNMEASURED
        );
    }
}
