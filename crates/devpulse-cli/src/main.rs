//! DevPulse discovery spike CLI (Milestone 0).
//!
//! Read-only inspection of what this machine is running. No control commands
//! exist and none are planned for the MVP (see `DECISIONS.md` D004).

mod render;

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use devpulse_core::{NoProject, ProjectMatch, ProjectResolver, ResolverConfig};
use devpulse_discovery::{
    Netstat2SocketCollector, ObservedProcess, ProcessCollector, ProcessSnapshot, SocketCollector,
    SocketSnapshot, SysinfoProcessCollector, capabilities, process::MINIMUM_CPU_UPDATE_INTERVAL,
};
use devpulse_server::daemon::{Daemon, DaemonConfig};
use devpulse_server::security::DEFAULT_PORT;

#[derive(Debug, Parser)]
#[command(
    name = "devpulse",
    version,
    about = "Local-first developer runtime discovery (Milestone 0 spike CLI)"
)]
struct Cli {
    /// Emit JSON instead of a table.
    #[arg(long, global = true)]
    json: bool,

    /// Log filter, e.g. `debug` or `devpulse_discovery=debug`.
    #[arg(long, global = true, default_value = "warn")]
    log: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List processes with PID, parent, executable, cwd, CPU, memory, start time.
    ScanProcesses(ScanProcessesArgs),
    /// List listening sockets and active TCP connections with owning PIDs.
    ScanSockets(ScanSocketsArgs),
    /// Group running processes into projects using working-directory evidence.
    ScanProjects(ScanProjectsArgs),
    /// Resolve a single directory to a project root.
    ResolveProject(ResolveProjectArgs),
    /// Measure collector cost against the polling budget.
    Bench(BenchArgs),
    /// Print what this operating system can and cannot disclose.
    Capabilities,
    /// Run the daemon: continuous discovery plus the local HTTP/WebSocket API.
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
struct ServeArgs {
    /// Port to listen on. The address is always loopback.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,

    /// Seconds between snapshots.
    #[arg(long, default_value_t = 1)]
    interval: u64,

    /// Skip the Docker probe at startup.
    #[arg(long)]
    no_docker: bool,

    /// Sample per-container CPU and memory. Costs about a second per snapshot
    /// batch, because Docker needs two samples to compute a percentage.
    #[arg(long)]
    docker_stats: bool,

    /// SQLite file for history. Defaults to `~/.devpulse/devpulse.db`.
    #[arg(long)]
    db: Option<PathBuf>,

    /// Keep no history on disk. Events live in memory and die with the daemon.
    #[arg(long)]
    no_persistence: bool,
}

#[derive(Debug, Args)]
struct BenchArgs {
    /// Snapshots per collector.
    #[arg(long, default_value_t = 20)]
    iterations: usize,
}

#[derive(Debug, Args)]
struct ScanProcessesArgs {
    /// Case-insensitive substring match on name, executable or command.
    #[arg(long)]
    filter: Option<String>,

    /// Maximum rows to print. `0` prints everything.
    #[arg(long, default_value_t = 40)]
    limit: usize,

    #[arg(long, value_enum, default_value_t = ProcessSort::Cpu)]
    sort: ProcessSort,

    /// Skip the CPU warm-up pass. Faster, but every CPU figure will be 0.
    #[arg(long)]
    no_warmup: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProcessSort {
    Cpu,
    Memory,
    Pid,
}

#[derive(Debug, Args)]
struct ScanSocketsArgs {
    /// Only listening sockets.
    #[arg(long)]
    listening: bool,

    /// Only sockets on this local or remote port.
    #[arg(long)]
    port: Option<u16>,

    /// Only sockets owned by this PID.
    #[arg(long)]
    pid: Option<u32>,

    /// Include UDP. TCP only by default.
    #[arg(long)]
    udp: bool,
}

#[derive(Debug, Args)]
struct ScanProjectsArgs {
    /// Hide projects whose confidence is below this value.
    #[arg(long, default_value_t = 0.0)]
    min_confidence: f32,
}

#[derive(Debug, Args)]
struct ResolveProjectArgs {
    /// Directory to resolve. Defaults to the current directory.
    path: Option<PathBuf>,

    /// Ignore the `$HOME` / system-directory exclusion policy.
    #[arg(long)]
    no_exclusions: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(&cli.log);

    match &cli.command {
        Command::ScanProcesses(args) => scan_processes(&cli, args).await,
        Command::ScanSockets(args) => scan_sockets(&cli, args).await,
        Command::ScanProjects(args) => scan_projects(&cli, args).await,
        Command::ResolveProject(args) => resolve_project(&cli, args),
        Command::Bench(args) => bench(&cli, args).await,
        Command::Serve(args) => serve(args).await,
        Command::Capabilities => {
            let caps = capabilities();
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&caps)?);
            } else {
                render::capabilities(&caps);
            }
            Ok(())
        }
    }
}

fn init_tracing(filter: &str) {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .init();
}

