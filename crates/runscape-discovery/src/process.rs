//! Process discovery (task T0.2).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use runscape_core::redact_command;
use serde::Serialize;
use sysinfo::{
    Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind, get_current_pid,
};

use crate::error::CollectorError;

/// Minimum interval between two refreshes for CPU percentages to be meaningful.
/// Re-exported so callers do not have to depend on `sysinfo` directly.
pub const MINIMUM_CPU_UPDATE_INTERVAL: Duration = sysinfo::MINIMUM_CPU_UPDATE_INTERVAL;

/// Coarse process state, normalised across platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Running,
    Sleeping,
    Idle,
    Stopped,
    Zombie,
    Dead,
    /// The platform reported a state Runscape does not model.
    Unknown,
}

impl From<ProcessStatus> for ProcessState {
    fn from(status: ProcessStatus) -> Self {
        match status {
            ProcessStatus::Run => Self::Running,
            ProcessStatus::Sleep | ProcessStatus::UninterruptibleDiskSleep => Self::Sleeping,
            ProcessStatus::Idle => Self::Idle,
            ProcessStatus::Stop | ProcessStatus::Tracing => Self::Stopped,
            ProcessStatus::Zombie => Self::Zombie,
            ProcessStatus::Dead => Self::Dead,
            _ => Self::Unknown,
        }
    }
}

/// One process as observed at a point in time.
///
/// Every field the OS may withhold is an `Option`. Runscape never substitutes a
/// plausible value for a missing one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ObservedProcess {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    /// Short process name as reported by the OS.
    pub name: String,
    pub executable: Option<PathBuf>,
    /// Command line with likely secrets already replaced. Raw argv is never
    /// retained: redaction happens at capture time.
    pub command: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Percentage of one CPU core. `0.0` on the very first snapshot, because
    /// CPU usage is a delta between two refreshes.
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub virtual_memory_bytes: u64,
    /// `None` when this process was not extra-sampled, or the OS would not
    /// disclose a count without treating Linux threads as processes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_count: Option<u32>,
    /// Bytes read since the previous extra sample of this process.
    pub disk_read_bytes: u64,
    /// Bytes written since the previous extra sample of this process.
    pub disk_write_bytes: u64,
    /// Seconds since the Unix epoch.
    pub start_time_epoch_secs: u64,
    pub run_time_secs: u64,
    pub state: ProcessState,
    pub user_id: Option<String>,
}

/// Counts of fields the OS refused to disclose during a snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Degradations {
    pub missing_cwd: usize,
    pub missing_executable: usize,
    pub missing_command: usize,
    pub missing_parent: usize,
    pub missing_user: usize,
}

/// A full process-table observation.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessSnapshot {
    pub captured_at: SystemTime,
    /// Wall-clock cost of the collector, for the performance budget.
    pub duration: Duration,
    pub processes: Vec<ObservedProcess>,
    pub degradations: Degradations,
    /// `true` when CPU percentages are still warming up (first snapshot).
    pub cpu_warming_up: bool,
    /// 1-minute load average from `sysinfo::System::load_average`.
    pub load_avg_1: f64,
    /// 5-minute load average.
    pub load_avg_5: f64,
    /// 15-minute load average.
    pub load_avg_15: f64,
    /// `processes.len()` at capture time.
    pub process_count: u32,
}

impl ProcessSnapshot {
    pub fn by_pid(&self, pid: u32) -> Option<&ObservedProcess> {
        self.processes.iter().find(|p| p.pid == pid)
    }
}

/// Platform-independent process collection.
#[async_trait]
pub trait ProcessCollector: Send + Sync {
    async fn snapshot(&self) -> Result<ProcessSnapshot, CollectorError>;
}

/// `sysinfo`-backed collector. Works on macOS and Linux; the differences are
/// which fields come back `None` (see [`crate::platform::capabilities`]).
#[derive(Debug)]
pub struct SysinfoProcessCollector {
    /// `sysinfo` needs a persistent `System` to compute CPU deltas between
    /// refreshes, so state is shared across snapshots.
    system: Arc<Mutex<SystemState>>,
}

