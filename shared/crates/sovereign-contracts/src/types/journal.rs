// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local journals — append-only, on the developer's own disk, and never
//! sent anywhere.
//!
//! # What a journal is here
//!
//! A **stream** of JSONL day-files under `<branded root>/journal/`, one
//! stream per feature that wants a local record of how it behaved on
//! real work. This module is the machinery only: where files live, when
//! they rotate, when they stop growing, how they are switched off, and
//! how lines are appended and read back. It knows nothing about any
//! feature's record shape.
//!
//! The first stream is next-edit
//! ([`crate::types::next_edit_journal`]). It will not be the last, which
//! is why this split exists: `svrn journal` is a generic verb, and the
//! machinery under it has no business being next-edit-shaped.
//!
//! # Adding a stream
//!
//! Three things, and no edit to this file:
//!
//! 1. A `const STREAM: JournalStream = JournalStream::new("<stem>",
//!    "SOVEREIGN_<X>_JOURNAL")` in your feature's module.
//! 2. Serde types for your lines. If a line can be superseded by later
//!    knowledge (an outcome arriving minutes after the event), make it
//!    TWO line kinds joined by an id rather than one mutable line —
//!    append-only files cannot rewrite history.
//! 3. A row in `svrn journal`'s view registry so it can be read,
//!    bundled, and switched off like the others. A stream with no view
//!    is a stream the developer cannot audit.
//!
//! # The rules that are NOT negotiable per stream
//!
//! - **No code in a journal line.** Enforce it structurally in your
//!   record type — no `serde_json::Value`, no free-form string field —
//!   rather than by remembering. This module cannot check it for you: it
//!   takes anything `Serialize`.
//! - **No network path.** Nothing here reads or writes a socket, and
//!   sharing is an explicit hand-back (`svrn journal bundle`) that
//!   prints what it contains. A stream that phones home is not a
//!   journal.
//! - **Off must mean off, without a restart.** Honour
//!   [`JournalStream::enabled`], which is the ONE decider for every
//!   switch (global and per-stream, env and marker file).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Env var that disables EVERY journal stream for a process (`off` / `0`
/// / `false`). Per-stream env vars are named by each
/// [`JournalStream::disable_env`]. Declared in `quality/env-flags.toml`.
pub const JOURNAL_ENV: &str = "SOVEREIGN_JOURNAL";

/// Env var overriding the journal directory. Tests and operators only.
pub const JOURNAL_DIR_ENV: &str = "SOVEREIGN_JOURNAL_DIR";

/// Presence of this file in the journal directory disables every
/// stream. A file rather than a config key so `svrn journal off` takes
/// effect on the daemon's next write with no restart and no IPC. A
/// single stream is disabled by `<stem>.disabled` beside it.
pub const DISABLED_MARKER: &str = "DISABLED";

/// Per-file byte cap. Past this the day's file stops growing (one
/// `tracing::warn!`, then silence) rather than filling a disk on behalf
/// of a feature nobody asked to measure.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Days of history retained. Pruning happens on the first write of a new
/// day, so the common path costs no extra syscalls.
pub const KEEP_DAYS: i64 = 14;

/// The journal directory: `$SOVEREIGN_JOURNAL_DIR` when set, else the
/// branded per-user root's `journal/`.
///
/// The default comes from [`crate::rebrand::journal_dir`] rather than
/// from `dirs::home_dir()` here — per-user paths have exactly one
/// derivation in this workspace so the `~/.svrnmesh` rebrand and its
/// legacy fallback cannot be missed by a hand-rolled join (clippy's
/// path-SSOT ban, `clippy.toml`).
pub fn journal_dir() -> PathBuf {
    if let Ok(dir) = std::env::var(JOURNAL_DIR_ENV) {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    crate::rebrand::journal_dir()
}

/// Whether an env var's value reads as "off".
fn env_says_off(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        ),
        Err(_) => false,
    }
}

/// One append-only journal stream: its file naming, its switches, and
/// its IO. Construct one `const` per feature.
///
/// Deliberately a plain descriptor rather than a trait: every stream
/// wants identical behaviour from this layer and differs only in its
/// record types, which live in the caller's generic parameter. A trait
/// here would invite per-stream overrides of exactly the rules — the
/// cap, the retention, the off-switch — that must not vary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalStream {
    /// File-name stem: `<stem>-<YYYY-MM-DD>.jsonl`. Filename-safe,
    /// stable across releases (it is how old days are found).
    pub stem: &'static str,
    /// Env var that disables THIS stream, in addition to [`JOURNAL_ENV`]
    /// which disables all of them.
    pub disable_env: &'static str,
}

impl JournalStream {
    /// Declare a stream. `const` so a feature module can own its
    /// descriptor with no initialization order to think about.
    pub const fn new(stem: &'static str, disable_env: &'static str) -> Self {
        Self { stem, disable_env }
    }

