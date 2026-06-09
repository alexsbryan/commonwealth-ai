// SPDX-License-Identifier: AGPL-3.0-or-later
//! Structural nudge generator (Phase 7.1).
//!
//! When the agent does work that *looks* architecturally
//! significant but hasn't called `note(...)`, we append a short
//! single-line hint to the next tool response: `[note worth
//! recording? You modified Trait `Foo` and 3 callers — call
//! note(decision, ...).]`. This replaces the obsolete 10-call
//! timer-based reflection nudge that fired regardless of context.
//!
//! ## Architectural signals
//!
//! Each call to [`StructuralNudgeGenerator::observe_diff`] hands
//! us a snapshot of files changed since the last nudge plus a
//! summary of what kind of changes they were. We tally signals;
//! when the total crosses the threshold we emit a nudge.
//!
//! Signals (per spec section 2.3):
//!
//! 1. ≥3 files modified since the last nudge.
//! 2. Diff touches a struct / trait / impl / interface / class
//!    definition.
//! 3. Manifest file (`Cargo.toml`, `package.json`, …) modified.
//! 4. Existing test assertion modified (not just added).
//! 5. Code matching a spec invariant keyword modified.
//!
//! ## Rate limit
//!
//! At most 1 nudge per [`NUDGE_INTERVAL_CALLS`] tool calls,
//! regardless of how many signals fired. This keeps the agent's
//! tool stream free of repeated reminders during a long edit
//! session.
//!
//! ## Output shape
//!
//! `pending_text(...)` returns `Some(line)` when a nudge is due
//! and `None` otherwise. Caller (the tool-call response path)
//! appends the line to the response body verbatim. Format:
//!
//! ```text
//! [note worth recording? <reason>. Call note(decision, …).]
//! ```
//!
//! Brackets bound it so the agent's parser doesn't confuse it
//! with tool output. The phrasing names the decision kind
//! (`decision`) so the simplest-correct invocation is obvious.
//!
//! ## Telemetry
//!
//! Conversion telemetry — "what fraction of nudges led to a
//! `note` call within N calls" — lives in the audit (Phase 7.3).
//! This module just emits and records what fired; the conversion
//! analysis reads `tool_call_log` after the fact.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Rate-limit window. At most one nudge fires per this many
/// observations. Picked as 15 in the spec — short enough that the
/// agent gets reminded during a meaningful edit session, long
/// enough that the same dozen tool calls don't spam.
pub const NUDGE_INTERVAL_CALLS: u32 = 15;

/// Minimum number of signals that must fire on a single
/// observation to trigger a nudge. With 5 signal types and a
/// threshold of 1, any single signal fires the nudge — but the
/// rate limit keeps the volume sane.
const MIN_SIGNALS_FOR_NUDGE: usize = 1;

/// Manifest filenames we treat as architecturally significant.
/// Modifying any of these is a signal — adds/removes a dep,
/// changes versions, opts into features, etc.
const MANIFEST_FILES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "pyproject.toml",
    "requirements.txt",
    "go.mod",
    "go.sum",
    "Gemfile",
    "Gemfile.lock",
    "build.gradle",
    "build.gradle.kts",
    "pom.xml",
    "Package.swift",
];

/// Keywords inside a diff snippet that suggest the change touched
/// a definition that other code depends on.
const STRUCTURAL_DEFINITION_KEYWORDS: &[&str] = &[
    // Rust
    "struct ",
    "trait ",
    "impl ",
    "enum ",
    // TS / Java / C# / Kotlin
    "interface ",
    "class ",
    // Python
    "class ",
    "def __init__",
    "abstract class",
];

/// Keywords inside a diff snippet that look like a test assertion
/// being modified (not added — modification implies semantic shift).
const TEST_ASSERTION_KEYWORDS: &[&str] = &[
    "assert_eq!",
    "assert!",
    "expect(",
    "assertEqual",
    "assertEquals",
    "should.equal",
    "shouldBe",
];

