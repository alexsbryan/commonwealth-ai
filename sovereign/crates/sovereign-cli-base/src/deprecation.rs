// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deprecation banner for renamed CLI surfaces.
//!
//! The CLI refactor collapses `svrn project *` / `svrn atos *`
//! / `svrn tools *` into a flat `svrn *` namespace. Old names keep working forever —
//! they dispatch to the new modules and print a one-time-per-process
//! banner pointing at the new name.
//!
//! Usage from a leaf subcommand handler:
//!
//! ```ignore
//! use crate::util::deprecation;
//! deprecation::announce("svrn project init", "svrn init");
//! // …continue with normal behaviour…
//! ```
//!
//! Suppression: set `SOVEREIGN_QUIET_DEPRECATIONS=1` in the
//! environment to silence every banner. We respect any non-empty
//! value other than `0` / `false` (case-insensitive) as "quiet."
//!
//! Once-per-process: each (`old`, `new`) pair prints at most once
//! per process. Repeated calls with the same pair are no-ops. A
//! cold start fires the banner again — by design, so a user who
//! restarts a shell sees the hint until they switch.
//!
//! The banner is emitted on `stderr` so it doesn't pollute pipes
//! when scripts parse the command's stdout.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::OnceLock;

/// Emit the banner for a renamed command — once per process. Safe to
/// call from any thread.
///
/// `old` is what the user typed; `new` is what they should type next
/// time. Both should include the leading `sovereign` and any
/// subcommand chain so the banner is copy-pastable.
pub fn announce(old: &str, new: &str) {
    if is_quiet() {
        return;
    }
    if !record_first_time(old, new) {
        return;
    }
    eprintln!();
    eprintln!("  note: `{old}` is now `{new}` (old name still works).");
    eprintln!("        Set SOVEREIGN_QUIET_DEPRECATIONS=1 to silence.");
    eprintln!();
}

/// Variant for commands that have been fully retired with no
/// drop-in replacement. Prints the banner regardless of suppression
/// (we won't silently swallow a retired command — but the helpful
/// pointer text obeys quiet mode). The caller is expected to either
/// no-op or proceed; this function does not exit.
pub fn announce_retired(old: &str, hint: &str) {
    eprintln!();
    eprintln!("  note: `{old}` has been retired.");
    if !is_quiet() {
        eprintln!("        {hint}");
    }
    eprintln!();
}

fn is_quiet() -> bool {
    match std::env::var("SOVEREIGN_QUIET_DEPRECATIONS") {
        Ok(v) => {
            let trimmed = v.trim().to_ascii_lowercase();
            !matches!(trimmed.as_str(), "" | "0" | "false" | "no" | "off")
        }
        Err(_) => false,
    }
}

/// Insert into the once-set; return `true` iff this is the first
/// time we've seen this `(old, new)` pair in the current process.
fn record_first_time(old: &str, new: &str) -> bool {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let lock = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    // Mutex poisoning here means another thread crashed mid-insert,
    // which we treat the same as "we already announced": the banner
    // is best-effort, never load-bearing. Recover the inner set so
    // subsequent calls don't double-fire.
    let mut set = lock.lock().unwrap_or_else(|p| p.into_inner());
    let key = format!("{old}\u{1f}{new}");
    set.insert(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `record_first_time` is the only piece we can test directly
    /// (the banner writes to stderr; capturing that across threads
    /// is fragile and not load-bearing here). Same `(old, new)` is
    /// idempotent; different pairs are independent.
    #[test]
    fn record_first_time_is_idempotent_per_pair() {
        // We deliberately use unusual strings so test ordering with
        // other tests in this process doesn't collide.
        assert!(record_first_time("test:cmd-x", "test:cmd-y"));
        assert!(!record_first_time("test:cmd-x", "test:cmd-y"));
        assert!(record_first_time("test:cmd-x", "test:cmd-z"));
    }

    /// Quiet-mode parsing accepts the conventional disabling values.
    #[test]
    fn quiet_mode_parsing_recognises_falsy_values() {
        for v in ["", "0", "false", "FALSE", "no", "off", "NO"] {
            // SAFETY: tests run sequentially per process and we
            // restore env state after; remaining writes happen on
            // the same key. This is a CLI utility test, not a
            // multi-threaded harness.
            unsafe { std::env::set_var("SOVEREIGN_QUIET_DEPRECATIONS", v) };
            assert!(!is_quiet(), "value {v:?} should not enable quiet");
        }
        for v in ["1", "true", "yes", "on", "anything-non-empty"] {
            unsafe { std::env::set_var("SOVEREIGN_QUIET_DEPRECATIONS", v) };
            assert!(is_quiet(), "value {v:?} should enable quiet");
        }
        unsafe { std::env::remove_var("SOVEREIGN_QUIET_DEPRECATIONS") };
        assert!(!is_quiet());
    }
}
