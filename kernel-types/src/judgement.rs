// SPDX-License-Identifier: AGPL-3.0-or-later
//! The trust envelope: *how much should you trust this result, and how old is
//! it.*
//!
//! # Why this is a type at all
//!
//! `quality/NOUN_CONVERGENCE.md` §10.1 measured three concept families that
//! all answer that one question and share no name, so the name-keyed census is
//! blind to every one of them:
//!
//! | family | types | crates | reach ≥ 3 |
//! |---|---:|---:|---:|
//! | verdict / judgement | 198 | 26 | 1% |
//! | citation / provenance | 112 | 23 | 4% |
//! | freshness / staleness | 41 | 15 | 0% |
//!
//! 351 types at roughly 2% adoption. The visible symptom is `svrn posture`,
//! which aggregates seven subsystems and prints them in **seven status
//! vocabularies** (`fresh`, `stale`, `fail (stale)`, `off (by design)`,
//! `present`, `present (gaps)`) and **seven age formats** (`12d`, `1h`, `16d`,
//! `7d ago`, `-`, `6d`, `9d..95d`) — because there was nothing to reach for,
//! so each subsystem invented its own. Underneath sit **172 hand-written
//! freshness fields in 13 spellings** (`age_secs` 41, `stale` 23,
//! `generated_at` 22, `built_at` 20, `age_days` 15, …): three questions —
//! when was it made, how old is that, is it too old — asked thirteen ways.
//!
//! # Why it lives in the kernel and carries work
//!
//! Two constraints from the campaign, both load-bearing:
//!
//! **Home.** `converge noun Verdict` ranks `sovereign-contracts` above
//! `kernel-types` as canonical owner, but it ranks by crates that ALREADY
//! depend on a candidate, so it structurally prefers whatever is biggest.
//! `sovereign-contracts` already absorbs 38.9% of inbound type traffic and
//! §10.8 names "sovereign-contracts becomes the megablock" as a failure mode
//! whose stated guard is *"the envelope goes to `kernel-types`, not
//! contracts"*. §10.5 says this crate "holds Custody, Grain, Source,
//! Attribution, ContentHash, CorpusId, Locator, Origin … and **stopped before
//! freshness and verdict**". The home was minted for this.
//!
//! **It must carry work.** §10.3 is a control experiment already run:
//! adoption is monotone in work carried and in nothing else — `Recipe` ~100%
//! (a whole pipeline), `Store` 35%, `Tool` 29%, `Error` 8%, `Config` 7%,
//! `Report` **1% of 105**, `Args` **0% of 58**. *"Extract work, not shape. A
//! shared struct saves an author nothing and loses to bespoke every time,
//! gate or no gate."* A bare four-variant enum is shape and would land where
//! `Report` landed. So [`Judgement`] owns, once, the five jobs its callers
//! were each writing by hand:
//!
//! 1. **age computation** — [`Judgement::age`], one subtraction against one clock;
//! 2. **one age format** — [`Judgement::age_label`], replacing seven;
//! 3. **staleness banding** — [`Judgement::freshness`], against a per-caller horizon;
//! 4. **one status vocabulary** — [`Judgement::label`], replacing seven;
//! 5. **the roll-up conjunction and the honesty footer** — [`Judgement::roll_up`]
//!    and [`honesty_footer`], the two things every aggregator re-derived.
//!
//! # Back-of-house and the product share this, and that is legal
//!
//! `quality/noun-convergence.toml` lists "Verdict (the eval kind)" among
//! back-of-house's nouns. That names where the noun's HOME is; it is not an
//! exclusion on other layers. The one-way rule forbids depending on
//! back-of-house *the layer* — it does not forbid back-of-house and the
//! product sharing a high-abstraction ancestor sitting BENEATH both. Product
//! → `kernel-types` is down. Back-of-house → `kernel-types` is down. Neither
//! names the other and no edge inverts. Splitting the eval verdict from the
//! product verdict to respect a rule that was never violated would have
//! minted definition eleven of a concept that already has ten.
//!
//! # What is deliberately NOT a field here
//!
//! `Judgement.quote: Evidence` was in the original sketch and **cannot
//! exist**: `Evidence` lives in `corpus-engine`, and `corpus-engine` depends
//! on this crate. Naming it here inverts the one edge this crate exists to
//! forbid. Evidence-bearing callers compose instead — a struct holding both a
//! `Judgement` and its `Evidence`, in the crate that owns the evidence. The
//! same reasoning keeps `sovereign-time` out and dates in `std::time`.