/// One observation hand-off from the caller. The matcher is pure
/// (no I/O), so we accept everything we need to score signals
/// up-front.
#[derive(Debug, Clone, Default)]
pub struct DiffObservation {
    /// All files changed since the last observe call. Path is
    /// relative to repo root.
    pub files_changed: Vec<PathBuf>,
    /// Diff text (additions + removals) for the changed files —
    /// concatenated, capped however the caller likes. The matcher
    /// treats this as one big string and grep's for the structural
    /// keyword set. Empty string is fine; it just means signals
    /// 2/4/5 don't fire.
    pub diff_text: String,
    /// True iff at least one of the modified file diffs deletes a
    /// pre-existing test assertion. Caller is responsible for the
    /// per-line semantic check (it's expensive enough that we
    /// don't want the matcher reparsing the diff).
    pub test_assertion_modified: bool,
    /// Spec-invariant keywords from the active spec(s). The
    /// matcher fires signal 5 when `diff_text` mentions any of
    /// these. Empty Vec → signal 5 doesn't fire.
    pub spec_invariant_keywords: Vec<String>,
}

/// Stateful nudge generator. One instance per running MCP server.
pub struct StructuralNudgeGenerator {
    /// Rate-limit counter. Increments every time `pending_text`
    /// is called (i.e. every observation). When it reaches
    /// `NUDGE_INTERVAL_CALLS` and a nudge is otherwise due, we
    /// emit the line and reset the counter.
    calls_since_last_nudge: std::sync::atomic::AtomicU32,
}

/// What signals fired on the most recent observation. Returned
/// alongside the nudge line (if any) so callers can record
/// telemetry. Set bit per signal:
///
/// - `multi_file`        — ≥3 files changed
/// - `structural_def`    — diff touches struct/trait/impl/etc.
/// - `manifest`          — manifest file modified
/// - `test_assertion`    — test assertion modified (not added)
/// - `spec_invariant`    — diff mentions a spec-invariant keyword
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NudgeSignals {
    pub multi_file: bool,
    pub structural_def: bool,
    pub manifest: bool,
    pub test_assertion: bool,
    pub spec_invariant: bool,
}

impl NudgeSignals {
    /// Total count of signals that fired. Used to decide whether
    /// the nudge clears the threshold.
    pub fn count(&self) -> usize {
        [
            self.multi_file,
            self.structural_def,
            self.manifest,
            self.test_assertion,
            self.spec_invariant,
        ]
        .iter()
        .filter(|b| **b)
        .count()
    }

    /// Plain-prose reason summary for the nudge line. Lists
    /// signals in priority order so the agent sees the most-
    /// concrete one first.
    pub fn reason(&self) -> String {
        let mut parts = Vec::new();
        if self.structural_def {
            parts.push("modified a struct/trait/impl definition");
        }
        if self.multi_file {
            parts.push("touched 3+ files");
        }
        if self.manifest {
            parts.push("modified a manifest file");
        }
        if self.test_assertion {
            parts.push("changed a test assertion");
        }
        if self.spec_invariant {
            parts.push("touched code matching a spec invariant");
        }
        parts.join("; ")
    }
}

