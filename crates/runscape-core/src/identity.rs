//! Stable service identity (task T1.2).
//!
//! A PID identifies a process instance, not a service (`AGENTS.md` rule 5). A
//! dev server that crashes and restarts is the same service to the developer,
//! so identity is derived from what is stable about it rather than from what
//! the kernel happened to hand out.
//!
//! # Fingerprint inputs
//!
//! Host process:
//!
//! ```text
//! host | project root | runtime | executable file name | cwd | primary listening port
//! ```
//!
//! Container:
//!
//! ```text
//! container | compose project | compose service        (when Compose labels exist)
//! container | container name                            (otherwise)
//! ```
//!
//! The two namespaces are disjoint, so a containerised Postgres and a host
//! Postgres can never collapse into one service.
//!
//! # Consequences, stated plainly
//!
//! * Restarting on the same port keeps the identity — that is the point.
//! * Restarting on a *different* port produces a different service. Port is the
//!   only thing that separates two dev servers started from the same directory
//!   with the same runtime, so it has to be part of the key. The registry
//!   handles the resulting churn by expiring the old service normally.
//! * A service with no listening port is keyed on the rest of the inputs, so
//!   several portless workers in one project stay distinct only if their
//!   executable or cwd differs.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, ServiceId};

/// The runtime a service is executed by. Used for display and as a fingerprint
/// input; never inferred beyond what the executable name states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    Node,
    Bun,
    Deno,
    Python,
    Rust,
    Go,
    Java,
    Ruby,
    Php,
    DotNet,
    Container,
    /// Recognised binary that is not a language runtime (postgres, redis, …) or
    /// an executable Runscape has no opinion about.
    Native,
}

impl Runtime {
    /// Classify from an executable path. Conservative: anything unrecognised is
    /// [`Runtime::Native`], never a guess.
    pub fn from_executable(executable: Option<&Path>) -> Self {
        let Some(name) = executable
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
        else {
            return Self::Native;
        };
        let name = name.to_ascii_lowercase();
        let stem = name.strip_suffix(".exe").unwrap_or(&name);

        match stem {
            "node" | "nodejs" => Self::Node,
            "bun" => Self::Bun,
            "deno" => Self::Deno,
            "java" => Self::Java,
            "ruby" => Self::Ruby,
            "php" | "php-fpm" => Self::Php,
            "dotnet" => Self::DotNet,
            "go" => Self::Go,
            _ if stem.starts_with("python") => Self::Python,
            _ => Self::Native,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Bun => "bun",
            Self::Deno => "deno",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Java => "java",
            Self::Ruby => "ruby",
            Self::Php => "php",
            Self::DotNet => "dotnet",
            Self::Container => "container",
            Self::Native => "native",
        }
    }
}

impl std::fmt::Display for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Identity of a container, when the service is one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerIdentity {
    /// Docker container name without the leading slash.
    pub name: String,
    /// `com.docker.compose.project`, when present.
    pub compose_project: Option<String>,
    /// `com.docker.compose.service`, when present.
    pub compose_service: Option<String>,
}

impl ContainerIdentity {
    /// Compose labels survive `docker compose up` recreating the container,
    /// which the container name and id do not.
    fn canonical(&self) -> String {
        match (&self.compose_project, &self.compose_service) {
            (Some(project), Some(service)) => format!("container|{project}|{service}"),
            _ => format!("container|{}", self.name),
        }
    }
}

/// Everything the fingerprint is computed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceFingerprint {
    canonical: String,
}

impl ServiceFingerprint {
    /// Fingerprint a host process.
    pub fn host(
        project: Option<&ProjectId>,
        runtime: Runtime,
        executable: Option<&Path>,
        cwd: Option<&Path>,
        primary_port: Option<u16>,
    ) -> Self {
        let project = project.map(ProjectId::as_str).unwrap_or("-");
        let exe = executable
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("-");
        let cwd = cwd.map(|p| p.to_string_lossy().into_owned());
        let cwd = cwd.as_deref().unwrap_or("-");
        let port = primary_port
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string());

