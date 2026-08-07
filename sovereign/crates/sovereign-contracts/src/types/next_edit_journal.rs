// SPDX-License-Identifier: AGPL-3.0-or-later
//! The next-edit journal — local, metadata-only evidence about how the
//! editing lane actually behaves on a developer's own machine
//! (`sovereign/docs/NEXT_EDIT.md` §10).
//!
//! # Why this exists
//!
//! Next-edit suggestion is handed to developers to *use*, and their
//! experience is the evidence. Today the only trace of an episode is a
//! `tracing::info!` line that vanishes with the daemon's log buffer, and
//! nothing at all joins a suggestion to what the developer did with it.
//! "Is this feature any good on real work?" is unanswerable from logs.
//!
//! So an episode becomes a **record**, and the record gets a joinable
//! identity:
//!
//! - one [`NextEditEpisode`] per `POST /v1/edit_predictions`, carrying
//!   why the lane fired or stayed silent, which slot answered, and how
//!   long it took;
//! - one [`NextEditOutcomeLine`] per episode the editor reports on,
//!   joined by `episode_id`.
//!
//! Two lines, not one mutated line: an append-only JSONL cannot rewrite
//! history, and the outcome arrives seconds to minutes after the
//! episode. This is the same decision→outcome join
//! `sovereign_mesh::decision_log` makes for routing, for the same
//! reason.
//!
//! # What it deliberately does not carry
//!
//! **No code.** Not the document, not the region, not the needle, not
//! the rule's find/replace strings, not the proposed rewrite, not the
//! file path. That is not a convention to remember — it is enforced by
//! the type: [`NextEditEpisode`] has no `serde_json::Value` field and no
//! free-form string field, so there is no channel through which a
//! code-bearing value could reach the file even if a caller tried to
//! pass one (ARCH §7). The route builds it from named scalars; the file
//! can only contain what the struct declares.
//!
//! Code-bearing episodes exist, but only **in memory** and only for the
//! last few episodes, for the developer who chooses to attach them to a
//! report (`svrn journal attach --last`). They are a different type and
//! they never pass through [`append`].
//!
//! # Where it goes, and where it does not
//!
//! `<journal dir>/next-edit-<YYYY-MM-DD>.jsonl`, on the developer's own
//! disk, and nowhere else. **There is no network path out of this
//! module** — no upload, no submit, no phone-home. Sharing is an
//! explicit, audited hand-back: `svrn journal bundle` writes one file
//! and prints exactly what is inside it, and the developer decides what
//! to do with it.
//!
//! # What lives here vs. in [`crate::types::journal`]
//!
//! This module is ONLY the next-edit vocabulary: the record shapes, the
//! closed outcome set, and the counting rules below. Where files live,
//! when they rotate, when they stop growing, and how they are switched
//! off is generic machinery in [`crate::types::journal`], shared by every
//! stream — `svrn journal` is a generic verb and the layer under it is
//! not next-edit-shaped. To add another stream, follow that module's
//! "Adding a stream"; you should not need to touch this file.
//!
//! # Reading the counts honestly
//!
//! Four outcomes plus absence, and the absence is counted (ARCH §18.1's
//! four verdicts, in this lane's vocabulary):
//!
//! | this lane | §18.1 |
//! |---|---|
//! | `accepted` | passed |
//! | `dismissed` | failed |
//! | `diverged` / `superseded` | could-not-judge |
//! | no outcome line at all | never-ran |
//!
//! [`JournalStats::acceptance_rate`] is therefore computed over
//! `accepted + dismissed` ONLY, and is `None` when that is zero.
//! `diverged` is not a rejection — the developer typed on, which says
//! nothing about whether the suggestion was good — and folding it (or
//! the un-reported episodes) into `dismissed` would yield an acceptance
//! rate that looks precise and is wrong. That is this system's
//! characteristic failure mode and the whole reason the outcome set is
//! four-way.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::journal::JournalStream;