    /// Today's file, named by UTC date so a day's records stay together
    /// regardless of the reader's timezone.
    pub fn path_in(&self, dir: &Path) -> PathBuf {
        dir.join(format!(
            "{}-{}.jsonl",
            self.stem,
            chrono::Utc::now().format("%Y-%m-%d")
        ))
    }

    /// The per-stream disable marker, beside the global one.
    pub fn marker_in(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.disabled", self.stem))
    }

    /// Whether to write at all. ONE decider, consulted by every writer:
    /// the global env switch, the global marker file, this stream's env
    /// switch, and this stream's marker file. Any of the four says no.
    pub fn enabled(&self, dir: &Path) -> bool {
        !env_says_off(JOURNAL_ENV)
            && !env_says_off(self.disable_env)
            && !dir.join(DISABLED_MARKER).exists()
            && !self.marker_in(dir).exists()
    }

    /// Append one serialized line.
    ///
    /// Returns `Ok(false)` when the stream is switched off or the day's
    /// file is at cap — those are postures, not errors. Errors are real
    /// IO failures and are the caller's to report; a daemon-side wrapper
    /// is expected to swallow them into a `tracing::warn!` because a
    /// journal failure must never become a user-facing failure.
    pub fn append<T: Serialize>(&self, dir: &Path, line: &T) -> std::io::Result<bool> {
        if !self.enabled(dir) {
            return Ok(false);
        }
        std::fs::create_dir_all(dir)?;
        let path = self.path_in(dir);
        // A new day's file is the cheap, self-scheduling moment to drop
        // old ones — no timer, no daemon task, one `read_dir` a day.
        let fresh_day = !path.exists();
        let mut body = serde_json::to_string(line).map_err(std::io::Error::other)?;
        body.push('\n');

        let existing = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if existing >= MAX_FILE_BYTES {
            tracing::warn!(
                target: "journal",
                stream = self.stem,
                path = %path.display(),
                bytes = existing,
                cap = MAX_FILE_BYTES,
                "journal file at cap; dropping this record"
            );
            return Ok(false);
        }

        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        // One `write_all` of one line, on a file opened O_APPEND:
        // concurrent tasks interleave whole lines rather than fragments.
        f.write_all(body.as_bytes())?;

        if fresh_day {
            self.prune(dir, KEEP_DAYS);
        }
        Ok(true)
    }

    /// Every line of this stream, oldest day first, parsed as `T`.
    ///
    /// The second element counts lines that could NOT be parsed. They
    /// are skipped, never guessed at: a truncated tail (daemon killed
    /// mid-write) or a line from a future schema must not silently
    /// become a zero-valued record.
    pub fn read_all<T: DeserializeOwned>(&self, dir: &Path) -> (Vec<T>, usize) {
        let (raw, mut unreadable) = self.read_raw(dir);
        let mut out = Vec::with_capacity(raw.len());
        for line in raw {
            match serde_json::from_str::<T>(&line) {
                Ok(v) => out.push(v),
                Err(_) => unreadable += 1,
            }
        }
        (out, unreadable)
    }

    /// Every line as its original text, oldest day first, plus a count
    /// of unreadable FILES. Used by `svrn journal bundle`, which must
    /// ship and audit the exact bytes rather than a re-serialization.
    pub fn read_raw(&self, dir: &Path) -> (Vec<String>, usize) {
        let mut files: Vec<(chrono::NaiveDate, PathBuf)> = Vec::new();
        let Ok(rd) = std::fs::read_dir(dir) else {
            return (Vec::new(), 0);
        };
        for entry in rd.flatten() {
            let name = entry.file_name();
            if let Some(day) = name.to_str().and_then(|n| self.day_of(n)) {
                files.push((day, entry.path()));
            }
        }
        files.sort();

        let mut out = Vec::new();
        let mut unreadable = 0usize;
        for (_, path) in files {
            match std::fs::read_to_string(&path) {
                Ok(text) => out.extend(
                    text.lines()
                        .filter(|l| !l.trim().is_empty())
                        .map(str::to_string),
                ),
                Err(_) => unreadable += 1,
            }
        }
        (out, unreadable)
    }

