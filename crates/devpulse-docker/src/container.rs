//! Container observations and their normalisation into the domain vocabulary
//! (tasks T6.2 and T6.3).
//!
//! Everything in this module is a pure mapping from what Docker reported to
//! what DevPulse models. No I/O happens here, which is why the mapping rules
//! that actually decide identity can be unit-tested without a daemon.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, SystemTime};

use bollard::models::{
    ContainerSummary, ContainerSummaryStateEnum, PortSummary, PortSummaryTypeEnum,
};
use devpulse_core::{
    ContainerIdentity, Endpoint, Health, ProjectId, Protocol, Runtime, Service, ServiceFingerprint,
    ServiceKind,
};
use serde::Serialize;

/// Compose writes the project name here. Present on every container Compose
/// creates, and unchanged when Compose recreates it.
pub const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";
/// Compose writes the service name (the key under `services:`) here.
pub const COMPOSE_SERVICE_LABEL: &str = "com.docker.compose.service";

/// Lifecycle state of a container.
///
/// Docker sends a free-form string and grows its vocabulary over time
/// (`removing`, `stopping`, …). Anything DevPulse does not recognise becomes
/// [`ContainerState::Unknown`] rather than being forced into a neighbouring
/// state, because guessing here would leak into health and into events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerState {
    Running,
    Created,
    Restarting,
    Paused,
    Exited,
    Dead,
    Unknown,
}

impl ContainerState {
    /// Map Docker's state string. Case-insensitive and whitespace-tolerant
    /// because this string reaches us from an API, not from a type.
    pub fn from_docker_str(state: &str) -> Self {
        match state.trim().to_ascii_lowercase().as_str() {
            "running" => Self::Running,
            "created" => Self::Created,
            "restarting" => Self::Restarting,
            "paused" => Self::Paused,
            "exited" => Self::Exited,
            "dead" => Self::Dead,
            _ => Self::Unknown,
        }
    }

    /// Health as the developer would read it.
    ///
    /// `Paused` and `Created` are deliberately [`Health::Unknown`] rather than
    /// `Stopped`: the container exists and was not killed, so calling it
    /// stopped would imply a failure that did not happen.
    pub fn health(self) -> Health {
        match self {
            Self::Running => Health::Healthy,
            Self::Restarting => Health::Degraded,
            Self::Exited | Self::Dead => Health::Stopped,
            Self::Created | Self::Paused | Self::Unknown => Health::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Created => "created",
            Self::Restarting => "restarting",
            Self::Paused => "paused",
            Self::Exited => "exited",
            Self::Dead => "dead",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for ContainerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The wire string Docker uses for a summary state, so that the mapping policy
/// lives in exactly one place ([`ContainerState::from_docker_str`]) without
/// allocating a `String` per container per poll.
fn state_wire_str(state: ContainerSummaryStateEnum) -> &'static str {
    match state {
        ContainerSummaryStateEnum::EMPTY => "",
        ContainerSummaryStateEnum::CREATED => "created",
        ContainerSummaryStateEnum::RUNNING => "running",
        ContainerSummaryStateEnum::PAUSED => "paused",
        ContainerSummaryStateEnum::RESTARTING => "restarting",
        ContainerSummaryStateEnum::EXITED => "exited",
        ContainerSummaryStateEnum::REMOVING => "removing",
        ContainerSummaryStateEnum::DEAD => "dead",
        ContainerSummaryStateEnum::STOPPING => "stopping",
    }
}

/// One port mapping of a container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContainerPort {
    /// Port inside the container.
    pub private_port: u16,
    /// Port on the host, when the mapping is published. `None` means the port
    /// is only exposed inside Docker networks and is not reachable from the
    /// host, so it is not an endpoint DevPulse can correlate to a host socket.
    pub public_port: Option<u16>,
    pub protocol: Protocol,
    /// Host address the port is bound to, when Docker disclosed one.
    pub host_ip: Option<IpAddr>,
}

impl ContainerPort {
    /// Map one Docker port summary.
    ///
    /// Returns `None` for protocols DevPulse has no domain type for (SCTP):
    /// dropping the mapping is honest, forcing it to TCP would not be.
    pub fn from_summary(port: &PortSummary) -> Option<Self> {
        let protocol = match port.typ {
            // Docker's documented default when the field is absent or empty.
            None | Some(PortSummaryTypeEnum::EMPTY) | Some(PortSummaryTypeEnum::TCP) => {
                Protocol::Tcp
            }
            Some(PortSummaryTypeEnum::UDP) => Protocol::Udp,
            Some(PortSummaryTypeEnum::SCTP) => return None,
        };

        Some(Self {
            private_port: port.private_port,
            public_port: port.public_port,
            protocol,
            host_ip: port
                .ip
                .as_deref()
                .filter(|ip| !ip.is_empty())
                .and_then(|ip| ip.parse().ok()),
        })
    }

