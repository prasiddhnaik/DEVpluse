//! Collector failures.

/// A collector failed to produce a snapshot at all.
///
/// Partial visibility (a process whose `cwd` cannot be read, a socket whose
/// owner belongs to another user) is *not* an error: it is recorded on the
/// snapshot instead, so the daemon keeps running with reduced confidence.
#[derive(Debug, thiserror::Error)]
pub enum CollectorError {
    /// The platform API refused or failed.
    #[error("{collector} collector failed: {source}")]
    Platform {
        collector: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The blocking worker running the collector panicked or was cancelled.
    #[error("{collector} collector task failed: {source}")]
    Join {
        collector: &'static str,
        #[source]
        source: tokio::task::JoinError,
    },

    /// This platform has no implementation for the collector.
    #[error("{collector} collector is not supported on {os}")]
    Unsupported {
        collector: &'static str,
        os: &'static str,
    },
}

impl CollectorError {
    pub(crate) fn platform<E>(collector: &'static str, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Platform {
            collector,
            source: Box::new(source),
        }
    }
}