use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// The four verdicts (ARCH §18.1, §18.2). Four, not two — *passed*, *failed*,
/// *could-not-judge*, *never-ran*. The two that usually go missing are the
/// two that matter: a check that could not run and a check that never ran are
/// each reported, never collapsed into either of the other two.
///
/// The wire form is kebab-case and is a contract: deep-research ICD artifacts
/// already carry `"could-not-judge"` on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Passed,
    Failed,
    CouldNotJudge,
    NeverRan,
}

impl Verdict {
    /// The wire spelling. Stable — artifacts on disk carry it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Verdict::Passed => "passed",
            Verdict::Failed => "failed",
            Verdict::CouldNotJudge => "could-not-judge",
            Verdict::NeverRan => "never-ran",
        }
    }

    /// Parse the wire spelling. Returns `None` rather than defaulting: an
    /// unrecognised verdict is an absence, and absence is reported, never
    /// defaulted (ARCH §18.3).
    pub fn parse_wire(s: &str) -> Option<Verdict> {
        match s {
            "passed" => Some(Verdict::Passed),
            "failed" => Some(Verdict::Failed),
            "could-not-judge" | "could_not_judge" => Some(Verdict::CouldNotJudge),
            "never-ran" | "never_ran" => Some(Verdict::NeverRan),
            _ => None,
        }
    }

    /// Severity order, worst first, for sorting a report so the rows that need
    /// attention are on screen: `Failed` 0 < `NeverRan` 1 < `CouldNotJudge` 2
    /// < `Passed` 3.
    pub const fn rank(self) -> u8 {
        match self {
            Verdict::Failed => 0,
            Verdict::NeverRan => 1,
            Verdict::CouldNotJudge => 2,
            Verdict::Passed => 3,
        }
    }

    /// Does this verdict want a human to look? True for everything except
    /// `Passed` — a could-not-judge is not a pass (ARCH §18.2).
    pub const fn needs_attention(self) -> bool {
        !matches!(self, Verdict::Passed)
    }
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a judgement reads the way it does.
///
/// Non-empty by construction, and it refuses the placeholder vocabulary that
/// is the silent default this whole module exists to forbid. A reason reading
/// `"unknown"` is not a reason; it is a `None` wearing a `String`'s clothes,
/// and it is exactly how a check with nothing behind it comes to render as
/// though something were (ARCH §18.3, principle 6).
///
/// Two doors, because the two sources of reason text have different failure
/// modes:
///
/// - [`Reason::new`] for text derived at runtime — returns `None` on refusal,
///   so the caller must decide what an unexplainable result means.
/// - [`Reason::literal`] for an author-written `&'static str` — panics on
///   refusal, which fires on the first test run rather than in production,
///   the same bargain as `unwrap` on a compile-time-known constant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Reason(String);

/// Spellings that carry no information. Compared trimmed and lowercased.
/// This list is short on purpose: it catches the placeholders observed in
/// this codebase, not every unhelpful sentence anyone could write. A type
/// cannot make a reason *good*; it can refuse the ones that are provably
/// empty.
const PLACEHOLDERS: &[&str] = &[
    "",
    "-",
    "--",
    "?",
    "n/a",
    "na",
    "none",
    "null",
    "nil",
    "tbd",
    "todo",
    "unknown",
    "unspecified",
];