    /// Whether the port is reachable from the host.
    pub fn is_published(&self) -> bool {
        self.public_port.is_some()
    }

    /// The host-side endpoint, when published.
    ///
    /// An absent host IP means "every interface", which is what Docker's empty
    /// `IP` field denotes, so it becomes the unspecified address rather than a
    /// guess at a concrete interface. The owning PID is always `None`: the
    /// container list does not disclose the host PID of the container's
    /// processes, and inventing one would break socket correlation.
    pub fn endpoint(&self) -> Option<Endpoint> {
        Some(Endpoint {
            address: self.host_ip.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
            port: self.public_port?,
            protocol: self.protocol,
            pid: None,
        })
    }
}

/// Identity of a container, derived from its name and its Compose labels.
///
/// The Compose labels are what makes identity survive `docker compose up`
/// recreating the container: the container id changes, the name may change, but
/// `project/service` does not. Without those labels the container name is all
/// there is, so a plain `docker run --rm` container that is recreated under a
/// different name is a different service — a real limitation, not a bug.
///
/// The container id is used as the name only if Docker reported no names at
/// all, which keeps identity non-empty at the cost of stability.
pub fn container_identity(
    id: &str,
    names: &[String],
    labels: Option<&HashMap<String, String>>,
) -> ContainerIdentity {
    let name = names
        .iter()
        // Docker prefixes names with `/` for historical reasons.
        .map(|name| name.trim_start_matches('/').trim())
        .find(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| id.to_owned());

    let label = |key: &str| {
        labels
            .and_then(|labels| labels.get(key))
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };

    ContainerIdentity {
        name,
        compose_project: label(COMPOSE_PROJECT_LABEL),
        compose_service: label(COMPOSE_SERVICE_LABEL),
    }
}

/// One container as observed at a point in time.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ObservedContainer {
    /// Full container id. Changes when the container is recreated, so it is
    /// diagnostic data only and never an identity input.
    pub id: String,
    pub identity: ContainerIdentity,
    /// Image reference as the container was created with. Empty when Docker
    /// did not report one.
    pub image: String,
    pub state: ContainerState,
    /// Docker's human-readable status line (`Up 3 hours`, `Exited (0) 1 min
    /// ago`). Passed through for display; never parsed.
    pub status: String,
    pub ports: Vec<ContainerPort>,
    /// Docker networks the container is attached to, sorted.
    pub networks: Vec<String>,
    /// `None` unless stats collection is enabled
    /// ([`with_stats`](crate::BollardCollector::with_stats)). `None` means "not
    /// measured", not "zero".
    pub cpu_percent: Option<f32>,
    /// See [`ObservedContainer::cpu_percent`].
    pub memory_bytes: Option<u64>,
}

impl ObservedContainer {
    /// Map a container list entry. Every field Docker omits degrades to an
    /// empty value or `None` instead of failing the whole snapshot: one odd
    /// container must not blind DevPulse to the rest.
    pub fn from_summary(summary: &ContainerSummary) -> Self {
        let id = summary.id.as_deref().unwrap_or_default();

        Self {
            id: id.to_owned(),
            identity: container_identity(
                id,
                summary.names.as_deref().unwrap_or_default(),
                summary.labels.as_ref(),
            ),
            image: summary.image.clone().unwrap_or_default(),
            state: summary
                .state
                .map(|state| ContainerState::from_docker_str(state_wire_str(state)))
                .unwrap_or(ContainerState::Unknown),
            status: summary.status.clone().unwrap_or_default(),
            ports: summary
                .ports
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter_map(ContainerPort::from_summary)
                .collect(),
            networks: summary
                .network_settings
                .as_ref()
                .and_then(|settings| settings.networks.as_ref())
                // Sorted and deduplicated so that two snapshots of an
                // unchanged container compare equal; `HashMap` order does not.
                .map(|networks| networks.keys().cloned().collect::<BTreeSet<_>>())
                .map(|networks| networks.into_iter().collect())
                .unwrap_or_default(),
            cpu_percent: None,
            memory_bytes: None,
        }
    }