/// Take a process snapshot. CPU usage is a delta between two refreshes, so an
/// accurate reading needs a warm-up pass separated by the platform's minimum
/// interval.
async fn process_snapshot(warmup: bool) -> Result<ProcessSnapshot> {
    let collector = SysinfoProcessCollector::new();
    if warmup {
        collector.snapshot().await.context("warm-up snapshot")?;
        tokio::time::sleep(MINIMUM_CPU_UPDATE_INTERVAL).await;
    }
    collector.snapshot().await.context("process snapshot")
}

async fn socket_snapshot(udp: bool) -> Result<SocketSnapshot> {
    let collector = if udp {
        Netstat2SocketCollector::new()
    } else {
        Netstat2SocketCollector::tcp_only()
    };
    collector.snapshot().await.context("socket snapshot")
}

async fn scan_processes(cli: &Cli, args: &ScanProcessesArgs) -> Result<()> {
    let snapshot = process_snapshot(!args.no_warmup).await?;

    let mut processes: Vec<&ObservedProcess> = snapshot
        .processes
        .iter()
        .filter(|p| matches_filter(p, args.filter.as_deref()))
        .collect();

    match args.sort {
        ProcessSort::Cpu => processes.sort_by(|a, b| {
            b.cpu_percent
                .partial_cmp(&a.cpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.pid.cmp(&b.pid))
        }),
        ProcessSort::Memory => {
            processes.sort_by(|a, b| b.memory_bytes.cmp(&a.memory_bytes).then(a.pid.cmp(&b.pid)));
        }
        ProcessSort::Pid => processes.sort_by_key(|p| p.pid),
    }

    let total_matched = processes.len();
    if args.limit > 0 {
        processes.truncate(args.limit);
    }

    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "captured_at_unix": unix_secs(&snapshot),
                "duration_us": snapshot.duration.as_micros(),
                "cpu_warming_up": snapshot.cpu_warming_up,
                "total_processes": snapshot.processes.len(),
                "matched": total_matched,
                "degradations": snapshot.degradations,
                "processes": processes,
            }))?
        );
    } else {
        render::processes(&snapshot, &processes, total_matched);
    }
    Ok(())
}

async fn scan_sockets(cli: &Cli, args: &ScanSocketsArgs) -> Result<()> {
    let snapshot = socket_snapshot(args.udp).await?;
    let processes = process_snapshot(false).await?;
    let names: BTreeMap<u32, &str> = processes
        .processes
        .iter()
        .map(|p| (p.pid, p.name.as_str()))
        .collect();

    let sockets: Vec<_> = snapshot
        .sockets
        .iter()
        .filter(|s| !args.listening || s.is_listening())
        .filter(|s| match args.port {
            Some(port) => s.local_port == port || s.remote_port == Some(port),
            None => true,
        })
        .filter(|s| match args.pid {
            Some(pid) => s.owned_by(pid),
            None => true,
        })
        .collect();

    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "duration_us": snapshot.duration.as_micros(),
                "total_sockets": snapshot.sockets.len(),
                "sockets_without_owner": snapshot.sockets_without_owner,
                "matched": sockets.len(),
                "sockets": sockets,
            }))?
        );
    } else {
        render::sockets(&snapshot, &sockets, &names);
    }
    Ok(())
}

async fn scan_projects(cli: &Cli, args: &ScanProjectsArgs) -> Result<()> {
    let processes = process_snapshot(false).await?;
    let sockets = socket_snapshot(false).await?;
    let resolver = ProjectResolver::default();

    #[derive(Default)]
    struct Group<'a> {
        project: Option<ProjectMatch>,
        processes: Vec<&'a ObservedProcess>,
    }

    let mut groups: BTreeMap<PathBuf, Group<'_>> = BTreeMap::new();
    let mut unresolved = 0usize;

    for process in &processes.processes {
        let Some(cwd) = process.cwd.as_deref() else {
            continue;
        };
        match resolver.resolve(cwd) {
            Ok(m) => {
                if m.confidence < args.min_confidence {
                    continue;
                }
                // The group's identity comes from the root, so the first match
                // wins: overwriting would make the group's evidence describe
                // whichever process happened to be enumerated last.
                let entry = groups.entry(m.root.clone()).or_default();
                entry.processes.push(process);
                entry.project.get_or_insert(m);
            }
            Err(NoProject::NoMarkers { .. } | NoProject::ExcludedRootOnly { .. }) => {}
            Err(NoProject::PathUnavailable { .. }) => unresolved += 1,
        }
    }

    let rows: Vec<_> = groups
        .into_values()
        .filter_map(|g| g.project.map(|p| (p, g.processes)))
        .collect();

    if cli.json {
        let payload: Vec<_> = rows
            .iter()
            .map(|(project, procs)| {
                serde_json::json!({
                    "project": project,
                    "ports": listening_ports(&sockets, procs),
                    "processes": procs,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        render::projects(&rows, &sockets, unresolved);
    }
    Ok(())
}

fn resolve_project(cli: &Cli, args: &ResolveProjectArgs) -> Result<()> {
    let path = match &args.path {
        Some(path) => path.clone(),
        None => std::env::current_dir().context("current directory")?,
    };
    let config = if args.no_exclusions {
        ResolverConfig::bare()
    } else {
        ResolverConfig::detect()
    };

    match ProjectResolver::new(config).resolve(&path) {
        Ok(m) => {
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&m)?);
            } else {
                render::project_match(&m);
            }
            Ok(())
        }
        Err(err) => {
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "resolved": false,
                        "reason": err.to_string(),
                    }))?
                );
                Ok(())
            } else {
                println!("no project: {err}");
                Ok(())
            }
        }
    }
}