impl StructuralNudgeGenerator {
    pub fn new() -> Self {
        Self {
            calls_since_last_nudge: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Score architectural signals on `obs`. Pure — no rate-limit
    /// state consulted. Tests call this directly to verify
    /// signal recognition independently of timing.
    pub fn score(obs: &DiffObservation) -> NudgeSignals {
        let multi_file = obs.files_changed.len() >= 3;

        let structural_def = STRUCTURAL_DEFINITION_KEYWORDS
            .iter()
            .any(|kw| obs.diff_text.contains(kw));

        let manifest = obs.files_changed.iter().any(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| MANIFEST_FILES.contains(&n))
                .unwrap_or(false)
        });

        let test_assertion = obs.test_assertion_modified
            || TEST_ASSERTION_KEYWORDS
                .iter()
                .any(|kw| obs.diff_text.contains(kw));

        // Normalise both sides to lowercase alphanumeric so a
        // spec heading like "Canonical Fingerprint" matches a
        // diff identifier like `canonical_fingerprint` (different
        // separators, same concept). Falls back to substring match
        // if the normalised form is too short (avoid a 3-letter
        // keyword false-matching half the diff).
        let spec_invariant = !obs.spec_invariant_keywords.is_empty() && {
            let lower_diff = obs.diff_text.to_lowercase();
            let normalised_diff = normalise(&lower_diff);
            obs.spec_invariant_keywords.iter().any(|kw| {
                let kw_lower = kw.to_lowercase();
                if lower_diff.contains(&kw_lower) {
                    return true;
                }
                let kw_norm = normalise(&kw_lower);
                kw_norm.len() >= 6 && normalised_diff.contains(&kw_norm)
            })
        };

        NudgeSignals {
            multi_file,
            structural_def,
            manifest,
            test_assertion,
            spec_invariant,
        }
    }

    /// Observe a diff and return the nudge line iff the
    /// rate-limit window has elapsed AND signals cleared the
    /// threshold. The line is meant to be appended verbatim to
    /// the next tool response.
    ///
    /// Side-effect: increments the per-instance call counter.
    /// Resets the counter on a successful emit.
    pub fn pending_text(&self, obs: &DiffObservation) -> Option<(String, NudgeSignals)> {
        // Always tick the counter — even if signals don't fire,
        // we want the rate window to advance so a later
        // qualifying observation isn't artificially suppressed
        // by a long quiet period without nudges.
        let prev = self
            .calls_since_last_nudge
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let signals = Self::score(obs);
        if signals.count() < MIN_SIGNALS_FOR_NUDGE {
            return None;
        }
        if prev + 1 < NUDGE_INTERVAL_CALLS {
            return None;
        }
        // Reset and emit.
        self.calls_since_last_nudge
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let reason = signals.reason();
        Some((
            format!("[note worth recording? You {reason}. Call note(decision, …).]"),
            signals,
        ))
    }