#[derive(Debug)]
struct SystemState {
    system: System,
    refreshed_once: bool,
    /// Ticks completed. Used to refresh process metadata on a slower cadence
    /// than CPU and memory.
    ticks: u32,
}

impl Default for SysinfoProcessCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl SysinfoProcessCollector {
    pub fn new() -> Self {
        Self {
            system: Arc::new(Mutex::new(SystemState {
                // `System::new()` loads nothing; the first refresh fills it.
                system: System::new(),
                refreshed_once: false,
                ticks: 0,
            })),
        }
    }

    /// PID of the Runscape process itself, when the platform exposes it.
    pub fn own_pid() -> Option<u32> {
        get_current_pid().ok().map(|pid| pid.as_u32())
    }

    /// How often to re-read cwd/exe/argv/user. Those rarely change on a live
    /// process; asking for them every tick is most of the collector's cost.
    const METADATA_REFRESH_EVERY: u32 = 5;

    /// Fields we ask the OS for on every process. `environ` is excluded
    /// (`AGENTS.md` rule 6). `disk_usage` and Linux task enumeration are not
    /// in this pass: disk I/O and thread counts are filled later, and only
    /// for processes that grouping will actually keep.
    fn refresh_kind(full_metadata: bool) -> ProcessRefreshKind {
        let metadata = if full_metadata {
            UpdateKind::Always
        } else {
            // New PIDs still get cwd/exe/cmd on first sight; everyone else
            // keeps the values from the last full pass.
            UpdateKind::OnlyIfNotSet
        };
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_cwd(metadata)
            .with_exe(metadata)
            .with_cmd(metadata)
            .with_user(metadata)
            .without_tasks()
    }

    /// Disk I/O only. Does not enable `tasks`, so Linux threads stay hidden
    /// from the process table.
    fn extra_refresh_kind() -> ProcessRefreshKind {
        ProcessRefreshKind::nothing().with_disk_usage()
    }