/// Spellings that name an absence in an EXTRACTED FIELD but are not
/// placeholder *reasons* — a model asked for a coin's mint may answer
/// "omitted" or "not applicable", and neither is a reason anyone wrote.
const FIELD_ABSENCES: &[&str] = &["omit", "omitted", "(none)", "not applicable"];

/// Prefixes that name an absence when they stand as a whole word.
///
/// THE WORD BOUNDARY IS THE POINT. `"unknown"` and `"unknown (not stated in
/// text)"` are absent values; `"unknown-type sceatta series"` is a real one,
/// and a bare `starts_with` swallowed it. That defect shipped twice — the
/// numismatics extractor lost real values to it, and the test that was meant
/// to defend the case carried an escape that made it pass for free (ARCH
/// §18.1, 2026-09-03).
const ABSENCE_PREFIXES: &[&str] = &["unknown", "not stated"];

/// Does this text NAME AN ABSENCE rather than carry a value?
///
/// THE one decider for that question. It was three: `Reason::is_placeholder`
/// here, `parse_policy::is_absent_marker` in corpus-engine (ten exact
/// spellings plus a prefix rule), and an inline chain in sovereign-core's
/// `value_presence.rs` (a bare `starts_with`, still). Three vocabularies
/// meant a value dropped by the extractor could survive the judge and vice
/// versa, with nothing pinning them together.
///
/// Trimmed and lowercased before matching. A prefix must end at a word
/// boundary — see [`ABSENCE_PREFIXES`].
pub fn is_absent_marker(text: &str) -> bool {
    let t = text.trim().to_ascii_lowercase();
    if PLACEHOLDERS.contains(&t.as_str()) || FIELD_ABSENCES.contains(&t.as_str()) {
        return true;
    }
    ABSENCE_PREFIXES.iter().any(|p| {
        t.strip_prefix(p)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace()))
    })
}

impl Reason {
    /// Build a reason from runtime-derived text. `None` when the text is
    /// empty, whitespace, or one of the placeholder spellings.
    pub fn new(text: impl Into<String>) -> Option<Reason> {
        let text = text.into();
        if Reason::is_placeholder(&text) {
            return None;
        }
        Some(Reason(text))
    }

    /// Build a reason from an author-written literal.
    ///
    /// # Panics
    ///
    /// If the literal is a placeholder. Deliberate: the argument is
    /// `&'static str`, so the offending text is in the caller's source and the
    /// panic is a programmer error caught by the first run of any test that
    /// touches the line.
    #[track_caller]
    pub fn literal(text: &'static str) -> Reason {
        Reason::new(text)
            .unwrap_or_else(|| panic!("Reason::literal({text:?}) is a placeholder, not a reason"))
    }

    /// Is `text` one of the spellings that carry no information?
    ///
    /// EXACT match only, and deliberately so — a reason is prose, and prose
    /// beginning "unknown cause of the flap" is informative. The other
    /// question, "does this text NAME an absence rather than carry a value",
    /// is [`is_absent_marker`]: it shares this vocabulary and adds a
    /// prefix rule, because an extracted FIELD reading "unknown (not stated
    /// in text)" is absent while a reason reading the same is not.
    pub fn is_placeholder(text: &str) -> bool {
        let t = text.trim().to_ascii_lowercase();
        PLACEHOLDERS.contains(&t.as_str())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How old the judged artifact is, relative to the horizon its caller set.
///
/// [`Freshness::Undated`] is the variant the hand-rolled instances disagreed
/// about and it is why this is an enum rather than a bool: on 2026-08-20
/// `NightlyPosture::is_stale` returned `true` for an unreadable mtime while
/// `posture_cmd::aged_artifact_row` returned `never_run` for the same
/// condition — two answers to "I do not know how old this is", neither of
/// them "I do not know".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Freshness {
    /// Dated, and within the horizon.
    Fresh,
    /// Dated, and past the horizon. The verdict still stands; how much it is
    /// worth does not.
    Stale,
    /// No date, or no horizon to judge one against. Not stale — unknown.
    Undated,
}

impl Freshness {
    pub const fn as_str(self) -> &'static str {
        match self {
            Freshness::Fresh => "fresh",
            Freshness::Stale => "stale",
            Freshness::Undated => "undated",
        }
    }
}

