// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed progress events for the `enrich build` orchestration.
//!
//! Emitted by the library-level orchestrator (`build_with_progress`
//! in the sovereign-cli layer) so callers — CLI, desktop app,
//! future web UI — can render consistent progress without
//! sniffing stdout or re-implementing banners.
//!
//! The event shape is deliberately closed-enum + `Serialize` so:
//!
//!   1. Tauri can `emit` these on a per-job channel (see
//!      `sovereign-desktop/src-tauri/src/enrich_commands.rs`).
//!   2. The CLI prints them via a `Display` impl for a uniform
//!      rendering between CLI text and desktop UI labels.
//!   3. A future headless mode (CI/automation) can filter on
//!      `serde_json::Value` without string-matching.
//!
//! Event granularity is **step-level** for orchestration
//! (`StepStart`, `StepDone`, `StepFailed`) with a `ChapterProgress`
//! pass-through for the one phase that is chapter-granular (Phase
//! 1). Finer per-phase events can land later without breaking
//! existing listeners because the enum is tagged as
//! `#[serde(tag = "kind")]` — new variants are additive.

use serde::{Deserialize, Serialize};

use super::types::{PhaseFailure, PipelinePhase};

/// Canonical orchestration step within a `build` run. Maps 1:1 to
/// the `Step` enum inside `sovereign-cli/src/enrich_cmd/build.rs`
/// but lives here so downstream consumers (desktop, web) don't
/// need to depend on the CLI crate.
///
/// Mirrors the CLI's step labels so an operator who has read the
/// `enrich build` help sees the same words in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildStep {
    Seed,
    Extract,
    Cluster,
    Name,
    Resolve,
    Tensions,
    Gaps,
    Configure,
    Report,
    /// Embed the resolved atoms into the persistent ANN seed table
    /// (`atlas/atoms_ann.lance`) so the corpus grounds without an operator
    /// command. Last: it reads the atlas every other step wrote.
    Backfill,
}

impl BuildStep {
    pub const fn id(&self) -> &'static str {
        match self {
            Self::Seed => "seed",
            Self::Extract => "extract",
            Self::Cluster => "cluster",
            Self::Name => "name",
            Self::Resolve => "resolve",
            Self::Tensions => "tensions",
            Self::Gaps => "gaps",
            Self::Configure => "configure",
            Self::Report => "report",
            Self::Backfill => "backfill",
        }
    }

    /// Human-readable one-liner for UI captions.
    pub const fn display_name(&self) -> &'static str {
        match self {
            Self::Seed => "Extract seed entity list (Stage 1a)",
            Self::Extract => "Extract per-section atlas (Phase 1)",
            Self::Cluster => "Cluster sketches by facet (Phase 2)",
            Self::Name => "Name clusters per facet (Phase 3)",
            Self::Resolve => "Resolve atoms + edges + trajectories (Phase 3a/3b)",
            Self::Tensions => "Select tension candidates (Phase 6)",
            Self::Gaps => "Detect structural gaps (Phase 7)",
            Self::Configure => "Identify interpretive configurations (Phase 8)",
            Self::Report => "§12 schema validation",
            Self::Backfill => "Embed atoms into the ANN seed table (grounding)",
        }
    }

    /// Which underlying `PipelinePhase` (if any) this step drives.
    /// `Report` has no phase of its own — it reads every cached
    /// phase to assemble the validation table. `Resolve` spans two
    /// phases (3a + 3b) so it has no single mapping either; we
    /// report it under `Questions` because its inputs come from the
    /// Phase 1 cache.
    pub const fn pipeline_phase(&self) -> Option<PipelinePhase> {
        match self {
            Self::Seed => Some(PipelinePhase::SeedExtraction),
            Self::Extract => Some(PipelinePhase::Questions),
            Self::Cluster => Some(PipelinePhase::AtlasClusters),
            Self::Name => Some(PipelinePhase::AtlasNamedClusters),
            Self::Resolve => None,
            Self::Tensions => Some(PipelinePhase::Tensions),
            Self::Gaps => Some(PipelinePhase::Gaps),
            Self::Configure => None,
            Self::Report => None,
            Self::Backfill => None,
        }
    }
}