/// This lane's stream: `next-edit-<date>.jsonl`, disabled on its own by
/// `SOVEREIGN_NEXT_EDIT_JOURNAL=off` (or globally, or by a marker file —
/// [`JournalStream::enabled`] is the one decider).
pub const NEXT_EDIT_STREAM: JournalStream =
    JournalStream::new("next-edit", "SOVEREIGN_NEXT_EDIT_JOURNAL");

/// Schema tag stamped on every line. Bump on any backwards-incompatible
/// change: the reader skips lines it does not understand rather than
/// silently mis-reading old fields into new meanings.
pub const NEXT_EDIT_JOURNAL_SCHEMA: &str = "next-edit-journal/v1";

// ---------------------------------------------------------------
// Record model
// ---------------------------------------------------------------

/// One line of the journal. Tagged so both halves of the
/// episode→outcome join can be appended to the same stream, interleaved
/// in whatever order they actually happen.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalLine {
    /// One prediction request the daemon served.
    Episode(NextEditEpisode),
    /// What the editor reported back about one of them.
    Outcome(NextEditOutcomeLine),
}

/// What one `POST /v1/edit_predictions` did.
///
/// Every field is a scalar or a closed-set string the daemon chose from
/// its own vocabulary. There is no field a document, a region, an
/// identifier from the user's code, or a file path can travel through —
/// see the module docs. Adding one would be the review that has to
/// justify it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NextEditEpisode {
    /// See [`NEXT_EDIT_JOURNAL_SCHEMA`].
    pub schema: String,
    /// RFC 3339, UTC.
    pub ts: String,
    /// Joins this episode to its outcome line. Random per episode
    /// (never a counter — ARCH §7.5).
    pub episode_id: String,

    // ── what the lane did ──
    /// `rule` or `model` — which lane produced the returned edits.
    pub engine: String,
    /// `engine == "model"`. Denormalized because every stats query
    /// wants it and re-deriving it from a string in two places is two
    /// implementations of one predicate (ARCH §10.6).
    pub fired: bool,
    /// Edits actually returned to the editor. `0` is a silent episode.
    pub proposed: usize,
    /// Induction support behind the rule.
    pub support: usize,
    /// Remaining candidate sites the rule matched.
    pub sites: usize,
    /// Rule-lane silence reason (`no_rule`, `below_threshold`,
    /// `no_sites`, …), absent when the rule lane had something to say.
    pub silent: Option<String>,

    // ── which model, if any ──
    /// The advertised model id that answered (gguf file stem).
    pub model_id: Option<String>,
    /// Which slot served — `edit` for a dedicated pinned extra, or the
    /// fast slot's name in alias mode.
    pub slot: Option<String>,
    /// The prompt dialect (`region_instruct`, `zeta2`, `sweep`).
    pub format: Option<String>,
    /// `true` when the slot is the fallback rather than a configured
    /// `[models.edit]`. The one field that tells a fallback episode from
    /// a specialist one, which is the distinction the whole
    /// `SOVEREIGN_NEXT_EDIT_FALLBACK` ledger row turns on.
    pub degraded: Option<bool>,
    /// Whether the consult suppressed the model's thinking phase.
    /// Recorded because unsuppressed reasoning scores 0/30 on this lane
    /// rather than merely scoring worse, and a truncated-empty result is
    /// indistinguishable from a model with nothing to say.
    pub suppress_thinking: Option<bool>,

    // ── why the model was or was not consulted ──
    /// Consult-gate reason when the gate said yes.
    pub reason: Option<String>,
    /// Gate refusal reason (`consulted: false`).
    pub skipped: Option<String>,
    /// Consulted, but the output did not survive (`timeout`, `busy`,
    /// `unavailable`, `malformed`, `verify_failed`, …).
    pub dropped: Option<String>,
    /// Size of the editable region sent to the model. A length, never
    /// the contents.
    pub region_bytes: Option<u64>,

    // ── context, at the coarsest grain that is still useful ──
    /// Editor-declared language id.
    pub language: Option<String>,
    /// File EXTENSION only (`rs`, `tsx`, `go`) — never the path, never
    /// the filename.
    pub path_ext: Option<String>,

    // ── cost ──
    /// Wall time for the whole request, as the editor experienced it.
    pub total_ms: u64,
    /// Of which, time inside the model. Absent when no consult happened.
    pub inference_ms: Option<u64>,
}