    /// Delete this stream's day-files older than `keep_days`. Best
    /// effort and silent on individual failures — a journal that cannot
    /// prune is still a working journal. Never touches a file that is
    /// not one of ours.
    pub fn prune(&self, dir: &Path, keep_days: i64) {
        let cutoff = chrono::Utc::now().date_naive() - chrono::Duration::days(keep_days);
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(day) = self.day_of(name) else {
                continue;
            };
            if day < cutoff {
                let _ = std::fs::remove_file(entry.path());
                tracing::debug!(target: "journal", stream = self.stem, file = name, "pruned journal day");
            }
        }
    }

    /// Delete every day-file of this stream. Returns how many went.
    pub fn clear(&self, dir: &Path) -> usize {
        let mut removed = 0usize;
        let Ok(rd) = std::fs::read_dir(dir) else {
            return 0;
        };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if self.day_of(name).is_some() && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// The date encoded in a filename, or `None` when the name is not
    /// one of this stream's.
    fn day_of(&self, name: &str) -> Option<chrono::NaiveDate> {
        let day = name
            .strip_prefix(self.stem)?
            .strip_prefix('-')?
            .strip_suffix(".jsonl")?;
        chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    const A: JournalStream = JournalStream::new("stream-a", "SOVEREIGN_TEST_A_JOURNAL");
    const B: JournalStream = JournalStream::new("stream-b", "SOVEREIGN_TEST_B_JOURNAL");

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Line {
        n: u32,
    }

    fn line(n: u32) -> Line {
        Line { n }
    }

    #[test]
    fn append_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert!(A.append(dir.path(), &line(1)).unwrap());
        assert!(A.append(dir.path(), &line(2)).unwrap());
        let (lines, bad) = A.read_all::<Line>(dir.path());
        assert_eq!(bad, 0);
        assert_eq!(lines, vec![line(1), line(2)]);
    }

    /// Streams must not read each other's files — the whole point of the
    /// stem is that `svrn journal` can host more than one feature.
    #[test]
    fn streams_are_isolated_in_one_directory() {
        let dir = tempfile::tempdir().unwrap();
        A.append(dir.path(), &line(1)).unwrap();
        B.append(dir.path(), &line(99)).unwrap();
        assert_eq!(A.read_all::<Line>(dir.path()).0, vec![line(1)]);
        assert_eq!(B.read_all::<Line>(dir.path()).0, vec![line(99)]);
    }

    #[test]
    fn the_global_marker_disables_every_stream() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join(DISABLED_MARKER), "").unwrap();
        assert!(!A.enabled(dir.path()));
        assert!(!B.enabled(dir.path()));
        assert!(
            !A.append(dir.path(), &line(1)).unwrap(),
            "off is Ok(false), not an error"
        );
    }

    #[test]
    fn a_per_stream_marker_disables_only_that_stream() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(A.marker_in(dir.path()), "").unwrap();
        assert!(!A.enabled(dir.path()));
        assert!(
            B.enabled(dir.path()),
            "disabling one stream must not disable its neighbours"
        );
    }

    #[test]
    fn truncated_tail_is_counted_not_guessed() {
        let dir = tempfile::tempdir().unwrap();
        A.append(dir.path(), &line(1)).unwrap();
        let path = A.path_in(dir.path());
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"n\":\n").unwrap();
        let (lines, bad) = A.read_all::<Line>(dir.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(
            bad, 1,
            "a half-written line must be reported, not read as zeroes"
        );
    }

    #[test]
    fn prune_drops_old_days_keeps_recent_and_spares_other_streams() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        let old = chrono::Utc::now().date_naive() - chrono::Duration::days(30);
        let recent = chrono::Utc::now().date_naive() - chrono::Duration::days(2);
        for d in [old, recent] {
            std::fs::write(dir.path().join(format!("stream-a-{d}.jsonl")), "{}\n").unwrap();
            std::fs::write(dir.path().join(format!("stream-b-{d}.jsonl")), "{}\n").unwrap();
        }
        std::fs::write(dir.path().join("unrelated.txt"), "x").unwrap();
        A.prune(dir.path(), KEEP_DAYS);
        assert!(!dir.path().join(format!("stream-a-{old}.jsonl")).exists());
        assert!(dir.path().join(format!("stream-a-{recent}.jsonl")).exists());
        assert!(
            dir.path().join(format!("stream-b-{old}.jsonl")).exists(),
            "pruning one stream must not touch another's history"
        );
        assert!(
            dir.path().join("unrelated.txt").exists(),
            "prune must not touch foreign files"
        );
    }

    #[test]
    fn file_at_cap_stops_growing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            A.path_in(dir.path()),
            vec![b'x'; MAX_FILE_BYTES as usize + 1],
        )
        .unwrap();
        assert!(!A.append(dir.path(), &line(1)).unwrap());
    }

    #[test]
    fn clear_removes_only_this_streams_days() {
        let dir = tempfile::tempdir().unwrap();
        A.append(dir.path(), &line(1)).unwrap();
        B.append(dir.path(), &line(2)).unwrap();
        assert_eq!(A.clear(dir.path()), 1);
        assert_eq!(A.read_all::<Line>(dir.path()).0.len(), 0);
        assert_eq!(B.read_all::<Line>(dir.path()).0.len(), 1);
    }

    #[test]
    fn a_foreign_filename_is_never_mistaken_for_a_day_file() {
        for name in [
            "stream-a.jsonl",              // no date
            "stream-a-2026-13-45.jsonl",   // not a date
            "stream-abc-2026-08-07.jsonl", // different stem
            "2026-08-07.jsonl",
            "stream-a-2026-08-07.json",
        ] {
            assert!(
                A.day_of(name).is_none(),
                "`{name}` must not parse as a stream-a day"
            );
        }
        assert!(A.day_of("stream-a-2026-08-07.jsonl").is_some());
    }
}