/// The one line-protocol for streaming [`EnrichProgress`] between a process
/// that runs a build and a process that watches one.
///
/// # Why this exists
///
/// `quality/TOPOLOGY.md` §9.3, hazard 7. `sovereign-tools`'s subprocess runner
/// rebuilt this event stream by REGEX-MATCHING the CLI's human banners — nine
/// `parse_*` functions against lines like `─── [3/9] extract ───`. The failure
/// mode is the silent one ARCH §7.2 names: someone rewords a banner for a
/// human, the desktop's progress panel quietly stops advancing, and there is
/// no compiler and no test in between. The banners are PROSE and prose is not
/// an interface.
///
/// This module's own header has claimed since it was written that "a future
/// headless mode can filter on `serde_json::Value` without string-matching".
/// The events were `Serialize` and `#[serde(tag = "kind")]` from the start —
/// the wire was designed and simply never used. This is it, in one place, so
/// the writer and the reader cannot drift (ARCH §10.6).
///
/// §9.3 itself — `enrich build` as a CALL, not a subprocess — is closed for
/// the daemon since ontology-v1 P0.4, without moving the 14-module orchestrator
/// subtree below `sovereign-tools`: the tools crate declares
/// `local_corpus::watched::enrich::AtlasBuildRunner`, the daemon (which links
/// `sovereign-cli-llm` as a library) implements it, and `enrich_now` on an
/// `[enrichment] type = "atlas"` recipe runs the build in-process. This wire
/// remains the contract for the subprocess path, which dev boxes without the
/// builder installed still take.
pub mod wire {
    use super::EnrichProgress;

    /// Prefix on every machine-readable line, so a line the child writes for a
    /// human can never be mistaken for an event. Anything without it is
    /// diagnostic output and is passed through untouched.
    pub const PREFIX: &str = "@progress ";

    /// The environment variable a parent sets to ask for this format.
    ///
    /// An environment variable rather than a CLI flag because the parent may
    /// resolve an OLDER `sovereign-cli` from `$PATH` (see
    /// `sovereign_tools::enrich::resolve_sovereign_cli`, which walks four
    /// ladders). An unknown flag makes that binary exit 2 on a usage error —
    /// a build that used to work now fails outright — whereas an unknown env
    /// var is ignored and the parent sees zero events and says so.
    pub const REQUEST_ENV: &str = "SOVEREIGN_ENRICH_PROGRESS";

    /// The value of [`REQUEST_ENV`] that turns this on.
    pub const REQUEST_VALUE: &str = "json";

    /// Render one event as a single line. Never fails: an event that will not
    /// serialise would be a bug in this crate, and dropping it silently is the
    /// substitution the wire exists to prevent, so it degrades to a line the
    /// reader reports as unrecognised rather than to nothing.
    pub fn encode(evt: &EnrichProgress) -> String {
        match serde_json::to_string(evt) {
            Ok(json) => format!("{PREFIX}{json}"),
            Err(e) => format!("{PREFIX}{{\"kind\":\"unencodable\",\"error\":\"{e}\"}}"),
        }
    }

    /// Decode one line. `None` for any line that is not an event — a human
    /// banner, a blank line, a warning — which the caller keeps rather than
    /// discards.
    pub fn decode(line: &str) -> Option<EnrichProgress> {
        let json = line.trim_start().strip_prefix(PREFIX)?;
        serde_json::from_str(json).ok()
    }
}

