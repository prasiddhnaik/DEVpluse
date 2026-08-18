//! Conversions between domain values and the handful of column types SQLite
//! actually has.
//!
//! # Time
//!
//! Timestamps are stored as signed Unix milliseconds. Milliseconds because the
//! timeline is the consumer and nobody scrolls a timeline at nanosecond
//! resolution; *signed* because a clock that is briefly wrong (or a fixture
//! that constructs a pre-epoch time) must not be able to panic the daemon, and
//! `SystemTime::duration_since` returns an error rather than a negative value.
//! The cost is honest and bounded: sub-millisecond precision does not survive a
//! round trip.
//!
//! # Enums
//!
//! Enum-shaped values go through serde rather than a hand-written table, so
//! adding a `Health` or `EventKind` variant needs no change here — a
//! hand-written `from_str` would compile happily and then fail at read time on
//! the new variant, six months from now, in someone else's database.
//!
//! Serde unit variants are stored as the bare tag (`healthy`) instead of a
//! quoted JSON string (`"healthy"`) so the file stays inspectable with plain
//! SQL; structured variants are stored as JSON. Decoding tells the two apart by
//! looking at the first byte, which is unambiguous because a serde tag is never
//! a valid JSON document.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::StorageError;

/// Encode a timestamp as signed Unix milliseconds.
///
/// Saturates instead of panicking: the extremes are ±292 million years, which
/// no real observation reaches, so clamping there cannot hide a plausible bug.
pub(crate) fn to_millis(at: SystemTime) -> i64 {
    match at.duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_millis()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_millis())
            .map(|millis| -millis)
            .unwrap_or(i64::MIN),
    }
}

/// Decode signed Unix milliseconds. Falls back to the epoch if the platform
/// cannot represent the value, so a corrupt row degrades a timestamp instead of
/// aborting a read.
pub(crate) fn from_millis(millis: i64) -> SystemTime {
    let offset = Duration::from_millis(millis.unsigned_abs());
    let shifted = if millis >= 0 {
        UNIX_EPOCH.checked_add(offset)
    } else {
        UNIX_EPOCH.checked_sub(offset)
    };
    shifted.unwrap_or(UNIX_EPOCH)
}

/// SQLite integers are signed 64-bit. Memory readings saturate at 8 EiB, which
/// is not a number any process will report.
pub(crate) fn bytes_to_sql(bytes: u64) -> i64 {
    i64::try_from(bytes).unwrap_or(i64::MAX)
}

/// A negative byte count can only come from a hand-edited file; read it as zero
/// rather than wrapping into an absurd value.
pub(crate) fn bytes_from_sql(bytes: i64) -> u64 {
    u64::try_from(bytes).unwrap_or(0)
}

/// Row counts and limits: `rusqlite` binds no `usize`, and a limit above
/// `i64::MAX` is nobody's literal intent, so saturate rather than fail a query.
pub(crate) fn count_to_sql(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

/// Encode an enum-shaped value for a TEXT column. See the module docs for the
/// bare-tag versus JSON rule.
pub(crate) fn encode<T: Serialize>(value: &T) -> Result<String, StorageError> {
    Ok(match serde_json::to_value(value)? {
        Value::String(tag) => tag,
        structured => structured.to_string(),
    })
}

/// Inverse of [`encode`].
pub(crate) fn decode<T: DeserializeOwned>(text: &str) -> Result<T, StorageError> {
    let value = match text.as_bytes().first() {
        // A JSON document; anything else is a bare serde tag.
        Some(b'{' | b'[' | b'"') => serde_json::from_str(text)?,
        _ => Value::String(text.to_owned()),
    };
    Ok(serde_json::from_value(value)?)
}

#[cfg(test)]
mod tests {
    use devpulse_core::model::{EventKind, Health, ServiceKind};
    use devpulse_core::{ContainerIdentity, ProjectId, Runtime};

    use super::*;

    #[test]
    fn millis_round_trip_across_the_epoch() {
        for millis in [0, 1, 1_700_000_000_123, -1, -86_400_000] {
            assert_eq!(to_millis(from_millis(millis)), millis, "millis {millis}");
        }
    }

    #[test]
    fn pre_epoch_times_do_not_panic() {
        let before = UNIX_EPOCH - Duration::from_secs(60 * 60 * 24 * 365);
        assert_eq!(to_millis(before), -31_536_000_000);
        assert_eq!(from_millis(to_millis(before)), before);
    }

    #[test]
    fn extreme_times_saturate_instead_of_panicking() {
        let far = UNIX_EPOCH + Duration::from_secs(u64::MAX / 1000);
        assert_eq!(to_millis(far), i64::MAX);
    }

    #[test]
    fn sub_millisecond_precision_is_truncated_not_rounded() {
        let at = UNIX_EPOCH + Duration::from_micros(1_999);
        assert_eq!(to_millis(at), 1);
    }

    #[test]
    fn unit_variants_are_stored_as_bare_tags() {
        assert_eq!(encode(&Health::Degraded).expect("encode"), "degraded");
        assert_eq!(encode(&Runtime::Node).expect("encode"), "node");
        assert_eq!(
            decode::<Health>("degraded").expect("decode"),
            Health::Degraded
        );
    }

    #[test]
    fn structured_variants_are_stored_as_json() {
        let kind = ServiceKind::Container(ContainerIdentity {
            name: "api".into(),
            compose_project: Some("shop".into()),
            compose_service: Some("api".into()),
        });
        let text = encode(&kind).expect("encode");
        assert!(text.starts_with('{'), "expected JSON, got {text}");
        assert_eq!(decode::<ServiceKind>(&text).expect("decode"), kind);
    }

    #[test]
    fn event_kinds_survive_the_round_trip() {
        let kind = EventKind::FileChanged {
            project_id: ProjectId::derived("/tmp/shop"),
            path: "/tmp/shop/src/main.rs".into(),
        };
        let text = encode(&kind).expect("encode");
        assert_eq!(decode::<EventKind>(&text).expect("decode"), kind);
    }

    #[test]
    fn byte_counts_saturate_rather_than_wrap() {
        assert_eq!(bytes_to_sql(u64::MAX), i64::MAX);
        assert_eq!(bytes_from_sql(-1), 0);
        assert_eq!(bytes_from_sql(bytes_to_sql(4096)), 4096);
    }
}