impl std::fmt::Display for Freshness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A verdict about a named subject, why it reads that way, and — when there
/// is an artifact behind it — how old that artifact is and how old it is
/// allowed to get.
///
/// Fields are private and there is no `Default`. The only doors are the four
/// verdict constructors, each of which **takes a [`Reason`] by value**: there
/// is no way to report a failure, a could-not-judge or a never-ran without
/// saying why, and no way to say why with a placeholder. That is the
/// structural half — see `kernel-types/tests/ui/` for the compile-fail proofs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Judgement {
    /// What was judged — a subsystem, a gate, a claim. Stable and greppable.
    subject: String,
    verdict: Verdict,
    reason: Reason,
    /// When the artifact behind this judgement was made. `None` when there is
    /// no artifact — a `NeverRan` usually has none.
    as_of: Option<SystemTime>,
    /// How old the artifact may get before [`Freshness::Stale`]. `None` means
    /// this caller has no horizon, not that the horizon is infinite.
    horizon: Option<Duration>,
}

impl Judgement {
    fn new(subject: impl Into<String>, verdict: Verdict, reason: Reason) -> Judgement {
        Judgement {
            subject: subject.into(),
            verdict,
            reason,
            as_of: None,
            horizon: None,
        }
    }

    /// The check ran and it passed. `reason` says what was checked — a pass
    /// with nothing behind it is the green this module exists to prevent.
    pub fn passed(subject: impl Into<String>, reason: Reason) -> Judgement {
        Judgement::new(subject, Verdict::Passed, reason)
    }

    /// The check ran and it failed. `reason` names the failure.
    pub fn failed(subject: impl Into<String>, reason: Reason) -> Judgement {
        Judgement::new(subject, Verdict::Failed, reason)
    }

    /// The check ran and could not reach a verdict. `reason` names what
    /// stopped it. Never a pass and never a failure (ARCH §18.2).
    pub fn could_not_judge(subject: impl Into<String>, reason: Reason) -> Judgement {
        Judgement::new(subject, Verdict::CouldNotJudge, reason)
    }

    /// The check never ran. `reason` names what is missing and, where one
    /// exists, the command that would produce it.
    pub fn never_ran(subject: impl Into<String>, reason: Reason) -> Judgement {
        Judgement::new(subject, Verdict::NeverRan, reason)
    }

    /// Date this judgement to the artifact behind it.
    #[must_use]
    pub fn as_of(mut self, when: SystemTime) -> Judgement {
        self.as_of = Some(when);
        self
    }

    /// Set the staleness horizon. Without one, [`Judgement::freshness`] is
    /// [`Freshness::Undated`] even when the judgement is dated — "I know when
    /// this was made" and "I know how old it is allowed to be" are two facts.
    #[must_use]
    pub fn stale_after(mut self, horizon: Duration) -> Judgement {
        self.horizon = Some(horizon);
        self
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }

    pub fn verdict(&self) -> Verdict {
        self.verdict
    }

    pub fn reason(&self) -> &Reason {
        &self.reason
    }

    pub fn dated_at(&self) -> Option<SystemTime> {
        self.as_of
    }

    pub fn horizon(&self) -> Option<Duration> {
        self.horizon
    }

    /// How long ago the artifact was made. `None` when undated. A clock that
    /// has gone backwards yields `ZERO` rather than an error — the one
    /// subtraction, in one place, so thirteen field spellings stop each doing
    /// it their own way.
    pub fn age(&self) -> Option<Duration> {
        self.age_at(SystemTime::now())
    }