/// One event in the `enrich build` progress stream.
///
/// Tagged union on `kind` so a JSON observer can `switch` on one
/// field. Every variant carries `corpus_id` so a UI rendering
/// events from multiple concurrent builds can route correctly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnrichProgress {
    /// Build started. Carries the full planned step list so the UI
    /// can render a progress bar with known total *before* the
    /// first step finishes.
    BuildStart {
        corpus_id: String,
        pipeline_id: String,
        steps: Vec<BuildStep>,
        /// Steps the pipeline's capability flags dropped before
        /// the orchestrator started (e.g. `Seed` when
        /// `seed_strategy = None`, `Configure` when
        /// `runs_configuration_phase = false`). Non-empty when the
        /// active pipeline opts out of a step entirely.
        auto_skipped: Vec<BuildStep>,
    },

    /// Transitioning into a step. Emitted before any work for the
    /// step begins.
    StepStart {
        corpus_id: String,
        step: BuildStep,
        /// 1-based ordinal in the enabled-steps sequence so the UI
        /// can render "3/7" without re-counting.
        ordinal: usize,
        total: usize,
    },

    /// Per-chapter progress within Phase 1. Emitted only by the
    /// `Extract` step. Gives the UI per-section granularity on the
    /// longest phase of the build; other steps are coarser.
    ChapterProgress {
        corpus_id: String,
        chapter_id: String,
        /// 1-based position in the extract queue.
        index: usize,
        total: usize,
        /// `Some(n)` when the LLM produced `n` questions for this
        /// chapter; `None` when the chapter is still being
        /// processed or failed (see `kind = chapter_failed`).
        question_count: Option<usize>,
    },

    /// Chapter-level failure captured during Phase 1 (parse error,
    /// chat transport failure, empty extraction). The structured
    /// failure lands in the run file; the event carries a
    /// UI-friendly summary so the progress panel can surface it
    /// inline without re-reading the run file.
    ///
    /// `failure_kind` (not `kind`) because the outer enum is
    /// tagged `#[serde(tag = "kind")]` — a `kind` field inside a
    /// variant would shadow the tag and confuse the serialiser.
    ChapterFailed {
        corpus_id: String,
        chapter_id: String,
        /// Enum discriminator matching `PhaseFailureKind` snake_case.
        failure_kind: String,
        /// Short operator-readable reason (≤ 200 chars).
        reason: String,
    },

    /// Step completed successfully. `summary` is a one-line human
    /// description for the UI ("12 entity atom(s), 22 claim(s),
    /// 5 relation(s)"); details live in the phase's cache file.
    StepDone {
        corpus_id: String,
        step: BuildStep,
        summary: String,
    },

    /// Step failed. `message` is the error the step returned;
    /// `exit_code` matches the CLI's process exit so a caller can
    /// treat the desktop event the same as a subprocess failure.
    StepFailed {
        corpus_id: String,
        step: BuildStep,
        message: String,
        exit_code: i32,
    },

    /// Terminal event on a successful build. Only emitted when
    /// every enabled step passed. The desktop listener cues the
    /// "build complete" toast + refetches the errors aggregator.
    Complete {
        corpus_id: String,
        steps_completed: usize,
    },

    /// Terminal event on a failed build. Paired with the preceding
    /// `StepFailed` — this one tells the listener "no more events
    /// coming, tear down the progress UI".
    Aborted {
        corpus_id: String,
        /// The step that stopped the flow.
        failed_step: BuildStep,
        exit_code: i32,
    },

    /// Terminal event when the build couldn't start at all (the
    /// CLI binary wasn't on `$PATH`, a permission error on spawn,
    /// etc.). Distinct from `Aborted` because no step ran — the
    /// UI should surface "couldn't start" rather than attribute
    /// the failure to `Seed` or any other step.
    SpawnFailed { corpus_id: String, message: String },

    /// Terminal event when a user-initiated cancellation killed
    /// the build mid-flight. Distinct from `Aborted` (a real
    /// failure) and `SpawnFailed` (never got started) so the UI
    /// can render "Cancelled" without string-sniffing the spawn
    /// error message. `failed_step` carries the step that was
    /// running when the cancel fired, `None` if the build
    /// hadn't reached its first step yet.
    Cancelled {
        corpus_id: String,
        at_step: Option<BuildStep>,
    },
}

