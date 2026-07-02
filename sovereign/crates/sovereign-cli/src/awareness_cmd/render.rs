// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared output formatters for `svrn awareness` subcommands.
//!
//! Status symbols mirror `enrich_cmd/status.rs:52-57` (`✓ ⚠ ·`); the
//! `--json` toggle pattern mirrors `enrich_cmd/errors.rs:479-482`
//! (presence-only flag, no `=value`; emit pretty JSON when set,
//! plain text otherwise).

/// Format a Unix epoch second as `YYYY-MM-DD`. Falls back to a
/// dash when the timestamp is missing or unparseable. Used by
/// every entity/timeline header — keeping it here means none of
/// the subcommand modules pull in chrono separately.
pub(super) fn format_date(unix_seconds: Option<i64>) -> String {
    match unix_seconds {
        Some(s) => chrono::DateTime::<chrono::Utc>::from_timestamp(s, 0)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "—".to_string()),
        None => "—".to_string(),
    }
}

/// Format a Unix epoch second as `YYYY-MM-DD HH:MM`. Used by
/// timeline rows where the time-of-day matters.
pub(super) fn format_datetime(unix_seconds: Option<i64>) -> String {
    match unix_seconds {
        Some(s) => chrono::DateTime::<chrono::Utc>::from_timestamp(s, 0)
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "—".to_string()),
        None => "—".to_string(),
    }
}

/// Days since a timestamp (positive int) — "(11 days old)" style
/// annotations for outstanding commitments and overdue follow-ups.
/// Clamps to zero when `now` is earlier than `unix_seconds`.
pub(super) fn days_since(unix_seconds: i64, now: i64) -> i64 {
    let delta = (now - unix_seconds).max(0);
    delta / 86_400
}

/// `~/.sovereign/...` style path display. Folds `$HOME` so error
/// messages don't leak the absolute path of the user's home dir.
pub(super) fn display_path(p: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rel) = p.strip_prefix(&home) {
            return format!("~/{}", rel.display());
        }
    }
    p.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_date_handles_none_and_zero() {
        assert_eq!(format_date(None), "—");
        // Epoch is 1970-01-01.
        assert_eq!(format_date(Some(0)), "1970-01-01");
    }

    #[test]
    fn days_since_returns_positive_diff() {
        // 86_400 seconds = 1 day.
        assert_eq!(days_since(0, 86_400), 1);
        assert_eq!(days_since(0, 86_400 * 11), 11);
        // Now-before-then clamps to zero rather than going negative.
        assert_eq!(days_since(86_400, 0), 0);
    }
}