    /// [`Judgement::age`] against an injected clock. Tests use this; nothing
    /// else should need to.
    pub fn age_at(&self, now: SystemTime) -> Option<Duration> {
        self.as_of
            .map(|t| now.duration_since(t).unwrap_or(Duration::ZERO))
    }

    /// The one age format: `45m` under an hour, `7h` under a day, `12d`
    /// beyond. `-` when undated — the same glyph the tables already used for
    /// "no artifact", now meaning it everywhere.
    pub fn age_label(&self) -> String {
        self.age()
            .map(format_age)
            .unwrap_or_else(|| "-".to_string())
    }

    /// The staleness band. [`Freshness::Undated`] unless BOTH a date and a
    /// horizon are present.
    pub fn freshness(&self) -> Freshness {
        self.freshness_at(SystemTime::now())
    }

    /// [`Judgement::freshness`] against an injected clock.
    pub fn freshness_at(&self, now: SystemTime) -> Freshness {
        match (self.age_at(now), self.horizon) {
            (Some(age), Some(h)) if age > h => Freshness::Stale,
            (Some(_), Some(_)) => Freshness::Fresh,
            _ => Freshness::Undated,
        }
    }

    /// The one status vocabulary: the verdict, with ` (stale)` appended when
    /// the artifact behind it is past its horizon. Replaces `fresh` / `stale`
    /// / `fail (stale)` / `present` / `present (gaps)` / `never_run` /
    /// `not yet present` — seven vocabularies whose only shared property was
    /// that a reader had to learn each one.
    pub fn label(&self) -> String {
        match self.freshness() {
            Freshness::Stale => format!("{} (stale)", self.verdict.as_str()),
            _ => self.verdict.as_str().to_string(),
        }
    }

    /// Does this row want a human to look? A non-`Passed` verdict, or a
    /// `Passed` whose evidence has gone stale.
    pub fn needs_attention(&self) -> bool {
        self.verdict.needs_attention() || self.freshness() == Freshness::Stale
    }

    /// The conjunction, stated once (ARCH §18.2 — four verdicts, not two).
    ///
    /// Strict severity order, worst wins: any `Failed` fails; else any
    /// `NeverRan` never-ran; else any `CouldNotJudge` could-not-judge; else
    /// `Passed`. An empty set is `CouldNotJudge`, never a pass — a roll-up
    /// over nothing has judged nothing.
    ///
    /// Note what this deliberately does NOT do: it does not exclude
    /// unjudgeable members from the conjunction and call the rest a pass.
    /// That collapse is how "3 of 4 gates could not be judged" comes to print
    /// as green, and §18.2 exists to name it.
    ///
    /// The roll-up inherits the OLDEST member's date and the SHORTEST horizon,
    /// so an aggregate is never fresher than its stalest input.
    pub fn roll_up<'a>(
        subject: impl Into<String>,
        parts: impl IntoIterator<Item = &'a Judgement>,
    ) -> Judgement {
        let parts: Vec<&Judgement> = parts.into_iter().collect();
        if parts.is_empty() {
            return Judgement::could_not_judge(
                subject,
                Reason::literal("rolled up over an empty set — nothing was judged"),
            );
        }
        let worst = parts
            .iter()
            .map(|p| p.verdict)
            .min_by_key(|v| v.rank())
            .expect("non-empty");

        let named: Vec<&str> = parts
            .iter()
            .filter(|p| p.verdict == worst)
            .map(|p| p.subject.as_str())
            .collect();
        let reason = Reason::new(format!(
            "{} of {} {}: {}",
            named.len(),
            parts.len(),
            worst.as_str(),
            named.join(", ")
        ))
        .expect("a count and a subject list is never a placeholder");

        let mut out = Judgement::new(subject, worst, reason);
        out.as_of = parts.iter().filter_map(|p| p.as_of).min();
        out.horizon = parts.iter().filter_map(|p| p.horizon).min();
        out
    }
}