    /// Test helper: peek the rate-limit counter without
    /// mutating it.
    #[cfg(test)]
    fn calls_since_last_nudge(&self) -> u32 {
        self.calls_since_last_nudge
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Default for StructuralNudgeGenerator {
    fn default() -> Self {
        Self::new()
    }
}

/// Reduce a string to lowercase alphanumeric characters. Used so
/// "Canonical Fingerprint" and `canonical_fingerprint` compare
/// equal — they describe the same concept with different
/// separators.
fn normalise(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Best-effort: detect spec-invariant keywords from a feature
/// spec. Reads `<repo>/.sovereign/features/*/spec.md` and harvests
/// nouns / important-looking phrases. The matcher uses these to
/// fire signal 5.
///
/// "Important-looking" here is deliberately loose: any
/// `**bold**` token, any `# heading` line, any back-tick-quoted
/// term. The audit is forgiving of false positives — a noisy
/// nudge fires once per 15 calls anyway, and the user can ignore
/// it. False negatives (missing a real keyword) would mean signal
/// 5 silently doesn't fire, which is safer than spam.
pub fn extract_spec_invariant_keywords(repo_root: &Path) -> Vec<String> {
    let features_dir = repo_root.join(".sovereign").join("features");
    let mut out: HashSet<String> = HashSet::new();
    let Ok(entries) = std::fs::read_dir(&features_dir) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let spec_path = entry.path().join("spec.md");
        let Ok(text) = std::fs::read_to_string(&spec_path) else {
            continue;
        };
        for token in scan_keywords(&text) {
            out.insert(token);
        }
    }
    out.into_iter().collect()
}

/// Internal: walk markdown text and harvest candidate keywords.
/// Public for tests.
pub(crate) fn scan_keywords(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in md.lines() {
        let trimmed = line.trim_start();
        // Headings.
        if let Some(stripped) = trimmed.strip_prefix('#') {
            let h = stripped.trim_start_matches('#').trim();
            if !h.is_empty() {
                out.push(h.to_string());
            }
        }
        // Bold spans + back-tick spans (cheap regex via str::find).
        for delim in ["**", "`"] {
            let mut rest = trimmed;
            while let Some(start) = rest.find(delim) {
                let after = &rest[start + delim.len()..];
                if let Some(end) = after.find(delim) {
                    let inner = &after[..end];
                    if !inner.is_empty() && inner.len() < 80 {
                        out.push(inner.to_string());
                    }
                    rest = &after[end + delim.len()..];
                } else {
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs_with_files(paths: &[&str]) -> DiffObservation {
        DiffObservation {
            files_changed: paths.iter().map(PathBuf::from).collect(),
            diff_text: String::new(),
            test_assertion_modified: false,
            spec_invariant_keywords: Vec::new(),
        }
    }

    /// Signal 1: 3+ files changed → multi_file fires; <3 doesn't.
    #[test]
    fn signal1_multi_file_threshold_at_three() {
        let s2 = StructuralNudgeGenerator::score(&obs_with_files(&["a.rs", "b.rs"]));
        assert!(!s2.multi_file);
        let s3 = StructuralNudgeGenerator::score(&obs_with_files(&["a.rs", "b.rs", "c.rs"]));
        assert!(s3.multi_file);
        let s5 = StructuralNudgeGenerator::score(&obs_with_files(&["a", "b", "c", "d", "e"]));
        assert!(s5.multi_file);
    }

    /// Signal 2: diff text containing struct/trait/impl/etc. fires.
    #[test]
    fn signal2_structural_definition_keywords() {
        let mut obs = obs_with_files(&["foo.rs"]);
        obs.diff_text = "+struct Foo { x: u32 }".into();
        assert!(StructuralNudgeGenerator::score(&obs).structural_def);

        obs.diff_text = "+trait Bar { fn baz(); }".into();
        assert!(StructuralNudgeGenerator::score(&obs).structural_def);

        obs.diff_text = "+impl Bar for Foo { ... }".into();
        assert!(StructuralNudgeGenerator::score(&obs).structural_def);

        obs.diff_text = "+interface IFoo extends Bar { …}".into();
        assert!(StructuralNudgeGenerator::score(&obs).structural_def);

        // Pure prose change — should not fire.
        obs.diff_text = "+let x = 1;".into();
        assert!(!StructuralNudgeGenerator::score(&obs).structural_def);
    }

    /// Signal 3: changing a manifest file fires.
    #[test]
    fn signal3_manifest_files_fire() {
        for f in ["Cargo.toml", "package.json", "go.mod", "pyproject.toml"] {
            let s = StructuralNudgeGenerator::score(&obs_with_files(&[f]));
            assert!(s.manifest, "manifest signal didn't fire for {f}");
        }
        // Non-manifest path doesn't fire.
        let s = StructuralNudgeGenerator::score(&obs_with_files(&["src/lib.rs"]));
        assert!(!s.manifest);
    }

    /// Signal 4: existing test assertion modified fires (caller's
    /// flag OR a textual assertion in the diff).
    #[test]
    fn signal4_test_assertion_modified_fires() {
        let mut obs = obs_with_files(&["tests/foo.rs"]);
        obs.test_assertion_modified = true;
        assert!(StructuralNudgeGenerator::score(&obs).test_assertion);

        obs.test_assertion_modified = false;
        obs.diff_text = "-    assert_eq!(x, 1);\n+    assert_eq!(x, 2);".into();
        assert!(StructuralNudgeGenerator::score(&obs).test_assertion);

        obs.diff_text = "// no assertions here".into();
        assert!(!StructuralNudgeGenerator::score(&obs).test_assertion);
    }

    /// Signal 5: spec invariant keyword present in diff fires.
    /// Case-insensitive.
    #[test]
    fn signal5_spec_invariant_keyword_fires_case_insensitive() {
        let mut obs = obs_with_files(&["foo.rs"]);
        obs.spec_invariant_keywords = vec!["Canonical Fingerprint".into()];

        obs.diff_text = "+let canonical_fingerprint = compute(...);".into();
        assert!(StructuralNudgeGenerator::score(&obs).spec_invariant);

        obs.diff_text = "+ let CANONICAL_FINGERPRINT = X;".into();
        assert!(StructuralNudgeGenerator::score(&obs).spec_invariant);

        // No mention → signal stays cold.
        obs.diff_text = "+let foo = 1;".into();
        assert!(!StructuralNudgeGenerator::score(&obs).spec_invariant);

        // Empty keyword list → signal cold regardless of diff.
        obs.spec_invariant_keywords.clear();
        obs.diff_text = "+let canonical_fingerprint = ...;".into();
        assert!(!StructuralNudgeGenerator::score(&obs).spec_invariant);
    }

    /// `pending_text` rate-limits to one emit per
    /// NUDGE_INTERVAL_CALLS observations even when signals fire
    /// every call.
    #[test]
    fn pending_text_rate_limits_to_one_per_interval() {
        let g = StructuralNudgeGenerator::new();
        let mut obs = obs_with_files(&["a.rs", "b.rs", "c.rs"]); // multi_file always

        // Fewer than the interval — no emit yet.
        for _ in 0..(NUDGE_INTERVAL_CALLS - 1) {
            assert!(g.pending_text(&obs).is_none());
        }
        // Exactly the interval — emit fires.
        let emit = g.pending_text(&obs);
        assert!(emit.is_some(), "expected nudge at the interval boundary");

        // Counter reset; another full interval needed.
        for _ in 0..(NUDGE_INTERVAL_CALLS - 1) {
            obs.diff_text = "irrelevant".into();
            assert!(g.pending_text(&obs).is_none());
        }
        assert!(
            g.pending_text(&obs).is_some(),
            "expected second nudge after reset window"
        );
    }

    /// `pending_text` returns None when no signals fire, even if
    /// the interval window has elapsed.
    #[test]
    fn pending_text_returns_none_when_no_signals() {
        let g = StructuralNudgeGenerator::new();
        let obs = obs_with_files(&["a.rs"]); // single file, no diff text

        for _ in 0..NUDGE_INTERVAL_CALLS {
            assert!(g.pending_text(&obs).is_none());
        }
    }

    /// The reason string lists every active signal in priority
    /// order so the nudge is concrete.
    #[test]
    fn reason_string_lists_active_signals() {
        let s = NudgeSignals {
            multi_file: true,
            structural_def: true,
            manifest: false,
            test_assertion: false,
            spec_invariant: true,
        };
        let r = s.reason();
        assert!(r.contains("struct/trait/impl"));
        assert!(r.contains("3+ files"));
        assert!(r.contains("spec invariant"));
        assert!(!r.contains("manifest")); // not active
    }

    /// `scan_keywords` extracts headings, bold spans, and
    /// back-tick spans from a markdown spec.
    #[test]
    fn scan_keywords_extracts_headings_and_spans() {
        let md = r#"# Canonical Fingerprint

The **fingerprint** is a stable hash of the canonical content.

Use `compute_canonical_fingerprint` to obtain it.

## Invariants

- The fingerprint MUST be byte-faithful.
"#;
        let kws = scan_keywords(md);
        // Headings
        assert!(kws.iter().any(|k| k == "Canonical Fingerprint"));
        assert!(kws.iter().any(|k| k == "Invariants"));
        // Bold span
        assert!(kws.iter().any(|k| k == "fingerprint"));
        // Back-tick span
        assert!(kws.iter().any(|k| k == "compute_canonical_fingerprint"));
    }
}
