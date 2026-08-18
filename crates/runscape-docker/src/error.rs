//! Docker failures that the daemon actually has to handle.
//!
//! Docker being absent, stopped, or refusing the socket is *not* one of these:
//! that is the normal state of a machine without Docker and is reported as
//! [`DockerAvailability::Unavailable`](crate::DockerAvailability::Unavailable)
//! instead (`TASKS.md` T6.1). A [`DockerError`] means Runscape had a working
//! connection and the API call itself went wrong, which is worth logging.

/// A Docker API call failed against a daemon that was reachable at detection
/// time.
#[derive(Debug, thiserror::Error)]
pub enum DockerError {
    /// The daemon returned an error, or the connection died mid-call (the
    /// daemon was stopped while Runscape was running, for instance).
    #[error("docker api call failed: {source}")]
    Api {
        #[from]
        source: bollard::errors::Error,
    },
}