        Self {
            canonical: format!("host|{project}|{runtime}|{exe}|{cwd}|{port}"),
        }
    }

    /// Fingerprint a container.
    pub fn container(identity: &ContainerIdentity) -> Self {
        Self {
            canonical: identity.canonical(),
        }
    }

    /// The exact string that is hashed. Exposed for diagnostics and tests so
    /// that identity decisions can be explained rather than trusted.
    pub fn canonical(&self) -> &str {
        &self.canonical
    }

    pub fn service_id(&self) -> ServiceId {
        ServiceId::derived(&self.canonical)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn project() -> ProjectId {
        ProjectId::derived("/Users/dev/code/app")
    }

    fn node_service(pid_independent_port: Option<u16>) -> ServiceId {
        ServiceFingerprint::host(
            Some(&project()),
            Runtime::Node,
            Some(&PathBuf::from("/usr/local/bin/node")),
            Some(&PathBuf::from("/Users/dev/code/app")),
            pid_independent_port,
        )
        .service_id()
    }

    #[test]
    fn identity_survives_a_restart_with_a_new_pid() {
        // Nothing about the fingerprint mentions a PID, so a restart on the
        // same port is the same service by construction.
        assert_eq!(node_service(Some(3000)), node_service(Some(3000)));
    }

    #[test]
    fn same_runtime_in_different_projects_stays_separate() {
        let a = ServiceFingerprint::host(
            Some(&ProjectId::derived("/code/a")),
            Runtime::Node,
            Some(&PathBuf::from("/usr/local/bin/node")),
            Some(&PathBuf::from("/code/a")),
            Some(3000),
        );
        let b = ServiceFingerprint::host(
            Some(&ProjectId::derived("/code/b")),
            Runtime::Node,
            Some(&PathBuf::from("/usr/local/bin/node")),
            Some(&PathBuf::from("/code/b")),
            Some(3000),
        );
        assert_ne!(a.service_id(), b.service_id());
    }

    #[test]
    fn same_project_different_ports_are_different_services() {
        assert_ne!(node_service(Some(3000)), node_service(Some(8080)));
    }

    #[test]
    fn container_identity_never_collides_with_a_host_process() {
        let host = ServiceFingerprint::host(
            Some(&project()),
            Runtime::Native,
            Some(&PathBuf::from("/usr/local/bin/postgres")),
            Some(&PathBuf::from("/Users/dev/code/app")),
            Some(5432),
        );
        let container = ServiceFingerprint::container(&ContainerIdentity {
            name: "app-postgres-1".into(),
            compose_project: Some("app".into()),
            compose_service: Some("postgres".into()),
        });
        assert_ne!(host.service_id(), container.service_id());
        assert!(container.canonical().starts_with("container|"));
        assert!(host.canonical().starts_with("host|"));
    }

    #[test]
    fn container_identity_survives_recreation_via_compose_labels() {
        let before = ContainerIdentity {
            name: "app-postgres-1".into(),
            compose_project: Some("app".into()),
            compose_service: Some("postgres".into()),
        };
        let after = ContainerIdentity {
            name: "app-postgres-2".into(),
            compose_project: Some("app".into()),
            compose_service: Some("postgres".into()),
        };
        assert_eq!(
            ServiceFingerprint::container(&before).service_id(),
            ServiceFingerprint::container(&after).service_id()
        );
    }

    #[test]
    fn container_without_compose_labels_falls_back_to_its_name() {
        let identity = ContainerIdentity {
            name: "lonely-redis".into(),
            compose_project: None,
            compose_service: None,
        };
        assert_eq!(
            ServiceFingerprint::container(&identity).canonical(),
            "container|lonely-redis"
        );
    }

    #[test]
    fn runtime_classification_is_conservative() {
        assert_eq!(
            Runtime::from_executable(Some(&PathBuf::from("/usr/bin/node"))),
            Runtime::Node
        );
        assert_eq!(
            Runtime::from_executable(Some(&PathBuf::from("/opt/python3.14"))),
            Runtime::Python
        );
        assert_eq!(
            Runtime::from_executable(Some(&PathBuf::from("/usr/local/bin/postgres"))),
            Runtime::Native
        );
        assert_eq!(Runtime::from_executable(None), Runtime::Native);
    }

    #[test]
    fn portless_services_are_still_distinguishable_by_executable() {
        let worker = ServiceFingerprint::host(
            Some(&project()),
            Runtime::Python,
            Some(&PathBuf::from("/usr/bin/celery")),
            Some(&PathBuf::from("/Users/dev/code/app")),
            None,
        );
        let scheduler = ServiceFingerprint::host(
            Some(&project()),
            Runtime::Python,
            Some(&PathBuf::from("/usr/bin/beat")),
            Some(&PathBuf::from("/Users/dev/code/app")),
            None,
        );
        assert_ne!(worker.service_id(), scheduler.service_id());
    }
}
