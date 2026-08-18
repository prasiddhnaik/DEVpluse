//! Secret redaction for process command lines.
//!
//! `AGENTS.md` rule 6 forbids collecting environment values and requires that
//! likely secrets in process arguments are redacted. Runscape redacts at
//! capture time rather than at render time, so an unredacted argument can never
//! reach storage or the API by accident.
//!
//! The rules are intentionally conservative-in-the-safe-direction: it is better
//! to redact a harmless argument than to leak a credential. Redaction is
//! lossy and is not reversible.

use std::sync::LazyLock;

use regex::Regex;

/// Replacement text substituted for any value considered sensitive.
pub const REDACTED: &str = "<redacted>";

/// Flag / key names whose *value* must never be retained.
///
/// Matches an optional `-`/`--` prefix, an optional chain of qualifier words
/// (`AWS_SECRET_`, `--db-`, `DATABASE_`), then a sensitive tail word. Anchoring
/// the tail on a word boundary keeps unrelated names such as `--monkey` or
/// `RUST_LOG` untouched.
static SENSITIVE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        ^ [-]{0,2}
        ( [a-z0-9]+ [-_] )*
        (
            pass (word | wd | phrase)?
          | secrets?
          | tokens?
          | keys?
          | apikey
          | credentials?
          | cred
          | auth (orization)?
          | bearer
          | dsn
          | conn (ection)? [-_]? string
        )
        $",
    )
    .expect("SENSITIVE_KEY is a valid regex")
});

/// Credentials embedded in a URL: `scheme://user:password@host`.
static URL_CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?P<prefix>[a-zA-Z][a-zA-Z0-9+.\-]*://[^\s:/@]+):(?P<secret>[^\s@/]+)@")
        .expect("URL_CREDENTIALS is a valid regex")
});

/// Well-known credential shapes that are recognisable without a key name.
static TOKEN_SHAPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
          sk-[A-Za-z0-9_\-]{16,}
        | gh[pousr]_[A-Za-z0-9]{16,}
        | github_pat_[A-Za-z0-9_]{20,}
        | AKIA[0-9A-Z]{16}
        | ASIA[0-9A-Z]{16}
        | xox[abprs]-[A-Za-z0-9\-]{10,}
        | eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}
        ",
    )
    .expect("TOKEN_SHAPE is a valid regex")
});

/// Redact a whole command line.
///
/// Handles four shapes:
///
/// 1. `--password=hunter2`     -> `--password=<redacted>`
/// 2. `--password hunter2`     -> `--password <redacted>`
/// 3. `DATABASE_TOKEN=hunter2` -> `DATABASE_TOKEN=<redacted>`
/// 4. free-standing values that look like credentials (`sk-…`, `ghp_…`,
///    JWTs, AWS access key ids, `postgres://user:pw@host`).
pub fn redact_command(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut redact_next = false;

    for arg in args {
        if redact_next {
            redact_next = false;
            // A value-looking token is redacted; a following flag is not, since
            // that means the sensitive flag was a boolean.
            if !is_flag(arg) {
                out.push(REDACTED.to_string());
                continue;
            }
        }

        if let Some((key, _value)) = split_assignment(arg)
            && is_sensitive_key(key)
        {
            out.push(format!("{key}={REDACTED}"));
            continue;
        }

        if is_flag(arg) && is_sensitive_key(arg) {
            redact_next = true;
            out.push(arg.clone());
            continue;
        }

        out.push(redact_value(arg));
    }

    out
}

/// Redact credential-shaped substrings inside a single value.
pub fn redact_value(value: &str) -> String {
    let masked = URL_CREDENTIALS.replace_all(value, format!("$prefix:{REDACTED}@").as_str());
    TOKEN_SHAPE.replace_all(&masked, REDACTED).into_owned()
}

fn is_flag(arg: &str) -> bool {
    arg.starts_with('-') && arg.len() > 1
}

fn is_sensitive_key(key: &str) -> bool {
    SENSITIVE_KEY.is_match(key)
}

/// Split `key=value`, returning `None` when there is no `=` or no key.
fn split_assignment(arg: &str) -> Option<(&str, &str)> {
    let (key, value) = arg.split_once('=')?;
    if key.is_empty() {
        return None;
    }
    Some((key, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redact(args: &[&str]) -> Vec<String> {
        redact_command(&args.iter().map(|a| (*a).to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn keeps_ordinary_arguments_intact() {
        assert_eq!(
            redact(&["node", "server.js", "--port", "3000", "--host=127.0.0.1"]),
            vec!["node", "server.js", "--port", "3000", "--host=127.0.0.1"]
        );
    }

    #[test]
    fn redacts_inline_assignment() {
        assert_eq!(
            redact(&["api", "--password=hunter2"]),
            vec!["api", "--password=<redacted>"]
        );
    }

    #[test]
    fn redacts_separated_flag_value() {
        assert_eq!(
            redact(&["api", "--api-key", "abc123", "--port", "8080"]),
            vec!["api", "--api-key", "<redacted>", "--port", "8080"]
        );
    }

    #[test]
    fn boolean_sensitive_flag_does_not_swallow_next_flag() {
        assert_eq!(
            redact(&["api", "--auth", "--port", "8080"]),
            vec!["api", "--auth", "--port", "8080"]
        );
    }

    #[test]
    fn redacts_env_style_argument() {
        assert_eq!(
            redact(&["DATABASE_TOKEN=zzz", "PORT=8080"]),
            vec!["DATABASE_TOKEN=<redacted>", "PORT=8080"]
        );
    }

    #[test]
    fn redacts_url_credentials_but_keeps_host() {
        assert_eq!(
            redact(&["psql", "postgres://app:s3cr3t@localhost:5432/dev"]),
            vec!["psql", "postgres://app:<redacted>@localhost:5432/dev"]
        );
    }

    #[test]
    fn redacts_known_token_shapes() {
        let out = redact(&[
            "curl",
            "sk-abcdefghijklmnopqrstuvwx",
            "ghp_abcdefghijklmnopqrstuvwxyz0123",
            "AKIAIOSFODNN7EXAMPLE",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        ]);
        assert_eq!(out[0], "curl");
        assert!(
            out[1..].iter().all(|a| a == REDACTED),
            "expected all tokens redacted, got {out:?}"
        );
    }

    #[test]
    fn keeps_non_secret_assignment_values() {
        assert_eq!(
            redact(&["RUST_LOG=debug", "--config=/etc/app.toml"]),
            vec!["RUST_LOG=debug", "--config=/etc/app.toml"]
        );
    }
}