    /// Normalise into the shared [`Service`] shape so that containers and host
    /// processes reach the graph through one interface (`TASKS.md` T6.3).
    ///
    /// `instances` is always empty: the container list does not disclose the
    /// host PIDs of the container's processes, so there is no honest
    /// [`ProcessInstance`](devpulse_core::ProcessInstance) to report. For a
    /// container, `health` is the liveness signal, not the instance count.
    pub fn to_service(&self, project_id: Option<ProjectId>, at: SystemTime) -> Service {
        let fingerprint = ServiceFingerprint::container(&self.identity);

        Service {
            id: fingerprint.service_id(),
            project_id,
            // The Compose service name is what the developer wrote in the
            // compose file, so it beats the name Docker generated.
            name: self
                .identity
                .compose_service
                .clone()
                .unwrap_or_else(|| self.identity.name.clone()),
            kind: ServiceKind::Container(self.identity.clone()),
            runtime: Runtime::Container,
            fingerprint: fingerprint.canonical().to_owned(),
            health: self.state.health(),
            instances: Vec::new(),
            endpoints: self
                .ports
                .iter()
                .filter_map(ContainerPort::endpoint)
                .collect(),
            first_seen: at,
            last_seen: at,
            // The registry owns restart counting; a single observation cannot
            // know about restarts.
            restart_count: 0,
        }
    }
}

/// A full container-table observation.
#[derive(Debug, Clone, Serialize)]
pub struct ContainerSnapshot {
    pub captured_at: SystemTime,
    /// How long the collection took, so the daemon can see its own cost
    /// (`AGENTS.md` rule 7).
    pub duration: Duration,
    /// Containers in every state, sorted by identity name.
    pub containers: Vec<ObservedContainer>,
}