impl EnrichProgress {
    /// The corpus this event belongs to, for router keying.
    pub fn corpus_id(&self) -> &str {
        match self {
            Self::BuildStart { corpus_id, .. }
            | Self::StepStart { corpus_id, .. }
            | Self::ChapterProgress { corpus_id, .. }
            | Self::ChapterFailed { corpus_id, .. }
            | Self::StepDone { corpus_id, .. }
            | Self::StepFailed { corpus_id, .. }
            | Self::Complete { corpus_id, .. }
            | Self::Aborted { corpus_id, .. }
            | Self::SpawnFailed { corpus_id, .. }
            | Self::Cancelled { corpus_id, .. } => corpus_id,
        }
    }

    /// Adapt a Phase 1 failure captured by the runner into the
    /// `ChapterFailed` variant. Used by the orchestrator so it
    /// doesn't have to re-serialise Phase 1 failures by hand.
    pub fn from_phase1_failure(corpus_id: &str, f: &PhaseFailure) -> Self {
        let kind = match f.kind {
            super::types::PhaseFailureKind::ThinkTruncated => "think_truncated",
            super::types::PhaseFailureKind::ParseDrift => "parse_drift",
            super::types::PhaseFailureKind::ChatError => "chat_error",
            super::types::PhaseFailureKind::DeadlineExceeded => "deadline_exceeded",
            super::types::PhaseFailureKind::EmptyExtraction => "empty_extraction",
            super::types::PhaseFailureKind::Skipped => "skipped",
            super::types::PhaseFailureKind::UnresolvedEntityName => "unresolved_entity_name",
            super::types::PhaseFailureKind::EntityMergeAmbiguous => "entity_merge_ambiguous",
            super::types::PhaseFailureKind::UnresolvedRelationParticipant => {
                "unresolved_relation_participant"
            }
            super::types::PhaseFailureKind::UnresolvedClaimAttribution => {
                "unresolved_claim_attribution"
            }
            super::types::PhaseFailureKind::EndpointTypeMismatch => "endpoint_type_mismatch",
            super::types::PhaseFailureKind::UnresolvedClaimSubject => "unresolved_claim_subject",
            super::types::PhaseFailureKind::UnresolvedAttributeRef => "unresolved_attribute_ref",
            super::types::PhaseFailureKind::NoClusterableItems => "no_clusterable_items",
            super::types::PhaseFailureKind::ClusterNamingFailed => "cluster_naming_failed",
            super::types::PhaseFailureKind::Other => "other",
        };
        // `subject` may carry a "chapter:<id>" prefix; strip it so
        // the UI shows the bare id. Non-chapter subjects (resolution
        // drops, for instance) shouldn't normally reach this helper
        // but we handle them gracefully rather than asserting.
        let chapter_id = f
            .subject
            .strip_prefix("chapter:")
            .unwrap_or(f.subject.as_str())
            .to_string();
        Self::ChapterFailed {
            corpus_id: corpus_id.to_string(),
            chapter_id,
            failure_kind: kind.to_string(),
            reason: truncate_reason(&f.reason),
        }
    }
}

fn truncate_reason(s: &str) -> String {
    const CAP: usize = 200;
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= CAP {
        flat
    } else {
        flat.chars().take(CAP - 1).collect::<String>() + "…"
    }
}