/// Measure collector wall-clock cost. `ARCHITECTURE.md` budgets one process
/// and one socket snapshot per second, so these numbers decide whether the
/// polling defaults are affordable.
/// Run the daemon until Ctrl-C.
///
/// The bind address is not configurable beyond the port: DevPulse serves
/// loopback only (`AGENTS.md` rule 6), and `Daemon::bind` refuses anything
/// else.
async fn serve(args: &ServeArgs) -> Result<()> {
    let config = DaemonConfig {
        bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), args.port),
        snapshot: devpulse_server::SnapshotConfig {
            tick_interval: Duration::from_secs(args.interval.max(1)),
            ..Default::default()
        },
        probe_docker: !args.no_docker,
        docker_stats: args.docker_stats,
        database: if args.no_persistence {
            None
        } else {
            args.db
                .clone()
                .or_else(devpulse_server::persistence::default_database_path)
        },
        ..Default::default()
    };

    let database = config.database.clone();
    let daemon = Daemon::bind(config).await?;
    let addr = daemon.local_addr()?;
    println!("devpulse daemon listening on http://{addr}");
    match &database {
        Some(path) => println!("  history   {}", path.display()),
        None => println!("  history   in memory only"),
    }
    println!("  status    http://{addr}/api/v1/status");
    println!("  websocket ws://{addr}/ws/v1");
    println!("press ctrl-c to stop");

    daemon.serve().await
}

async fn bench(cli: &Cli, args: &BenchArgs) -> Result<()> {
    let processes = SysinfoProcessCollector::new();
    let sockets = Netstat2SocketCollector::tcp_only();

    // Discard the first sample of each: it primes caches and CPU deltas.
    processes.snapshot().await?;
    sockets.snapshot().await?;

    let mut process_us = Vec::with_capacity(args.iterations);
    let mut socket_us = Vec::with_capacity(args.iterations);
    let mut process_count = 0usize;
    let mut socket_count = 0usize;

    for _ in 0..args.iterations {
        let snapshot = processes.snapshot().await?;
        process_count = snapshot.processes.len();
        process_us.push(snapshot.duration.as_micros() as u64);

        let snapshot = sockets.snapshot().await?;
        socket_count = snapshot.sockets.len();
        socket_us.push(snapshot.duration.as_micros() as u64);
    }

    let process_stats = Stats::of(&mut process_us);
    let socket_stats = Stats::of(&mut socket_us);

    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "iterations": args.iterations,
                "process": { "items": process_count, "us": process_stats },
                "socket": { "items": socket_count, "us": socket_stats },
            }))?
        );
    } else {
        println!("iterations {}", args.iterations);
        println!(
            "process collector  {process_count:>5} items  min {:.2} ms  p50 {:.2} ms  max {:.2} ms",
            ms(process_stats.min),
            ms(process_stats.p50),
            ms(process_stats.max)
        );
        println!(
            "socket collector   {socket_count:>5} items  min {:.2} ms  p50 {:.2} ms  max {:.2} ms",
            ms(socket_stats.min),
            ms(socket_stats.p50),
            ms(socket_stats.max)
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
struct Stats {
    min: u64,
    p50: u64,
    max: u64,
}

impl Stats {
    /// Sorts `samples` in place.
    fn of(samples: &mut [u64]) -> Self {
        samples.sort_unstable();
        Self {
            min: samples.first().copied().unwrap_or(0),
            p50: samples.get(samples.len() / 2).copied().unwrap_or(0),
            max: samples.last().copied().unwrap_or(0),
        }
    }
}

fn ms(micros: u64) -> f64 {
    micros as f64 / 1000.0
}

/// Listening ports owned by any process in the group.
fn listening_ports(sockets: &SocketSnapshot, processes: &[&ObservedProcess]) -> Vec<u16> {
    let mut ports: Vec<u16> = sockets
        .sockets
        .iter()
        .filter(|s| s.is_listening())
        .filter(|s| processes.iter().any(|p| s.owned_by(p.pid)))
        .map(|s| s.local_port)
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

fn matches_filter(process: &ObservedProcess, filter: Option<&str>) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    let needle = filter.to_lowercase();
    let haystacks = [
        process.name.to_lowercase(),
        process
            .executable
            .as_ref()
            .map(|p| p.to_string_lossy().to_lowercase())
            .unwrap_or_default(),
        process.command.join(" ").to_lowercase(),
        process
            .cwd
            .as_ref()
            .map(|p| p.to_string_lossy().to_lowercase())
            .unwrap_or_default(),
    ];
    haystacks.iter().any(|h| h.contains(&needle))
}

fn unix_secs(snapshot: &ProcessSnapshot) -> u64 {
    snapshot
        .captured_at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