/// What the developer did with one episode's suggestion.
///
/// The set is closed and deliberately has no `unknown` variant: an
/// episode nobody reported on is the ABSENCE of a line, and absence is
/// counted at read time ([`JournalStats::unknown`]). A wire that could
/// say "unknown" would let a client turn a never-ran into a verdict
/// (ARCH §18.3).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NextEditOutcome {
    /// The edit applied.
    Accepted,
    /// Esc, or an explicit dismiss.
    Dismissed,
    /// The document moved under the prediction before it could be
    /// applied. NOT a rejection.
    Diverged,
    /// A newer prediction replaced this one.
    Superseded,
}

impl NextEditOutcome {
    /// The wire spelling. One implementation, shared by the route's
    /// parser and the stats renderer.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Dismissed => "dismissed",
            Self::Diverged => "diverged",
            Self::Superseded => "superseded",
        }
    }

    /// Parse a wire value. `None` for anything else — the route answers
    /// 400 rather than picking a default, because a mis-spelled outcome
    /// silently counted as `dismissed` would corrupt the one number this
    /// journal exists to produce (ARCH §18.3).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "accepted" => Some(Self::Accepted),
            "dismissed" => Some(Self::Dismissed),
            "diverged" => Some(Self::Diverged),
            "superseded" => Some(Self::Superseded),
            _ => None,
        }
    }

    /// Every variant, for help text and exhaustive tests.
    pub const ALL: [NextEditOutcome; 4] =
        [Self::Accepted, Self::Dismissed, Self::Diverged, Self::Superseded];
}

/// The outcome half of the join.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NextEditOutcomeLine {
    /// See [`NEXT_EDIT_JOURNAL_SCHEMA`].
    pub schema: String,
    /// RFC 3339, UTC — when the EDITOR resolved the episode, which can
    /// be minutes after the episode itself.
    pub ts: String,
    /// The [`NextEditEpisode::episode_id`] this resolves.
    pub episode_id: String,
    /// What the developer did.
    pub outcome: NextEditOutcome,
}

impl NextEditOutcomeLine {
    /// Stamp an outcome for `episode_id` at the current time.
    pub fn new(episode_id: String, outcome: NextEditOutcome) -> Self {
        Self {
            schema: NEXT_EDIT_JOURNAL_SCHEMA.to_string(),
            ts: chrono::Utc::now().to_rfc3339(),
            episode_id,
            outcome,
        }
    }
}

/// Builder for an episode record. Exists so the route can fill fields as
/// it learns them without a 20-argument constructor, and so `episode_id`
/// and `schema` are set in exactly one place.
impl NextEditEpisode {
    /// A fresh episode with a random id. The caller fills the rest.
    pub fn new(engine: &str, proposed: usize, total_ms: u64) -> Self {
        Self {
            schema: NEXT_EDIT_JOURNAL_SCHEMA.to_string(),
            ts: chrono::Utc::now().to_rfc3339(),
            episode_id: uuid::Uuid::new_v4().to_string(),
            engine: engine.to_string(),
            fired: engine == "model",
            proposed,
            support: 0,
            sites: 0,
            silent: None,
            model_id: None,
            slot: None,
            format: None,
            degraded: None,
            suppress_thinking: None,
            reason: None,
            skipped: None,
            dropped: None,
            region_bytes: None,
            language: None,
            path_ext: None,
            total_ms,
            inference_ms: None,
        }
    }

    /// The file extension of a path, lowercased — the ONLY thing this
    /// record takes from a path. A single accessor so no caller is
    /// tempted to store `path` "just for debugging" (ARCH §7.5).
    pub fn ext_of(path: Option<&str>) -> Option<String> {
        Path::new(path?).extension()?.to_str().map(|e| e.to_lowercase())
    }
}

