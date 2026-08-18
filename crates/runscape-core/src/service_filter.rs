//! What counts as a service.
//!
//! Runscape promises to show *services*, not every process on the machine. A
//! developer's shell, the `sleep` inside a script, and the `git` a tool just
//! shelled out to are all processes in a project directory, and none of them is
//! a service. Left in, they are worse than noise: a `sleep` that runs every few
//! seconds in the same directory has one stable fingerprint and a new PID each
//! time, which the registry correctly reads as a service restarting in a loop.
//! The warning that follows is technically true and completely useless.
//!
//! The rule is deliberately mechanical, so it can be read, tested and argued
//! with:
//!
//! 1. Anything listening on a port is a service. A port is the strongest
//!    evidence there is that something is serving.
//! 2. Anything else must clear four bars: it is not one of the operating
//!    system's own tools, it is not a GUI app bundle, it is not a compiler or
//!    build driver, and it has been alive for at least [`MIN_PORTLESS_LIFETIME`].
//!
//! Rule 2 keeps a worker or a queue consumer — the source end of most edges
//! Runscape draws — while dropping `/bin/zsh`, the `sleep` in a script, and the
//! `rustc` a build just spawned. The lifetime bar is what separates "a process
//! this project runs" from "a step in a build": a build step is measured in
//! seconds, a service in hours.
//!
//! The cost is that a genuinely new portless worker takes half a minute to
//! appear. That is the right trade: a late service is a small annoyance, a
//! project listing full of dead build steps makes the product useless.
//!
//! A process whose executable the OS would not disclose is not a service
//! either: Runscape cannot say what it is, and guessing is not an option
//! (`AGENTS.md` rule 3).

use std::path::Path;
use std::time::Duration;

/// How long a process with no listening port must have been alive before it
/// counts as a service.
pub const MIN_PORTLESS_LIFETIME: Duration = Duration::from_secs(30);

/// Directories that hold the operating system's own tools rather than a
/// developer's software. `/usr/local` and `/opt` are deliberately absent:
/// Homebrew and hand-installed runtimes live there.
const SYSTEM_BIN_DIRS: &[&str] = &[
    "/bin/",
    "/sbin/",
    "/usr/bin/",
    "/usr/sbin/",
    "/usr/libexec/",
    "/System/",
    "/Library/Apple/",
    "C:\\Windows\\",
];

/// GUI app bundles. A Chrome helper with a readable cwd is not a service; walking
/// `/Applications` trees for `.git` is wasted work. Listeners still count:
/// Postgres.app and Docker Desktop bind ports on purpose.
///
/// Path prefixes only — no process-name denylist.
const BUNDLED_APP_DIRS: &[&str] = &[
    "/Applications/",
    "C:\\Program Files\\",
    "C:\\Program Files (x86)\\",
];

/// Compilers, linkers and build drivers. None of these is ever a service, and
/// a build spawns dozens of them inside a project directory.
///
/// Matched on the executable's file name, case-insensitively, with a `.exe`
/// suffix stripped.
const BUILD_TOOLS: &[&str] = &[
    "cargo",
    "rustc",
    "rustdoc",
    "clippy-driver",
    "rust-analyzer",
    "rust-analyzer-proc-macro-srv",
    "cc",
    "cc1",
    "cc1plus",
    "clang",
    "clang++",
    "gcc",
    "g++",
    "ld",
    "lld",
    "ld64.lld",
    "ar",
    "as",
    "collect2",
    "make",
    "gmake",
    "cmake",
    "ninja",
    "sccache",
    "tsc",
    "esbuild",
    "javac",
    "kotlinc",
    "go",
    "gopls",
    "xcrun",
    "xcodebuild",
    "swift-frontend",
];

/// Whether an observed process should be treated as a service.
///
/// * `listening` — whether the process owns at least one listening socket.
/// * `uptime` — how long it has been alive, when that is knowable.
pub fn is_service_process(
    executable: Option<&Path>,
    listening: bool,
    uptime: Option<Duration>,
) -> bool {
    if listening {
        return true;
    }

    let Some(executable) = executable else {
        // No executable path means the OS would not disclose one, which happens
        // for processes owned by another user. Nothing can be said about it.
        return false;
    };

    if is_system_tool(executable) || is_build_tool(executable) || is_bundled_app(executable) {
        return false;
    }

    // An unknown uptime is treated as too young: Runscape would rather show a
    // service a moment late than fill a project with build steps.
    uptime.is_some_and(|uptime| uptime >= MIN_PORTLESS_LIFETIME)
}