/// Progress-emitting callback type used by the orchestrator.
///
/// Boxed + `Send + Sync + 'static` so a callback can be shared
/// between a spawned tokio task (desktop path) and the
/// synchronous CLI printer that constructs it. Cheap: one
/// allocation at build start, passed by reference to each step.
pub type EnrichProgressFn = std::sync::Arc<dyn Fn(EnrichProgress) + Send + Sync + 'static>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_step_ids_are_stable_and_unique() {
        // The CLI accepts these as `--skip <id>` and the UI
        // serialises them in progress events; a rename without
        // updating consumers would silently break both.
        let ids: Vec<_> = [
            BuildStep::Seed,
            BuildStep::Extract,
            BuildStep::Cluster,
            BuildStep::Name,
            BuildStep::Resolve,
            BuildStep::Tensions,
            BuildStep::Gaps,
            BuildStep::Configure,
            BuildStep::Report,
            BuildStep::Backfill,
        ]
        .iter()
        .map(|s| s.id())
        .collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "step ids must be unique");
    }

    #[test]
    fn enrich_progress_tagged_on_kind_field() {
        // The desktop listener switches on `kind`. Locking the
        // serde tag here prevents a drive-by refactor from
        // reshaping the JSON into something the UI can't route.
        let evt = EnrichProgress::StepStart {
            corpus_id: "bk".into(),
            step: BuildStep::Extract,
            ordinal: 2,
            total: 7,
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(json.contains("\"kind\":\"step_start\""));
        assert!(json.contains("\"step\":\"extract\""));
        assert!(json.contains("\"corpus_id\":\"bk\""));
    }

    #[test]
    fn corpus_id_accessor_covers_every_variant() {
        for evt in sample_events() {
            assert_eq!(evt.corpus_id(), "bk");
        }
    }

    fn sample_events() -> Vec<EnrichProgress> {
        vec![
            EnrichProgress::BuildStart {
                corpus_id: "bk".into(),
                pipeline_id: "literary_atlas".into(),
                steps: vec![BuildStep::Seed, BuildStep::Extract],
                auto_skipped: vec![],
            },
            EnrichProgress::StepStart {
                corpus_id: "bk".into(),
                step: BuildStep::Extract,
                ordinal: 1,
                total: 2,
            },
            EnrichProgress::ChapterProgress {
                corpus_id: "bk".into(),
                chapter_id: "sec_0001".into(),
                index: 1,
                total: 5,
                question_count: Some(3),
            },
            EnrichProgress::ChapterFailed {
                corpus_id: "bk".into(),
                chapter_id: "sec_0002".into(),
                failure_kind: "parse_drift".into(),
                reason: "parse error".into(),
            },
            EnrichProgress::StepDone {
                corpus_id: "bk".into(),
                step: BuildStep::Extract,
                summary: "5/5 chapters extracted".into(),
            },
            EnrichProgress::StepFailed {
                corpus_id: "bk".into(),
                step: BuildStep::Cluster,
                message: "oops".into(),
                exit_code: 1,
            },
            EnrichProgress::Complete {
                corpus_id: "bk".into(),
                steps_completed: 7,
            },
            EnrichProgress::Aborted {
                corpus_id: "bk".into(),
                failed_step: BuildStep::Cluster,
                exit_code: 1,
            },
        ]
    }

    #[test]
    fn truncate_reason_collapses_whitespace_and_caps_at_200_chars() {
        let long = "a ".repeat(300); // 600 chars, lots of whitespace
        let out = truncate_reason(&long);
        assert!(out.chars().count() <= 200);
        // Hidden edge case: when truncation fires we append …, so
        // the cap includes that character — pin it explicitly.
        assert!(out.ends_with('…'));
    }

    #[test]
    fn from_phase1_failure_strips_chapter_prefix_from_subject() {
        use super::super::types::{PhaseFailure, PhaseFailureKind, PipelinePhase};
        let f = PhaseFailure {
            phase: PipelinePhase::Questions,
            subject: "chapter:sec_0017".into(),
            kind: PhaseFailureKind::ParseDrift,
            reason: "parse error x".into(),
            raw_response_head: None,
        };
        let evt = EnrichProgress::from_phase1_failure("bk", &f);
        match evt {
            EnrichProgress::ChapterFailed {
                chapter_id,
                failure_kind,
                ..
            } => {
                assert_eq!(chapter_id, "sec_0017");
                assert_eq!(failure_kind, "parse_drift");
            }
            _ => panic!("expected ChapterFailed variant"),
        }
    }
}