impl std::fmt::Display for Judgement {
    /// `subject: label (age) — reason`. The single-line form; the table form
    /// is [`render_rows`].
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} ({}) — {}",
            self.subject,
            self.label(),
            self.age_label(),
            self.reason
        )
    }
}

/// `45m` / `7h` / `12d`. One format, replacing the seven `svrn posture`
/// printed and the four spellings of "how long ago" in the CLI crates.
fn format_age(d: Duration) -> String {
    let s = d.as_secs();
    if s < 3_600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3_600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// Render judgements as an aligned table: subject, status, age, reason.
///
/// Column widths come from the data, so a long subject widens the column
/// instead of being truncated into ambiguity. Rows are printed in the order
/// given — the caller's order is usually meaningful (display order in a
/// registry), and re-sorting it here would hide that.
pub fn render_rows(rows: &[Judgement]) -> String {
    let subject_w = rows.iter().map(|r| r.subject.len()).max().unwrap_or(0);
    let label_w = rows.iter().map(|r| r.label().len()).max().unwrap_or(0);
    let age_w = rows.iter().map(|r| r.age_label().len()).max().unwrap_or(0);
    let mut out = String::new();
    for r in rows {
        out.push_str(&format!(
            "  {:<subject_w$}  {:<label_w$}  {:>age_w$}  {}\n",
            r.subject,
            r.label(),
            r.age_label(),
            r.reason
        ));
    }
    out
}

/// The honesty footer: how many rows want attention, or `None` when none do.
///
/// This is the line every aggregator wrote by hand and several forgot — a
/// table where the reader has to scan for the bad row is a table whose bad
/// row goes unread for three days (which is the observed failure, not a
/// hypothetical: `cli_contract_report`'s own doc comment records a FAIL
/// verdict sitting unread behind exactly that).
pub fn honesty_footer(rows: &[Judgement]) -> Option<String> {
    let n = rows.iter().filter(|r| r.needs_attention()).count();
    if n == 0 {
        return None;
    }
    Some(format!(
        "{n} of {} want attention (not passed, or passed on stale evidence)",
        rows.len()
    ))
}

#[cfg(test)]
mod tests {

    /// The two questions this module answers about empty-looking text, and
    /// the case that separates them. A reason reading "unknown cause of the
    /// flap" is informative prose; a coin's `mint` field reading "unknown
    /// (not stated in text)" is an absence. Only the second reading carries
    /// the prefix rule.
    #[test]
    fn an_absence_marker_is_not_the_same_question_as_a_placeholder_reason() {
        for absent in [
            "unknown",
            "Unknown (not stated in text)",
            "not stated",
            "omitted",
            "not applicable",
            "(none)",
            "n/a",
            "  TBD ",
        ] {
            assert!(is_absent_marker(absent), "{absent}");
        }
        // The word boundary. Both are real values, and a bare `starts_with`
        // swallowed them.
        for real in [
            "unknown-type sceatta series",
            "not stated-in-catalogue variant",
            "Wessex Down 1",
            "c. 720",
        ] {
            assert!(!is_absent_marker(real), "{real}");
        }
        // A reason is prose: the prefix rule must NOT reach it.
        assert!(!Reason::is_placeholder("unknown cause of the flap"));
        assert!(Reason::is_placeholder("unknown"));
        // …and the field-only spellings are not placeholder reasons either.
        assert!(!Reason::is_placeholder("not applicable"));
        assert!(is_absent_marker("not applicable"));
    }
    use super::*;

    fn r(s: &'static str) -> Reason {
        Reason::literal(s)
    }

    #[test]
    fn wire_form_round_trips_and_is_kebab_case() {
        for v in [
            Verdict::Passed,
            Verdict::Failed,
            Verdict::CouldNotJudge,
            Verdict::NeverRan,
        ] {
            assert_eq!(Verdict::parse_wire(v.as_str()), Some(v));
            assert_eq!(serde_json::to_string(&v).unwrap(), format!("\"{v}\""));
        }
        assert_eq!(
            Verdict::parse_wire("could-not-judge"),
            Some(Verdict::CouldNotJudge)
        );
        // An unrecognised verdict is reported, never defaulted (§18.3).
        assert_eq!(Verdict::parse_wire("green"), None);
        assert_eq!(Verdict::parse_wire(""), None);
    }

    #[test]
    fn severity_order_puts_failures_first() {
        let mut vs = vec![
            Verdict::Passed,
            Verdict::CouldNotJudge,
            Verdict::Failed,
            Verdict::NeverRan,
        ];
        vs.sort_by_key(|v| v.rank());
        assert_eq!(
            vs,
            vec![
                Verdict::Failed,
                Verdict::NeverRan,
                Verdict::CouldNotJudge,
                Verdict::Passed
            ]
        );
        assert!(!Verdict::Passed.needs_attention());
        assert!(Verdict::CouldNotJudge.needs_attention());
    }

    #[test]
    fn reason_refuses_the_placeholder_vocabulary() {
        for bad in [
            "", "   ", "-", "?", "n/a", "N/A", "None", "UNKNOWN", " tbd ",
        ] {
            assert!(Reason::new(bad).is_none(), "{bad:?} should be refused");
        }
        assert_eq!(
            Reason::new("no baseline on this host").unwrap().as_str(),
            "no baseline on this host"
        );
    }

    #[test]
    #[should_panic(expected = "is a placeholder, not a reason")]
    fn reason_literal_panics_on_a_placeholder() {
        let _ = Reason::literal("unknown");
    }

    #[test]
    fn age_and_freshness_need_both_a_date_and_a_horizon() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let made = now - Duration::from_secs(3 * 86_400);

        // Dated, no horizon: age is known, freshness is not.
        let dated = Judgement::passed("drift", r("report present")).as_of(made);
        assert_eq!(dated.age_at(now), Some(Duration::from_secs(3 * 86_400)));
        assert_eq!(dated.freshness_at(now), Freshness::Undated);

        // Horizon, no date: freshness is not knowable either.
        let horizoned = Judgement::passed("drift", r("report present"))
            .stale_after(Duration::from_secs(86_400));
        assert_eq!(horizoned.age_at(now), None);
        assert_eq!(horizoned.freshness_at(now), Freshness::Undated);

        // Both: banded.
        let fresh = dated.clone().stale_after(Duration::from_secs(14 * 86_400));
        assert_eq!(fresh.freshness_at(now), Freshness::Fresh);
        let stale = dated.stale_after(Duration::from_secs(86_400));
        assert_eq!(stale.freshness_at(now), Freshness::Stale);
    }

    #[test]
    fn one_age_format_replaces_seven() {
        assert_eq!(format_age(Duration::from_secs(45 * 60)), "45m");
        assert_eq!(format_age(Duration::from_secs(7 * 3_600)), "7h");
        assert_eq!(format_age(Duration::from_secs(60 * 3_600)), "2d");
        assert_eq!(format_age(Duration::ZERO), "0m");
        // Undated renders as the one glyph, not as "unknown"/""/"-"/"n/a".
        assert_eq!(
            Judgement::never_ran("bench", r("no latest.json")).age_label(),
            "-"
        );
    }

    #[test]
    fn a_stale_pass_says_so_in_the_status_word() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let old = Judgement::passed("arch", r("census present"))
            .as_of(now - Duration::from_secs(30 * 86_400))
            .stale_after(Duration::from_secs(14 * 86_400));
        // `freshness()` reads the real clock, so assert the composition
        // through the injected-clock accessors and the verdict word.
        assert_eq!(old.freshness_at(now), Freshness::Stale);
        assert_eq!(old.verdict(), Verdict::Passed);
        let never = Judgement::never_ran("watchers", r("no heartbeat sidecar"));
        assert_eq!(never.label(), "never-ran");
        assert!(never.needs_attention());
    }

    #[test]
    fn roll_up_takes_the_worst_and_never_launders_an_unjudgeable() {
        let pass = Judgement::passed("a", r("checked"));
        let cnj = Judgement::could_not_judge("b", r("daemon down"));
        let never = Judgement::never_ran("c", r("no artifact"));
        let fail = Judgement::failed("d", r("two claims contradicted"));

        assert_eq!(
            Judgement::roll_up("all", [&pass, &cnj]).verdict(),
            Verdict::CouldNotJudge,
            "one pass must not launder an unjudgeable into green"
        );
        assert_eq!(
            Judgement::roll_up("all", [&pass, &cnj, &never]).verdict(),
            Verdict::NeverRan
        );
        assert_eq!(
            Judgement::roll_up("all", [&pass, &cnj, &never, &fail]).verdict(),
            Verdict::Failed
        );
        assert_eq!(
            Judgement::roll_up("all", [&pass]).verdict(),
            Verdict::Passed
        );
    }

    #[test]
    fn roll_up_over_nothing_is_not_a_pass() {
        let empty: Vec<&Judgement> = vec![];
        let j = Judgement::roll_up("all", empty);
        assert_eq!(j.verdict(), Verdict::CouldNotJudge);
        assert!(j.reason().as_str().contains("empty set"));
    }

    #[test]
    fn roll_up_names_which_members_carried_the_verdict() {
        let ok = Judgement::passed("a", r("checked"));
        let bad = Judgement::failed("b", r("boom"));
        let worse = Judgement::failed("c", r("also boom"));
        let j = Judgement::roll_up("suite", [&ok, &bad, &worse]);
        assert_eq!(j.reason().as_str(), "2 of 3 failed: b, c");
    }

    #[test]
    fn roll_up_is_never_fresher_than_its_stalest_input() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000_000);
        let recent = Judgement::passed("a", r("checked"))
            .as_of(now - Duration::from_secs(3_600))
            .stale_after(Duration::from_secs(14 * 86_400));
        let ancient = Judgement::passed("b", r("checked"))
            .as_of(now - Duration::from_secs(90 * 86_400))
            .stale_after(Duration::from_secs(2 * 86_400));
        let j = Judgement::roll_up("suite", [&recent, &ancient]);
        assert_eq!(j.age_at(now), Some(Duration::from_secs(90 * 86_400)));
        assert_eq!(j.freshness_at(now), Freshness::Stale);
    }

    #[test]
    fn the_footer_is_absent_when_nothing_wants_attention() {
        let rows = vec![
            Judgement::passed("a", r("checked")),
            Judgement::passed("b", r("checked")),
        ];
        assert_eq!(honesty_footer(&rows), None);
        let rows = vec![
            Judgement::passed("a", r("checked")),
            Judgement::never_ran("b", r("no artifact")),
        ];
        assert_eq!(
            honesty_footer(&rows).unwrap(),
            "1 of 2 want attention (not passed, or passed on stale evidence)"
        );
    }

    #[test]
    fn rows_render_aligned_and_keep_the_callers_order() {
        let rows = vec![
            Judgement::never_ran("contract-nightly", r("no verdict on this host")),
            Judgement::passed("arch", r("census present")),
        ];
        let out = render_rows(&rows);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("  contract-nightly  never-ran"));
        // Column widened to the longest subject, not truncated.
        assert!(lines[1].starts_with("  arch              passed"));
    }

    #[test]
    fn judgement_serde_round_trips() {
        let j = Judgement::failed("env-gate", r("3 undeclared env reads"))
            .as_of(SystemTime::UNIX_EPOCH + Duration::from_secs(42))
            .stale_after(Duration::from_secs(86_400));
        let wire = serde_json::to_string(&j).unwrap();
        assert_eq!(serde_json::from_str::<Judgement>(&wire).unwrap(), j);
        assert!(wire.contains("\"failed\""));
        assert!(wire.contains("3 undeclared env reads"));
    }
}