/// Whether an executable is a compiler, linker or build driver.
pub fn is_build_tool(executable: &Path) -> bool {
    let Some(name) = executable.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    let stem = name.strip_suffix(".exe").unwrap_or(&name);
    BUILD_TOOLS.contains(&stem)
}

/// Whether a path is one of the operating system's own binaries.
pub fn is_system_tool(executable: &Path) -> bool {
    path_starts_with_any(executable, SYSTEM_BIN_DIRS)
}

/// Whether a path lives inside a GUI app bundle (`/Applications`, Program Files).
pub fn is_bundled_app(path: &Path) -> bool {
    path_starts_with_any(path, BUNDLED_APP_DIRS)
}

fn path_starts_with_any(path: &Path, prefixes: &[&str]) -> bool {
    let path = path.to_string_lossy();
    prefixes
        .iter()
        .any(|dir| path.starts_with(dir) || path.to_lowercase().starts_with(&dir.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path(value: &str) -> PathBuf {
        PathBuf::from(value)
    }

    /// Comfortably past the lifetime bar.
    fn settled() -> Option<Duration> {
        Some(Duration::from_secs(600))
    }

    #[test]
    fn a_listening_process_is_always_a_service() {
        // Even a system tool: `python3 -m http.server` is somebody's dev server,
        // and even in its first second.
        assert!(is_service_process(
            Some(&path("/usr/bin/python3")),
            true,
            Some(Duration::from_millis(50))
        ));
        assert!(is_service_process(None, true, None));
    }

    #[test]
    fn shell_tooling_is_not_a_service() {
        for tool in [
            "/bin/zsh",
            "/bin/sleep",
            "/bin/sh",
            "/usr/bin/git",
            "/usr/bin/grep",
            "/usr/sbin/cupsd",
            "/usr/libexec/rosetta/oahd",
        ] {
            assert!(
                !is_service_process(Some(&path(tool)), false, settled()),
                "{tool} must not be reported as a service"
            );
        }
    }

    #[test]
    fn a_build_is_not_a_service_however_long_it_takes() {
        for tool in [
            "/Users/dev/.cargo/bin/cargo",
            "/Users/dev/.rustup/toolchains/stable/bin/rustc",
            "/opt/homebrew/bin/ninja",
            "/Users/dev/.rustup/toolchains/stable/libexec/rust-analyzer-proc-macro-srv",
        ] {
            assert!(
                !is_service_process(Some(&path(tool)), false, settled()),
                "{tool} is a build step, not a service"
            );
        }
    }

    #[test]
    fn developer_software_is_a_service_once_it_has_stuck_around() {
        // A worker or a client has no port and is still part of the picture:
        // it is the source end of most edges Runscape draws.
        for binary in [
            "/Users/dev/project/target/debug/worker",
            "/opt/homebrew/bin/node",
            "/usr/local/bin/python3.12",
            "/Users/dev/.bun/bin/bun",
        ] {
            assert!(
                is_service_process(Some(&path(binary)), false, settled()),
                "{binary} must be reported as a service"
            );
        }
    }

    #[test]
    fn a_portless_process_that_just_started_is_not_a_service_yet() {
        let binary = path("/Users/dev/project/target/debug/worker");
        assert!(!is_service_process(
            Some(&binary),
            false,
            Some(Duration::from_secs(2))
        ));
        assert!(
            !is_service_process(Some(&binary), false, None),
            "an unknown uptime is treated as too young"
        );
    }

    #[test]
    fn a_process_with_no_disclosed_executable_is_not_a_service() {
        assert!(!is_service_process(None, false, settled()));
    }

    #[test]
    fn homebrew_and_opt_are_not_system_directories() {
        assert!(!is_system_tool(&path("/opt/homebrew/bin/node")));
        assert!(!is_system_tool(&path("/usr/local/bin/deno")));
        assert!(is_system_tool(&path("/usr/bin/env")));
    }

    #[test]
    fn bundled_gui_apps_are_not_services_unless_they_listen() {
        let chrome = path("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
        let vscode = path("C:\\Program Files\\Microsoft VS Code\\Code.exe");
        assert!(is_bundled_app(&chrome));
        assert!(is_bundled_app(&vscode));
        assert!(!is_service_process(Some(&chrome), false, settled()));
        assert!(!is_service_process(Some(&vscode), false, settled()));
        assert!(is_service_process(
            Some(&path("/Applications/Postgres.app/Contents/MacOS/postgres")),
            true,
            Some(Duration::from_millis(50))
        ));
        assert!(!is_bundled_app(&path(
            "/Users/dev/project/target/debug/worker"
        )));
        assert!(!is_bundled_app(&path("/opt/homebrew/bin/node")));
    }
}