    /// Fill disk I/O (and, on Linux, thread counts) for `pids` only.
    ///
    /// Call this after grouping knows which processes belong to a project.
    /// Chrome helpers and other ungrouped processes are left at zero / `None`.
    pub async fn enrich(
        &self,
        snapshot: &mut ProcessSnapshot,
        pids: &HashSet<u32>,
    ) -> Result<(), CollectorError> {
        if pids.is_empty() {
            return Ok(());
        }
        let state = Arc::clone(&self.system);
        let wanted: Vec<u32> = pids.iter().copied().collect();
        let extras = tokio::task::spawn_blocking(move || {
            let mut guard = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            collect_extras(&mut guard, &wanted)
        })
        .await
        .map_err(|source| CollectorError::Join {
            collector: "process",
            source,
        })?;

        for process in &mut snapshot.processes {
            if let Some(extra) = extras.get(&process.pid) {
                process.disk_read_bytes = extra.disk_read_bytes;
                process.disk_write_bytes = extra.disk_write_bytes;
                process.thread_count = extra.thread_count;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl ProcessCollector for SysinfoProcessCollector {
    async fn snapshot(&self) -> Result<ProcessSnapshot, CollectorError> {
        let state = Arc::clone(&self.system);

        // sysinfo is blocking (it walks /proc or issues libproc syscalls), so it
        // must never run on a runtime worker thread.
        let snapshot = tokio::task::spawn_blocking(move || {
            // A panic elsewhere must not disable process discovery for the rest
            // of the daemon's life, so a poisoned lock is recovered rather than
            // propagated.
            let mut guard = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            collect(&mut guard)
        })
        .await
        .map_err(|source| CollectorError::Join {
            collector: "process",
            source,
        })?;

        tracing::debug!(
            processes = snapshot.processes.len(),
            duration_us = snapshot.duration.as_micros(),
            missing_cwd = snapshot.degradations.missing_cwd,
            "process snapshot"
        );

        Ok(snapshot)
    }
}

fn collect(state: &mut SystemState) -> ProcessSnapshot {
    let started = std::time::Instant::now();
    let captured_at = SystemTime::now();

    let full_metadata = state.ticks % SysinfoProcessCollector::METADATA_REFRESH_EVERY == 0;
    state.system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        SysinfoProcessCollector::refresh_kind(full_metadata),
    );
    let cpu_warming_up = !state.refreshed_once;
    state.refreshed_once = true;
    state.ticks = state.ticks.saturating_add(1);

    let mut degradations = Degradations::default();
    let mut processes = Vec::with_capacity(state.system.processes().len());

    for (pid, process) in state.system.processes() {
        let cwd = process.cwd().map(Path::to_path_buf);
        let executable = process.exe().map(Path::to_path_buf);
        let parent_pid = process.parent().map(Pid::as_u32);
        let user_id = process.user_id().map(|uid| uid.to_string());

        let raw_command: Vec<String> = process
            .cmd()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        if cwd.is_none() {
            degradations.missing_cwd += 1;
        }
        if executable.is_none() {
            degradations.missing_executable += 1;
        }
        if raw_command.is_empty() {
            degradations.missing_command += 1;
        }
        if parent_pid.is_none() {
            degradations.missing_parent += 1;
        }
        if user_id.is_none() {
            degradations.missing_user += 1;
        }

        processes.push(ObservedProcess {
            pid: pid.as_u32(),
            parent_pid,
            name: process.name().to_string_lossy().into_owned(),
            executable,
            command: redact_command(&raw_command),
            cwd,
            cpu_percent: process.cpu_usage(),
            memory_bytes: process.memory(),
            virtual_memory_bytes: process.virtual_memory(),
            thread_count: None,
            disk_read_bytes: 0,
            disk_write_bytes: 0,
            start_time_epoch_secs: process.start_time(),
            run_time_secs: process.run_time(),
            state: process.status().into(),
            user_id,
        });
    }

    processes.sort_unstable_by_key(|p| p.pid);

    let load = System::load_average();
    let process_count = u32::try_from(processes.len()).unwrap_or(u32::MAX);

    ProcessSnapshot {
        captured_at,
        duration: started.elapsed(),
        processes,
        degradations,
        cpu_warming_up,
        load_avg_1: load.one,
        load_avg_5: load.five,
        load_avg_15: load.fifteen,
        process_count,
    }
}

struct ExtraResources {
    disk_read_bytes: u64,
    disk_write_bytes: u64,
    thread_count: Option<u32>,
}

fn collect_extras(state: &mut SystemState, pids: &[u32]) -> HashMap<u32, ExtraResources> {
    let sys_pids: Vec<Pid> = pids.iter().map(|&pid| Pid::from(pid as usize)).collect();
    state.system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&sys_pids),
        false,
        SysinfoProcessCollector::extra_refresh_kind(),
    );

    let mut extras = HashMap::with_capacity(pids.len());
    for &pid in pids {
        let Some(process) = state.system.process(Pid::from(pid as usize)) else {
            continue;
        };
        let usage = process.disk_usage();
        extras.insert(
            pid,
            ExtraResources {
                disk_read_bytes: usage.read_bytes,
                disk_write_bytes: usage.written_bytes,
                thread_count: thread_count(pid),
            },
        );
    }
    extras
}

/// Thread count without enumerating Linux tasks as processes.
///
/// sysinfo only exposes `Process::tasks()` when `with_tasks()` is on, which
/// would put every thread in the process table. `/proc/<pid>/status` is the
/// same OS fact, for the PIDs we already decided to extra-sample. Other
/// platforms leave this `None` rather than guessing.
fn thread_count(pid: u32) -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        for line in status.lines() {
            let Some(rest) = line.strip_prefix("Threads:") else {
                continue;
            };
            return rest.trim().parse().ok();
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn snapshot_sees_this_test_process() {
        let collector = SysinfoProcessCollector::new();
        let snapshot = collector.snapshot().await.expect("snapshot");
        let own_pid = SysinfoProcessCollector::own_pid().expect("own pid");

        let me = snapshot
            .by_pid(own_pid)
            .unwrap_or_else(|| panic!("own pid {own_pid} missing from snapshot"));

        assert!(me.memory_bytes > 0, "own memory should be observable");
        assert!(me.start_time_epoch_secs > 0);
        assert_eq!(snapshot.process_count, snapshot.processes.len() as u32);
        assert!(snapshot.load_avg_1 >= 0.0);
        assert!(snapshot.load_avg_5 >= 0.0);
        assert!(snapshot.load_avg_15 >= 0.0);
        assert_eq!(
            me.cwd.as_deref(),
            Some(
                std::fs::canonicalize(std::env::current_dir().expect("cwd"))
                    .expect("canonical cwd")
                    .as_path()
            ),
            "cwd of our own process must be readable"
        );
    }

    #[tokio::test]
    async fn inaccessible_processes_degrade_instead_of_failing() {
        let collector = SysinfoProcessCollector::new();
        let snapshot = collector.snapshot().await.expect("snapshot");

        assert!(
            snapshot.processes.len() > 10,
            "expected a populated process table, got {}",
            snapshot.processes.len()
        );
        // On a normal desktop there are always root-owned processes we cannot
        // fully inspect; the snapshot must still be produced.
        assert!(
            snapshot.degradations.missing_cwd <= snapshot.processes.len(),
            "degradation counters must stay consistent"
        );
    }

    #[tokio::test]
    async fn second_snapshot_reports_cpu_ready() {
        let collector = SysinfoProcessCollector::new();
        let first = collector.snapshot().await.expect("first");
        assert!(first.cpu_warming_up);
        let own_pid = SysinfoProcessCollector::own_pid().expect("own pid");

        tokio::time::sleep(MINIMUM_CPU_UPDATE_INTERVAL).await;
        let second = collector.snapshot().await.expect("second");
        assert!(!second.cpu_warming_up);
        assert!(second.duration > Duration::ZERO, "duration is measured");
        let me = second
            .by_pid(own_pid)
            .unwrap_or_else(|| panic!("own pid {own_pid} missing after cheap refresh"));
        assert_eq!(
            me.cwd.as_deref(),
            Some(
                std::fs::canonicalize(std::env::current_dir().expect("cwd"))
                    .expect("canonical cwd")
                    .as_path()
            ),
            "cached cwd must survive a metadata-skipping refresh"
        );
    }

    #[tokio::test]
    async fn command_lines_are_redacted_at_capture() {
        let collector = SysinfoProcessCollector::new();
        let snapshot = collector.snapshot().await.expect("snapshot");
        for process in &snapshot.processes {
            for arg in &process.command {
                assert!(
                    !arg.contains("ghp_") && !arg.starts_with("sk-"),
                    "unredacted credential shape survived capture: {arg}"
                );
            }
        }
    }

    #[tokio::test]
    async fn enrich_fills_extras_only_for_requested_pids() {
        let collector = SysinfoProcessCollector::new();
        let mut snapshot = collector.snapshot().await.expect("snapshot");
        let own_pid = SysinfoProcessCollector::own_pid().expect("own pid");
        let mut wanted = HashSet::new();
        wanted.insert(own_pid);

        collector
            .enrich(&mut snapshot, &wanted)
            .await
            .expect("enrich");

        let me = snapshot.by_pid(own_pid).expect("own pid");
        #[cfg(target_os = "linux")]
        assert!(
            me.thread_count.is_some_and(|n| n >= 1),
            "Linux /proc status Threads should be readable for our own pid"
        );
        #[cfg(not(target_os = "linux"))]
        assert!(
            me.thread_count.is_none(),
            "thread counts are Linux-only; other platforms must not guess"
        );

        let other = snapshot
            .processes
            .iter()
            .find(|p| p.pid != own_pid)
            .expect("another process");
        assert_eq!(other.disk_read_bytes, 0);
        assert_eq!(other.disk_write_bytes, 0);
        assert!(other.thread_count.is_none());
    }
}