// ---------------------------------------------------------------
// Reading
// ---------------------------------------------------------------

/// Every episode + outcome line, oldest day first, plus a count of
/// lines that could not be parsed.
///
/// A one-line wrapper over [`JournalStream::read_all`] so callers name
/// this lane rather than repeating its stream constant — the machinery
/// (file discovery, ordering, skip-and-count) is generic and lives in
/// [`super::journal`].
pub fn read_all(dir: &Path) -> (Vec<JournalLine>, usize) {
    NEXT_EDIT_STREAM.read_all(dir)
}

/// Append one line to this lane's stream. See
/// [`JournalStream::append`] for the `Ok(false)` postures.
pub fn append(dir: &Path, line: &JournalLine) -> std::io::Result<bool> {
    NEXT_EDIT_STREAM.append(dir, line)
}

/// Joined counts over a set of lines. Read the field docs before quoting
/// a rate — the honesty rules are in the field, not in the caller.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JournalStats {
    /// Episodes recorded.
    pub episodes: usize,
    /// Episodes that returned at least one edit — i.e. the developer
    /// was actually shown something. The denominator for "did this
    /// feature do anything".
    pub shown: usize,
    /// Episodes served by the model lane.
    pub fired: usize,
    /// Episodes where the model was consulted and its answer dropped.
    pub dropped: usize,
    /// Episodes on a fallback (`degraded`) slot rather than a
    /// configured edit model.
    pub degraded: usize,
    /// The edit applied — the one outcome that says the suggestion was
    /// wanted.
    pub accepted: usize,
    /// Esc or explicit dismiss — the only other judgement of quality.
    pub dismissed: usize,
    /// The document moved first. NOT a rejection, and never added to
    /// `dismissed`.
    pub diverged: usize,
    /// Replaced by a newer prediction. Also not a judgement.
    pub superseded: usize,
    /// Shown episodes with NO outcome line — editor closed, session
    /// garbage-collected, daemon restarted, or an older extension that
    /// does not report. Counted, never folded into `dismissed`; a large
    /// `unknown` is the signal that the rate below is unrepresentative.
    pub unknown: usize,
    /// Outcome lines whose `episode_id` matches no episode in this
    /// window (an outcome for a pruned day, usually). Reported so the
    /// join's losses are visible rather than absorbed.
    pub orphan_outcomes: usize,
    /// Lines that could not be parsed.
    pub unreadable: usize,
    /// Median `total_ms` over all episodes.
    pub p50_ms: u64,
    /// p95 `total_ms` over all episodes — the number a developer
    /// actually feels, and the one the GM5 latency gate is written
    /// against.
    pub p95_ms: u64,
}

impl JournalStats {
    /// `accepted / (accepted + dismissed)` — the only two outcomes that
    /// are a judgement of the suggestion. `None` when nothing has been
    /// judged, which is a *could-not-judge*, not a 0%.
    pub fn acceptance_rate(&self) -> Option<f64> {
        let judged = self.accepted + self.dismissed;
        (judged > 0).then(|| self.accepted as f64 / judged as f64)
    }

    /// How much of the shown population actually reported. Quote this
    /// next to any acceptance rate — a rate over 4 of 900 episodes is
    /// not a measurement (ARCH §18.5).
    pub fn reported_coverage(&self) -> Option<f64> {
        let reported = self.accepted + self.dismissed + self.diverged + self.superseded;
        (self.shown > 0).then(|| reported as f64 / self.shown as f64)
    }
}

