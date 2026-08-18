//! Docker daemon detection (task T6.1).
//!
//! Most developer machines that run Runscape have Docker; plenty do not, and on
//! Linux a fresh install often has a socket the user may not read. All three are
//! normal, so detection returns a *state*, never an error the daemon has to
//! decide what to do with.

use bollard::Docker;
use bollard::errors::Error;

use crate::collector::BollardCollector;

/// Whether Docker can be inspected at all.
///
/// [`DockerAvailability::Available`] is the only way to obtain a
/// [`BollardCollector`], so a collector cannot exist without a daemon that
/// answered a ping.
#[derive(Debug)]
pub enum DockerAvailability {
    Available(BollardCollector),
    /// Docker is absent, stopped, or unreachable. `reason` is written for a
    /// developer to read in the UI or in a log line and is never empty.
    Unavailable {
        reason: String,
    },
}

impl DockerAvailability {
    /// Connect to the local Docker daemon and ping it.
    ///
    /// Creating a client does almost no work — on Unix it just remembers a
    /// socket path — so the ping is what actually decides availability. This
    /// never returns an error and never panics: a machine without Docker is a
    /// supported machine.
    pub async fn detect() -> Self {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(docker) => docker,
            Err(error) => return Self::unavailable(&error),
        };

        match docker.ping().await {
            Ok(_) => Self::Available(BollardCollector::new(docker)),
            Err(error) => Self::unavailable(&error),
        }
    }

    fn unavailable(error: &Error) -> Self {
        Self::Unavailable {
            reason: describe(error),
        }
    }

    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }

    /// The reason Docker cannot be inspected, if it cannot.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Available(_) => None,
            Self::Unavailable { reason } => Some(reason),
        }
    }

    pub fn collector(&self) -> Option<&BollardCollector> {
        match self {
            Self::Available(collector) => Some(collector),
            Self::Unavailable { .. } => None,
        }
    }

    pub fn into_collector(self) -> Option<BollardCollector> {
        match self {
            Self::Available(collector) => Some(collector),
            Self::Unavailable { .. } => None,
        }
    }
}

/// Turn a Bollard error into something a developer can act on.
///
/// The interesting cases are I/O failures, and they arrive either directly or
/// wrapped in whichever HTTP client Bollard used, so those are unwrapped before
/// the variant is matched.
fn describe(error: &Error) -> String {
    if let Some(io) = io_cause(error) {
        return match io.kind() {
            std::io::ErrorKind::NotFound => {
                "the Docker socket does not exist; Docker is not installed or not running"
                    .to_owned()
            }
            std::io::ErrorKind::PermissionDenied => {
                "permission denied on the Docker socket; grant this user access to Docker"
                    .to_owned()
            }
            std::io::ErrorKind::ConnectionRefused => {
                "the Docker socket refused the connection; the Docker daemon is not running"
                    .to_owned()
            }
            std::io::ErrorKind::TimedOut => {
                "the Docker daemon did not answer in time; it may still be starting".to_owned()
            }
            _ => format!("cannot reach the Docker daemon: {io}"),
        };
    }

    match error {
        Error::SocketNotFoundError(path) => format!(
            "the Docker socket {path} does not exist; Docker is not installed or not running"
        ),
        Error::DockerResponseServerError {
            status_code,
            message,
        } => format!("the Docker daemon refused the request with status {status_code}: {message}"),
        Error::RequestTimeoutError => {
            "the Docker daemon did not answer in time; it may still be starting".to_owned()
        }
        other => format!("cannot reach the Docker daemon: {other}"),
    }
}

/// First `std::io::Error` behind a Bollard error.
///
/// `Error::IOError` is declared `#[error(transparent)]`, which makes its
/// `source()` skip the io error and return *its* source — so the direct case
/// has to be matched, and only the wrapped cases are worth walking.
fn io_cause(error: &Error) -> Option<&std::io::Error> {
    if let Error::IOError { err } = error {
        return Some(err);
    }

    let mut current = std::error::Error::source(error);
    while let Some(error) = current {
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            return Some(io);
        }
        current = error.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn io_error(kind: std::io::ErrorKind) -> Error {
        Error::IOError {
            err: std::io::Error::new(kind, "boom"),
        }
    }

    #[test]
    fn a_missing_socket_is_explained_not_reported_as_a_failure() {
        let reason = describe(&Error::SocketNotFoundError(
            "/var/run/docker.sock".to_owned(),
        ));

        assert!(reason.contains("/var/run/docker.sock"), "{reason}");
        assert!(reason.contains("not installed"), "{reason}");
    }

    #[test]
    fn permission_denied_tells_the_developer_what_to_fix() {
        let reason = describe(&io_error(std::io::ErrorKind::PermissionDenied));

        assert!(reason.contains("permission denied"), "{reason}");
        assert!(reason.contains("access to Docker"), "{reason}");
    }

    #[test]
    fn a_refused_connection_names_a_stopped_daemon() {
        let reason = describe(&io_error(std::io::ErrorKind::ConnectionRefused));

        assert!(reason.contains("not running"), "{reason}");
    }

    #[test]
    fn a_server_error_keeps_the_status_and_message() {
        let reason = describe(&Error::DockerResponseServerError {
            status_code: 503,
            message: "server is shutting down".to_owned(),
        });

        assert!(reason.contains("503"), "{reason}");
        assert!(reason.contains("shutting down"), "{reason}");
    }

    #[test]
    fn every_reason_is_non_empty() {
        let errors = [
            Error::SocketNotFoundError(String::new()),
            io_error(std::io::ErrorKind::NotFound),
            io_error(std::io::ErrorKind::PermissionDenied),
            io_error(std::io::ErrorKind::ConnectionRefused),
            io_error(std::io::ErrorKind::TimedOut),
            io_error(std::io::ErrorKind::BrokenPipe),
            Error::RequestTimeoutError,
            Error::APIVersionParseError {},
        ];

        for error in &errors {
            let reason = describe(error);
            assert!(!reason.trim().is_empty(), "empty reason for {error:?}");
        }
    }

    #[test]
    fn finds_the_io_error_behind_a_transparent_variant() {
        let direct = io_error(std::io::ErrorKind::PermissionDenied);

        assert_eq!(
            io_cause(&direct).map(|io| io.kind()),
            Some(std::io::ErrorKind::PermissionDenied)
        );
        assert!(io_cause(&Error::RequestTimeoutError).is_none());
    }

    #[test]
    fn unavailable_exposes_its_reason_and_no_collector() {
        let availability = DockerAvailability::unavailable(&Error::RequestTimeoutError);

        assert!(!availability.is_available());
        assert!(availability.collector().is_none());
        assert!(availability.reason().is_some_and(|r| !r.is_empty()));
        assert!(availability.into_collector().is_none());
    }
}
