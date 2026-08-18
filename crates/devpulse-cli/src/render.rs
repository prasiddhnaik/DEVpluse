//! Plain-text rendering for the spike CLI. No colour, no dependencies: this
//! output is meant to be pasted into the spike report.

use std::collections::BTreeMap;
use std::path::Path;

use devpulse_core::{ProjectEvidence, ProjectMatch};
use devpulse_discovery::{
    ObservedProcess, ObservedSocket, PlatformCapabilities, ProcessSnapshot, SocketSnapshot, Support,
};

pub fn processes(snapshot: &ProcessSnapshot, rows: &[&ObservedProcess], matched: usize) {
    println!(
        "{:>7} {:>7} {:>6} {:>9} {:>8}  {:<22} {:<38} EXECUTABLE",
        "PID", "PPID", "CPU%", "MEM", "UPTIME", "NAME", "CWD"
    );
    for p in rows {
        println!(
            "{:>7} {:>7} {:>6.1} {:>9} {:>8}  {:<22} {:<38} {}",
            p.pid,
            p.parent_pid
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into()),
            p.cpu_percent,
            human_bytes(p.memory_bytes),
            human_duration(p.run_time_secs),
            truncate(&p.name, 22),
            truncate(&opt_path(p.cwd.as_deref()), 38),
            opt_path(p.executable.as_deref()),
        );
    }

    let d = snapshot.degradations;
    println!();
    println!(
        "{matched} matched / {total} processes in {ms:.1} ms{warm}",
        total = snapshot.processes.len(),
        ms = snapshot.duration.as_secs_f64() * 1000.0,
        warm = if snapshot.cpu_warming_up {
            " (CPU warming up: values are 0)"
        } else {
            ""
        }
    );
    println!(
        "undisclosed by OS: cwd {} | exe {} | cmd {} | parent {} | user {}",
        d.missing_cwd, d.missing_executable, d.missing_command, d.missing_parent, d.missing_user
    );
}

pub fn sockets(snapshot: &SocketSnapshot, rows: &[&ObservedSocket], names: &BTreeMap<u32, &str>) {
    println!(
        "{:<5} {:<13} {:<24} {:<24} {:>8}  PROCESS",
        "PROTO", "STATE", "LOCAL", "REMOTE", "PID"
    );
    for s in rows {
        let pid = s
            .pids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let name = s
            .pids
            .first()
            .and_then(|pid| names.get(pid))
            .copied()
            .unwrap_or(if s.pids.is_empty() {
                "<owner hidden>"
            } else {
                "?"
            });
        println!(
            "{:<5} {:<13} {:<24} {:<24} {:>8}  {}",
            s.protocol.to_string(),
            format!("{:?}", s.state).to_lowercase(),
            format!("{}:{}", s.local_addr, s.local_port),
            match (s.remote_addr, s.remote_port) {
                (Some(addr), Some(port)) => format!("{addr}:{port}"),
                _ => "-".to_string(),
            },
            if pid.is_empty() { "-".into() } else { pid },
            name,
        );
    }

    println!();
    println!(
        "{matched} matched / {total} sockets in {ms:.1} ms | owner undisclosed for {hidden}",
        matched = rows.len(),
        total = snapshot.sockets.len(),
        ms = snapshot.duration.as_secs_f64() * 1000.0,
        hidden = snapshot.sockets_without_owner,
    );
}

pub fn projects(
    rows: &[(ProjectMatch, Vec<&ObservedProcess>)],
    sockets: &SocketSnapshot,
    unresolved: usize,
) {
    for (project, procs) in rows {
        let ports = super::listening_ports(sockets, procs);
        println!(
            "{} [{}] confidence {:.2}",
            project.name, project.kind, project.confidence
        );
        println!("  root      {}", project.root.display());
        println!(
            "  processes {}",
            procs
                .iter()
                .map(|p| format!("{}({})", p.name, p.pid))
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!(
            "  listening {}",
            if ports.is_empty() {
                "-".to_string()
            } else {
                ports
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
        println!("  evidence  {}", evidence_summary(&project.evidence));
        println!();
    }
    println!(
        "{} project(s); {} process(es) had an unreadable working directory",
        rows.len(),
        unresolved
    );
}

pub fn project_match(m: &ProjectMatch) {
    println!("root       {}", m.root.display());
    println!("name       {}", m.name);
    println!("kind       {}", m.kind);
    println!("confidence {:.2}", m.confidence);
    println!("evidence:");
    for e in &m.evidence {
        println!("  - {}", evidence_line(e));
    }
}

pub fn capabilities(caps: &PlatformCapabilities) {
    println!("platform            {}", caps.os);
    println!("process list        {}", support(caps.process_list));
    println!("process cwd         {}", support(caps.process_cwd));
    println!("process exe         {}", support(caps.process_exe));
    println!("process command     {}", support(caps.process_command));
    println!("socket list         {}", support(caps.socket_list));
    println!("socket owner pid    {}", support(caps.socket_owner_pid));
    println!("root widens view    {}", caps.root_widens_view);
    println!("notes:");
    for note in caps.notes {
        println!("  - {note}");
    }
}

fn support(s: Support) -> &'static str {
    match s {
        Support::Full => "full",
        Support::SameUserOnly => "same-user only",
        Support::Unavailable => "unavailable",
    }
}

/// Group-level evidence. `CwdAncestry` is deliberately omitted: the depth of a
/// working directory is a fact about one process, not about the project, and
/// printing one process's depth for a whole group would misrepresent it. The
/// per-process working directories remain in `--json` output.
fn evidence_summary(evidence: &[ProjectEvidence]) -> String {
    evidence
        .iter()
        .filter(|e| !matches!(e, ProjectEvidence::CwdAncestry { .. }))
        .map(evidence_line)
        .collect::<Vec<_>>()
        .join("; ")
}

fn evidence_line(e: &ProjectEvidence) -> String {
    match e {
        ProjectEvidence::GitRoot { path } => format!("git root at {}", path.display()),
        ProjectEvidence::WorkspaceRoot { path, marker } => {
            format!("{marker} workspace at {}", path.display())
        }
        ProjectEvidence::ProjectManifest { path, marker } => {
            format!("{marker} manifest at {}", path.display())
        }
        ProjectEvidence::ComposeFile { path } => format!("compose file at {}", path.display()),
        ProjectEvidence::CwdAncestry { depth, .. } => format!("cwd {depth} level(s) below root"),
    }
}

fn opt_path(path: Option<&Path>) -> String {
    path.map(|p| p.display().to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn truncate(value: &str, max: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max {
        return value.to_string();
    }
    // Keep the tail of paths: the leaf is the informative part.
    let tail: String = chars[chars.len() - (max - 1)..].iter().collect();
    format!("…{tail}")
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}{}", UNITS[0])
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

fn human_duration(secs: u64) -> String {
    let (d, h, m, s) = (
        secs / 86_400,
        (secs % 86_400) / 3600,
        (secs % 3600) / 60,
        secs % 60,
    );
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m}m")
    } else if m > 0 {
        format!("{m}m{s}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(512), "512B");
        assert_eq!(human_bytes(2048), "2.0K");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0M");
    }

    #[test]
    fn human_duration_scales() {
        assert_eq!(human_duration(45), "45s");
        assert_eq!(human_duration(125), "2m5s");
        assert_eq!(human_duration(7200), "2h0m");
        assert_eq!(human_duration(200_000), "2d7h");
    }

    #[test]
    fn truncate_keeps_path_tail() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("/very/long/path/to/leaf", 10), "…h/to/leaf");
    }
}