/// Join episodes to outcomes and count. `unreadable` comes from
/// [`read_all`] and is threaded through rather than recomputed.
pub fn stats(lines: &[JournalLine], unreadable: usize) -> JournalStats {
    use std::collections::{HashMap, HashSet};

    let mut s = JournalStats { unreadable, ..Default::default() };
    let mut shown_ids: HashSet<&str> = HashSet::new();
    let mut all_ids: HashSet<&str> = HashSet::new();
    let mut outcomes: HashMap<&str, NextEditOutcome> = HashMap::new();
    let mut durations: Vec<u64> = Vec::new();

    for line in lines {
        match line {
            JournalLine::Episode(e) => {
                s.episodes += 1;
                all_ids.insert(&e.episode_id);
                if e.proposed > 0 {
                    s.shown += 1;
                    shown_ids.insert(&e.episode_id);
                }
                if e.fired {
                    s.fired += 1;
                }
                if e.dropped.is_some() {
                    s.dropped += 1;
                }
                if e.degraded == Some(true) {
                    s.degraded += 1;
                }
                durations.push(e.total_ms);
            }
            JournalLine::Outcome(o) => {
                // Last write wins: a session that diverged and was then
                // superseded reports twice, and the terminal state is
                // the later one.
                outcomes.insert(&o.episode_id, o.outcome);
            }
        }
    }

    for (id, outcome) in &outcomes {
        if !all_ids.contains(*id) {
            s.orphan_outcomes += 1;
            continue;
        }
        match outcome {
            NextEditOutcome::Accepted => s.accepted += 1,
            NextEditOutcome::Dismissed => s.dismissed += 1,
            NextEditOutcome::Diverged => s.diverged += 1,
            NextEditOutcome::Superseded => s.superseded += 1,
        }
    }
    s.unknown = shown_ids.iter().filter(|id| !outcomes.contains_key(**id)).count();

    durations.sort_unstable();
    if !durations.is_empty() {
        s.p50_ms = durations[durations.len() / 2];
        s.p95_ms = durations[(durations.len() * 95 / 100).min(durations.len() - 1)];
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(id: &str, proposed: usize, ms: u64) -> JournalLine {
        let mut e = NextEditEpisode::new("model", proposed, ms);
        e.episode_id = id.to_string();
        JournalLine::Episode(e)
    }

    fn oc(id: &str, o: NextEditOutcome) -> JournalLine {
        JournalLine::Outcome(NextEditOutcomeLine::new(id.to_string(), o))
    }

    /// THE guard this module exists for: nothing code-bearing can reach
    /// a journal line, because the record has nowhere to put it. If
    /// someone adds a free-form field, this test is what fails — the
    /// canary appears in the serialized line.
    #[test]
    fn no_code_bearing_field_can_reach_a_line() {
        const CANARY: &str = "SECRET_IDENTIFIER_FROM_USER_CODE";
        let mut e = NextEditEpisode::new("model", 2, 12);
        // Everything a caller might plausibly be tempted to smuggle
        // through: the path, the language, the region size, the drop
        // reason. Each is either a length, a closed-set token, or the
        // extension alone.
        e.path_ext = NextEditEpisode::ext_of(Some(&format!("/home/dev/{CANARY}/thing.RS")));
        e.language = Some("rust".into());
        e.region_bytes = Some(4096);
        e.dropped = Some("verify_failed".into());
        let line = serde_json::to_string(&JournalLine::Episode(e.clone())).unwrap();
        assert!(!line.contains(CANARY), "journal line leaked code-bearing text: {line}");
        // The extension survives, lowercased, and nothing else of the path does.
        assert_eq!(e.path_ext.as_deref(), Some("rs"));
        assert!(!line.contains("/home/dev"));
    }

    #[test]
    fn ext_of_takes_only_the_extension() {
        assert_eq!(NextEditEpisode::ext_of(Some("src/main.rs")).as_deref(), Some("rs"));
        assert_eq!(NextEditEpisode::ext_of(Some("a/b/Component.TSX")).as_deref(), Some("tsx"));
        assert_eq!(NextEditEpisode::ext_of(Some("Makefile")), None);
        assert_eq!(NextEditEpisode::ext_of(None), None);
    }

    /// `diverged` is not a rejection. If this ever passes with
    /// `dismissed == 2` the acceptance rate has started lying.
    #[test]
    fn diverged_is_never_counted_as_dismissed() {
        let lines = vec![
            ep("a", 1, 10),
            ep("b", 1, 20),
            ep("c", 1, 30),
            oc("a", NextEditOutcome::Accepted),
            oc("b", NextEditOutcome::Diverged),
            oc("c", NextEditOutcome::Superseded),
        ];
        let s = stats(&lines, 0);
        assert_eq!(s.dismissed, 0);
        assert_eq!(s.diverged, 1);
        assert_eq!(s.superseded, 1);
        // One accept, zero dismissals judged → 100% of the JUDGED set,
        // and the coverage number is what says that is 3 of 3 reported.
        assert_eq!(s.acceptance_rate(), Some(1.0));
        assert_eq!(s.reported_coverage(), Some(1.0));
    }

    #[test]
    fn unreported_episodes_are_unknown_not_dismissed() {
        let lines = vec![ep("a", 1, 10), ep("b", 1, 20), oc("a", NextEditOutcome::Accepted)];
        let s = stats(&lines, 0);
        assert_eq!(s.unknown, 1);
        assert_eq!(s.dismissed, 0);
        assert_eq!(s.acceptance_rate(), Some(1.0));
        // ...but only half the shown population reported, which is the
        // number that stops anyone quoting the 100%.
        assert_eq!(s.reported_coverage(), Some(0.5));
    }

    #[test]
    fn silent_episodes_are_not_awaiting_an_outcome() {
        // proposed == 0: nothing was shown, so there is nothing to
        // accept or dismiss and it must not inflate `unknown`.
        let s = stats(&[ep("a", 0, 5)], 0);
        assert_eq!(s.episodes, 1);
        assert_eq!(s.shown, 0);
        assert_eq!(s.unknown, 0);
        assert_eq!(s.acceptance_rate(), None);
        assert_eq!(s.reported_coverage(), None);
    }

    #[test]
    fn nothing_judged_is_none_not_zero_percent() {
        let s = stats(&[ep("a", 1, 5), oc("a", NextEditOutcome::Diverged)], 0);
        assert_eq!(s.acceptance_rate(), None, "0/0 must be could-not-judge, not 0%");
    }

    #[test]
    fn outcome_for_a_pruned_episode_is_an_orphan_not_a_verdict() {
        let s = stats(&[oc("gone", NextEditOutcome::Accepted)], 0);
        assert_eq!(s.orphan_outcomes, 1);
        assert_eq!(s.accepted, 0);
    }

    #[test]
    fn outcome_wire_is_a_closed_set() {
        for o in NextEditOutcome::ALL {
            assert_eq!(NextEditOutcome::from_wire(o.as_str()), Some(o));
        }
        assert_eq!(NextEditOutcome::from_wire("unknown"), None);
        assert_eq!(NextEditOutcome::from_wire("Accepted"), None);
        assert_eq!(NextEditOutcome::from_wire(""), None);
    }

    /// The round-trip through the real files, so this lane's line types
    /// are proven to survive the generic writer. The machinery itself —
    /// day rotation, the byte cap, pruning, the off-switches, a
    /// truncated tail — is tested once in `super::journal`, against a
    /// stream of its own, rather than re-tested per feature.
    #[test]
    fn a_real_append_round_trips_through_the_stream() {
        let dir = tempfile::tempdir().unwrap();
        assert!(append(dir.path(), &ep("a", 1, 10)).unwrap());
        assert!(append(dir.path(), &oc("a", NextEditOutcome::Accepted)).unwrap());
        let (lines, bad) = read_all(dir.path());
        assert_eq!(bad, 0);
        assert_eq!(lines.len(), 2);
        assert_eq!(stats(&lines, bad).accepted, 1);
    }

    /// The stream's own switch must gate THIS lane, not just the global
    /// one — a developer turning off next-edit journaling should not
    /// have to turn off every other stream too.
    #[test]
    fn this_lanes_own_marker_stops_the_writes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(NEXT_EDIT_STREAM.marker_in(dir.path()), "").unwrap();
        assert!(!NEXT_EDIT_STREAM.enabled(dir.path()));
        assert!(!append(dir.path(), &ep("a", 1, 10)).unwrap(), "off is Ok(false), not an error");
        assert_eq!(read_all(dir.path()).0.len(), 0);
    }

}
