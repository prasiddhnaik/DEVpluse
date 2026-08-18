//! What the current operating system can actually tell us.
//!
//! `AGENTS.md` rule 3 forbids inventing OS capabilities. This module states,
//! per platform, which discovery facts are reliable, which are partial, and
//! what privileges change the answer. The CLI prints it (`devpulse
//! capabilities`) and the spike report quotes it.

use serde::Serialize;

/// How well a given fact can be observed on this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    /// Available for every process we can enumerate.
    Full,
    /// Available only for processes owned by the current user (unless root).
    SameUserOnly,
    /// Never available on this platform; the field stays `None`.
    Unavailable,
}

/// Capability matrix for the running platform.
#[derive(Debug, Clone, Serialize)]
pub struct PlatformCapabilities {
    pub os: &'static str,
    /// Enumerating the process table.
    pub process_list: Support,
    /// Reading a process working directory.
    pub process_cwd: Support,
    /// Reading a process executable path.
    pub process_exe: Support,
    /// Reading a process command line.
    pub process_command: Support,
    /// Enumerating listening/established sockets.
    pub socket_list: Support,
    /// Attributing a socket to its owning PID.
    pub socket_owner_pid: Support,
    /// Whether elevated privileges widen the socket/process view.
    pub root_widens_view: bool,
    pub notes: &'static [&'static str],
}

/// Capabilities of the platform this binary was compiled for.
pub const fn capabilities() -> PlatformCapabilities {
    #[cfg(target_os = "macos")]
    {
        PlatformCapabilities {
            os: "macos",
            process_list: Support::Full,
            process_cwd: Support::SameUserOnly,
            process_exe: Support::SameUserOnly,
            process_command: Support::SameUserOnly,
            socket_list: Support::SameUserOnly,
            socket_owner_pid: Support::SameUserOnly,
            root_widens_view: true,
            notes: &[
                "Socket enumeration uses libproc (proc_pidfdinfo); it walks the file \
                 descriptors of processes the caller may inspect, so an unprivileged run \
                 sees only its own user's sockets.",
                "cwd, exe and argv come from proc_pidinfo/KERN_PROCARGS2 and are None for \
                 processes owned by other users without root.",
                "Sockets owned by another user's process are invisible entirely - they are \
                 not reported with an unknown PID.",
                "No entitlement or kernel extension is required for the same-user view.",
                "A connection still queued in a listener's accept backlog has no file \
                 descriptor in the server process, so it is attributed to the client only \
                 until the server calls accept(). Topology is eventually consistent, \
                 typically within one snapshot interval.",
            ],
        }
    }
    #[cfg(target_os = "linux")]
    {
        PlatformCapabilities {
            os: "linux",
            process_list: Support::Full,
            process_cwd: Support::SameUserOnly,
            process_exe: Support::SameUserOnly,
            process_command: Support::Full,
            socket_list: Support::Full,
            socket_owner_pid: Support::SameUserOnly,
            root_widens_view: true,
            notes: &[
                "Sockets are listed from /proc/net/{tcp,tcp6,udp,udp6} for every user, but \
                 mapping an inode to a PID requires reading /proc/<pid>/fd, which is \
                 restricted to the owning user (or root).",
                "/proc/<pid>/cwd and /proc/<pid>/exe are readlink-restricted to the owner.",
                "hidepid=2 mounts hide other users' processes entirely.",
                "As on macOS, a connection is only attributable to the server process once \
                 accept() has assigned it a descriptor.",
            ],
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        PlatformCapabilities {
            os: std::env::consts::OS,
            process_list: Support::Full,
            process_cwd: Support::Unavailable,
            process_exe: Support::Unavailable,
            process_command: Support::Unavailable,
            socket_list: Support::Unavailable,
            socket_owner_pid: Support::Unavailable,
            root_widens_view: false,
            notes: &["Untested platform: DevPulse targets macOS first, then Linux."],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_describe_the_current_platform() {
        let caps = capabilities();
        assert_eq!(caps.os, std::env::consts::OS);
        assert!(
            !caps.notes.is_empty(),
            "every platform must document limits"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_socket_ownership_is_same_user_only() {
        assert_eq!(capabilities().socket_owner_pid, Support::SameUserOnly);
    }
}