impl ContainerSnapshot {
    pub fn running(&self) -> impl Iterator<Item = &ObservedContainer> {
        self.containers
            .iter()
            .filter(|container| container.state == ContainerState::Running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn compose_labels(project: &str, service: &str) -> HashMap<String, String> {
        labels(&[
            (COMPOSE_PROJECT_LABEL, project),
            (COMPOSE_SERVICE_LABEL, service),
        ])
    }

    fn summary(id: &str, name: &str, state: ContainerSummaryStateEnum) -> ContainerSummary {
        ContainerSummary {
            id: Some(id.to_owned()),
            names: Some(vec![format!("/{name}")]),
            image: Some("postgres:16".to_owned()),
            state: Some(state),
            status: Some("Up 2 minutes".to_owned()),
            ..Default::default()
        }
    }

    #[test]
    fn strips_the_leading_slash_from_the_container_name() {
        let identity = container_identity("abc123", &["/shop-db-1".to_owned()], None);

        assert_eq!(identity.name, "shop-db-1");
        assert_eq!(identity.compose_project, None);
        assert_eq!(identity.compose_service, None);
    }

    #[test]
    fn reads_compose_labels_into_the_identity() {
        let identity = container_identity(
            "abc123",
            &["/shop-db-1".to_owned()],
            Some(&compose_labels("shop", "db")),
        );

        assert_eq!(identity.compose_project.as_deref(), Some("shop"));
        assert_eq!(identity.compose_service.as_deref(), Some("db"));
    }

    #[test]
    fn ignores_blank_compose_labels() {
        let identity = container_identity(
            "abc123",
            &["/shop-db-1".to_owned()],
            Some(&compose_labels("  ", "")),
        );

        assert_eq!(identity.compose_project, None, "blank label is not a value");
        assert_eq!(identity.compose_service, None);
    }

    #[test]
    fn falls_back_to_the_container_id_when_docker_reports_no_name() {
        let identity = container_identity("abc123", &[], None);

        assert_eq!(identity.name, "abc123");
    }

    #[test]
    fn compose_identity_survives_container_recreation() {
        // `docker compose up` after an image change: new id, new name suffix,
        // same compose labels.
        let before = container_identity(
            "1111111111",
            &["/shop-db-1".to_owned()],
            Some(&compose_labels("shop", "db")),
        );
        let after = container_identity(
            "2222222222",
            &["/shop-db-2".to_owned()],
            Some(&compose_labels("shop", "db")),
        );

        let id_before = ServiceFingerprint::container(&before).service_id();
        let id_after = ServiceFingerprint::container(&after).service_id();

        assert_eq!(
            id_before, id_after,
            "compose labels must keep the service identity across recreation"
        );
    }

    #[test]
    fn identity_without_compose_labels_does_not_survive_a_rename() {
        let before = container_identity("1111111111", &["/shop-db-1".to_owned()], None);
        let after = container_identity("2222222222", &["/shop-db-2".to_owned()], None);

        assert_ne!(
            ServiceFingerprint::container(&before).service_id(),
            ServiceFingerprint::container(&after).service_id(),
            "without compose labels the name is the only identity input"
        );
    }

    #[test]
    fn two_compose_services_in_one_project_stay_distinct() {
        let db = container_identity(
            "a",
            &["/shop-db-1".to_owned()],
            Some(&compose_labels("shop", "db")),
        );
        let api = container_identity(
            "b",
            &["/shop-api-1".to_owned()],
            Some(&compose_labels("shop", "api")),
        );

        assert_ne!(
            ServiceFingerprint::container(&db).service_id(),
            ServiceFingerprint::container(&api).service_id()
        );
    }

    #[test]
    fn maps_known_docker_states() {
        for (input, expected) in [
            ("running", ContainerState::Running),
            ("created", ContainerState::Created),
            ("restarting", ContainerState::Restarting),
            ("paused", ContainerState::Paused),
            ("exited", ContainerState::Exited),
            ("dead", ContainerState::Dead),
            ("RUNNING", ContainerState::Running),
            (" running ", ContainerState::Running),
        ] {
            assert_eq!(
                ContainerState::from_docker_str(input),
                expected,
                "state {input:?}"
            );
        }
    }

    #[test]
    fn maps_unrecognised_docker_states_to_unknown() {
        for input in ["", "removing", "stopping", "restarting-soon", "zombie"] {
            assert_eq!(
                ContainerState::from_docker_str(input),
                ContainerState::Unknown,
                "state {input:?} must not be guessed"
            );
        }
    }

    #[test]
    fn derives_health_from_state() {
        assert_eq!(ContainerState::Running.health(), Health::Healthy);
        assert_eq!(ContainerState::Restarting.health(), Health::Degraded);
        assert_eq!(ContainerState::Exited.health(), Health::Stopped);
        assert_eq!(ContainerState::Dead.health(), Health::Stopped);
        assert_eq!(ContainerState::Created.health(), Health::Unknown);
        assert_eq!(ContainerState::Paused.health(), Health::Unknown);
        assert_eq!(ContainerState::Unknown.health(), Health::Unknown);
    }

    #[test]
    fn maps_a_published_tcp_port() {
        let port = ContainerPort::from_summary(&PortSummary {
            ip: Some("127.0.0.1".to_owned()),
            private_port: 5432,
            public_port: Some(55432),
            typ: Some(PortSummaryTypeEnum::TCP),
        })
        .expect("tcp port is mappable");

        assert_eq!(port.private_port, 5432);
        assert_eq!(port.public_port, Some(55432));
        assert_eq!(port.protocol, Protocol::Tcp);
        assert_eq!(port.host_ip, Some(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(port.is_published());
    }

    #[test]
    fn treats_a_missing_or_empty_port_type_as_tcp() {
        for typ in [None, Some(PortSummaryTypeEnum::EMPTY)] {
            let port = ContainerPort::from_summary(&PortSummary {
                private_port: 80,
                typ,
                ..Default::default()
            })
            .expect("port is mappable");

            assert_eq!(port.protocol, Protocol::Tcp);
        }
    }

    #[test]
    fn maps_udp_and_drops_sctp() {
        let udp = ContainerPort::from_summary(&PortSummary {
            private_port: 53,
            public_port: Some(53),
            typ: Some(PortSummaryTypeEnum::UDP),
            ..Default::default()
        })
        .expect("udp port is mappable");
        assert_eq!(udp.protocol, Protocol::Udp);

        assert_eq!(
            ContainerPort::from_summary(&PortSummary {
                private_port: 132,
                typ: Some(PortSummaryTypeEnum::SCTP),
                ..Default::default()
            }),
            None,
            "sctp has no domain protocol and must not be reported as tcp"
        );
    }

    #[test]
    fn ignores_an_unparseable_host_ip_without_losing_the_port() {
        let port = ContainerPort::from_summary(&PortSummary {
            ip: Some("not-an-ip".to_owned()),
            private_port: 8080,
            public_port: Some(8080),
            typ: Some(PortSummaryTypeEnum::TCP),
        })
        .expect("port is still mappable");

        assert_eq!(port.host_ip, None);
        assert_eq!(port.public_port, Some(8080));
    }

    #[test]
    fn unpublished_ports_are_not_endpoints() {
        let internal = ContainerPort {
            private_port: 5432,
            public_port: None,
            protocol: Protocol::Tcp,
            host_ip: None,
        };

        assert_eq!(internal.endpoint(), None);
        assert!(!internal.is_published());
    }

    #[test]
    fn a_published_port_without_a_host_ip_binds_every_interface() {
        let published = ContainerPort {
            private_port: 5432,
            public_port: Some(55432),
            protocol: Protocol::Tcp,
            host_ip: None,
        };

        let endpoint = published.endpoint().expect("published port is an endpoint");
        assert_eq!(endpoint.address, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(endpoint.port, 55432);
        assert_eq!(endpoint.pid, None, "host pid is not disclosed by docker");
    }

    #[test]
    fn observes_a_compose_container_from_a_summary() {
        let mut raw = summary("abc123", "shop-db-1", ContainerSummaryStateEnum::RUNNING);
        raw.labels = Some(compose_labels("shop", "db"));
        raw.ports = Some(vec![PortSummary {
            ip: Some("0.0.0.0".to_owned()),
            private_port: 5432,
            public_port: Some(55432),
            typ: Some(PortSummaryTypeEnum::TCP),
        }]);
        raw.network_settings = Some(bollard::models::ContainerSummaryNetworkSettings {
            networks: Some(
                [
                    ("shop_default".to_owned(), Default::default()),
                    ("bridge".to_owned(), Default::default()),
                ]
                .into_iter()
                .collect(),
            ),
        });

        let observed = ObservedContainer::from_summary(&raw);

        assert_eq!(observed.id, "abc123");
        assert_eq!(observed.identity.name, "shop-db-1");
        assert_eq!(observed.identity.compose_service.as_deref(), Some("db"));
        assert_eq!(observed.image, "postgres:16");
        assert_eq!(observed.state, ContainerState::Running);
        assert_eq!(observed.status, "Up 2 minutes");
        assert_eq!(observed.ports.len(), 1);
        assert_eq!(
            observed.networks,
            vec!["bridge".to_owned(), "shop_default".to_owned()],
            "networks are sorted so unchanged snapshots compare equal"
        );
        assert_eq!(observed.cpu_percent, None, "stats are off by default");
        assert_eq!(observed.memory_bytes, None);
    }

    #[test]
    fn a_summary_missing_everything_still_maps() {
        let observed = ObservedContainer::from_summary(&ContainerSummary::default());

        assert_eq!(observed.id, "");
        assert_eq!(observed.identity.name, "");
        assert_eq!(observed.image, "");
        assert_eq!(observed.state, ContainerState::Unknown);
        assert!(observed.ports.is_empty());
        assert!(observed.networks.is_empty());
    }

    #[test]
    fn maps_removing_state_from_a_summary_to_unknown() {
        let observed = ObservedContainer::from_summary(&summary(
            "abc",
            "x",
            ContainerSummaryStateEnum::REMOVING,
        ));

        assert_eq!(observed.state, ContainerState::Unknown);
    }

    #[test]
    fn to_service_prefers_the_compose_service_name() {
        let mut raw = summary("abc123", "shop-db-1", ContainerSummaryStateEnum::RUNNING);
        raw.labels = Some(compose_labels("shop", "db"));
        let observed = ObservedContainer::from_summary(&raw);

        let service = observed.to_service(None, SystemTime::UNIX_EPOCH);

        assert_eq!(service.name, "db");
        assert_eq!(service.runtime, Runtime::Container);
        assert_eq!(service.health, Health::Healthy);
        assert_eq!(
            service.kind,
            ServiceKind::Container(observed.identity.clone())
        );
        assert_eq!(service.restart_count, 0);
        assert!(
            service.instances.is_empty(),
            "docker does not disclose the host pid, so there is no instance to report"
        );
        assert_eq!(service.fingerprint, "container|shop|db");
        assert_eq!(
            service.id,
            ServiceFingerprint::container(&observed.identity).service_id()
        );
    }

    #[test]
    fn to_service_falls_back_to_the_container_name() {
        let observed = ObservedContainer::from_summary(&summary(
            "abc123",
            "lonely-redis",
            ContainerSummaryStateEnum::EXITED,
        ));

        let service = observed.to_service(None, SystemTime::UNIX_EPOCH);

        assert_eq!(service.name, "lonely-redis");
        assert_eq!(service.health, Health::Stopped);
        assert_eq!(service.fingerprint, "container|lonely-redis");
    }

    #[test]
    fn to_service_publishes_only_host_reachable_endpoints() {
        let mut raw = summary("abc123", "shop-db-1", ContainerSummaryStateEnum::RUNNING);
        raw.ports = Some(vec![
            PortSummary {
                ip: Some("127.0.0.1".to_owned()),
                private_port: 5432,
                public_port: Some(55432),
                typ: Some(PortSummaryTypeEnum::TCP),
            },
            // Exposed inside the compose network only.
            PortSummary {
                private_port: 9187,
                public_port: None,
                typ: Some(PortSummaryTypeEnum::TCP),
                ip: None,
            },
        ]);

        let service =
            ObservedContainer::from_summary(&raw).to_service(None, SystemTime::UNIX_EPOCH);

        assert_eq!(
            service.endpoints.len(),
            1,
            "only published ports are endpoints"
        );
        assert_eq!(service.primary_port(), Some(55432));
        assert_eq!(
            service.endpoints[0].address,
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
    }

    #[test]
    fn to_service_carries_the_project_id_it_is_given() {
        let project = ProjectId::derived("/Users/dev/code/shop");
        let observed = ObservedContainer::from_summary(&summary(
            "a",
            "db",
            ContainerSummaryStateEnum::RUNNING,
        ));

        let service = observed.to_service(Some(project.clone()), SystemTime::UNIX_EPOCH);

        assert_eq!(service.project_id, Some(project));
    }

    #[test]
    fn a_containerised_service_never_collides_with_a_host_process() {
        use std::path::Path;

        let project = ProjectId::derived("/Users/dev/code/shop");
        let mut raw = summary("a", "shop-db-1", ContainerSummaryStateEnum::RUNNING);
        raw.labels = Some(compose_labels("shop", "db"));
        raw.ports = Some(vec![PortSummary {
            ip: None,
            private_port: 5432,
            public_port: Some(5432),
            typ: Some(PortSummaryTypeEnum::TCP),
        }]);

        let container = ObservedContainer::from_summary(&raw)
            .to_service(Some(project.clone()), SystemTime::UNIX_EPOCH);

        let host = ServiceFingerprint::host(
            Some(&project),
            Runtime::Native,
            Some(Path::new("/usr/local/bin/postgres")),
            Some(Path::new("/Users/dev/code/shop")),
            Some(5432),
        );

        assert_ne!(container.id, host.service_id());
        assert_ne!(container.fingerprint, host.canonical());
    }

    #[test]
    fn snapshot_running_filters_by_state() {
        let snapshot = ContainerSnapshot {
            captured_at: SystemTime::UNIX_EPOCH,
            duration: Duration::from_millis(3),
            containers: vec![
                ObservedContainer::from_summary(&summary(
                    "a",
                    "up",
                    ContainerSummaryStateEnum::RUNNING,
                )),
                ObservedContainer::from_summary(&summary(
                    "b",
                    "down",
                    ContainerSummaryStateEnum::EXITED,
                )),
            ],
        };

        let running: Vec<_> = snapshot
            .running()
            .map(|c| c.identity.name.as_str())
            .collect();
        assert_eq!(running, vec!["up"]);
    }
}
