// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate configuration: the closed set of gated surfaces, per-surface
//! verification budgets, and the env-flag registry (SSOT for every
//! knob the gate reads — mirrors `retrieval_pipeline_flags()`).

use crate::runtime::retrieval_pipeline::EnvFlag;

/// Stderr mirror for bench/CLI surfaces that install no tracing
/// subscriber — same pattern (and same env var) as the agentic
/// loop's dbg().
pub(crate) fn dbg(msg: &str) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let on = *ON.get_or_init(|| {
        std::env::var("SOVEREIGN_AGENTIC_KQ_DEBUG")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    });
    if on {
        eprintln!("    [gate] {msg}");
        // Also emit via tracing: a DETACHED daemon discards stderr, so eprintln
        // never reaches daemon.err — the gate was invisible in the deployed path.
        // Use the DEFAULT target (this module = `sovereign_core::…`), which
        // matches the daemon's crate-scoped filter (`sovereign_core=info`); a
        // custom `target:` would be filtered out. (2026-06-18 glassbox fix.)
        tracing::info!("[gate] {msg}");
    }
}

/// The grounding verification contract is ON by default — it is the
/// "Grounded Everywhere" promise (desktop chat and every other
/// answer-producing surface ship with it live), not an opt-in env flag.
/// Only an explicit `SOVEREIGN_GROUNDING_GATE=0` / `false` turns it off
/// (naked benches, latency debugging); unset — or any other value —
/// leaves it on. Per-surface overrides (`SOVEREIGN_GROUNDING_GATE_<SURFACE>`)
/// still win over this global default, see `GateSurface::enabled`.
pub(crate) fn grounding_gate_enabled() -> bool {
    std::env::var("SOVEREIGN_GROUNDING_GATE")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
        .unwrap_or(true)
}

pub(crate) fn grounding_gate_threshold() -> f64 {
    std::env::var("SOVEREIGN_GV_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.9)
}

/// Citation-grounded answering on entity-anchored fact queries. OFF by default
/// (clean A/B until the bank justifies a flip): when on, the gate replaces
/// generate-then-substring-verify with active quoting — the model must copy the
/// supporting sentence before it answers. See `citation::citation_grounded_answer`.
pub(crate) fn citation_grounding_enabled() -> bool {
    std::env::var("SOVEREIGN_CITATION_GROUNDING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// The closed set of answer-producing surfaces the gate covers.
/// Adding a surface = adding a variant + a profile + a bank — there
/// is no open registration, by design: every gated surface must have
/// shipped with its own measured calibration bank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
// Variants land with their phases (attached_doc P2, complex_task P4,
// simple_query + refinement P6) — declared up front so the closed set
// and its override grammar are reviewable as one unit.
#[allow(dead_code)]
pub(crate) enum GateSurface {
    /// Streaming + non-streaming KnowledgeQuery (share one profile).
    KnowledgeQuery,
    /// Streaming DeepQuery spawn.
    DeepQuery,
    /// Attached-document Q&A (Phase 2).
    AttachedDoc,
    /// Tool-using task synthesis over step transcripts (Phase 4).
    ComplexTask,
    /// Non-streaming simple-query path when retrieval matched (Phase 6).
    SimpleQuery,
    /// Gap-check refinement re-verification (Phase 6; retry off —
    /// the refinement itself was the rewrite).
    Refinement,
    /// Governance Q&A over current law (FR-9). Cite an active rule or
    /// abstain — its own surface so the governance bank (RL-1: no
    /// confabulated rule; RL-2: honest abstention) calibrates the gate
    /// independently of the general KnowledgeQuery banks.
    Governance,
}

impl GateSurface {
    pub(crate) const fn id(self) -> &'static str {
        match self {
            GateSurface::KnowledgeQuery => "knowledge_query",
            GateSurface::DeepQuery => "deep_query",
            GateSurface::AttachedDoc => "attached_doc",
            GateSurface::ComplexTask => "complex_task",
            GateSurface::SimpleQuery => "simple_query",
            GateSurface::Refinement => "refinement",
            GateSurface::Governance => "governance",
        }
    }

    /// Per-surface env override name (e.g.
    /// `SOVEREIGN_GROUNDING_GATE_ATTACHED_DOC`).
    const fn override_var(self) -> &'static str {
        match self {
            GateSurface::KnowledgeQuery => "SOVEREIGN_GROUNDING_GATE_KNOWLEDGE_QUERY",
            GateSurface::DeepQuery => "SOVEREIGN_GROUNDING_GATE_DEEP_QUERY",
            GateSurface::AttachedDoc => "SOVEREIGN_GROUNDING_GATE_ATTACHED_DOC",
            GateSurface::ComplexTask => "SOVEREIGN_GROUNDING_GATE_COMPLEX_TASK",
            GateSurface::SimpleQuery => "SOVEREIGN_GROUNDING_GATE_SIMPLE_QUERY",
            GateSurface::Refinement => "SOVEREIGN_GROUNDING_GATE_REFINEMENT",
            GateSurface::Governance => "SOVEREIGN_GROUNDING_GATE_GOVERNANCE",
        }
    }

    /// Is the gate on for THIS surface? Global `SOVEREIGN_GROUNDING_GATE`
    /// sets the default; the per-surface var overrides in either
    /// direction (=1 forces on, =0 forces off). Per-surface rollout is
    /// the whole point: each surface flips only on its own bank's
    /// evidence.
    pub(crate) fn enabled(self) -> bool {
        match std::env::var(self.override_var()) {
            Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") => true,
            Ok(v) if v == "0" || v.eq_ignore_ascii_case("false") => false,
            _ => grounding_gate_enabled(),
        }
    }

    /// This surface's verification budget. Defaults are pinned by the
    /// `profile_defaults_are_pinned` golden test — change a value
    /// there only together with the bank run that justifies it.
    pub(crate) fn profile(self) -> GroundingProfile {
        let tau = grounding_gate_threshold();
        match self {
            GateSurface::KnowledgeQuery | GateSurface::DeepQuery => GroundingProfile {
                surface: self,
                tau,
                max_claims: 4,
                max_chunks: 8,
                retry: true,
                longform_chars: 1_800,
            },
            GateSurface::AttachedDoc | GateSurface::SimpleQuery => GroundingProfile {
                surface: self,
                tau,
                max_claims: 4,
                max_chunks: 8,
                retry: true,
                longform_chars: 1_800,
            },
            // Synthesis claims assemble across step outputs — the
            // per-chunk max-support check is structurally biased
            // against exactly that, so ComplexTask always takes the
            // per-claim joint-judge ladder.
            GateSurface::ComplexTask => GroundingProfile {
                surface: self,
                tau,
                max_claims: 4,
                max_chunks: 8,
                retry: true,
                longform_chars: 0,
            },
            // The refinement itself was the rewrite: verify only,
            // never re-synthesize. On failure the caller keeps the
            // already-verified original.
            GateSurface::Refinement => GroundingProfile {
                surface: self,
                tau,
                max_claims: 4,
                max_chunks: 8,
                retry: false,
                longform_chars: 1_800,
            },
            // Governance answers are short statements of current law —
            // cite the active rule or abstain. `retry` on so a failed
            // verify becomes RL-2 honest abstention, not a confident
            // guess. Same budget as KnowledgeQuery; the override var and
            // bank are what make it a separately-calibrated surface.
            GateSurface::Governance => GroundingProfile {
                surface: self,
                tau,
                max_claims: 4,
                max_chunks: 8,
                retry: true,
                longform_chars: 1_800,
            },
        }
    }
}

/// HOW MUCH verification one surface budgets. Plain copyable data,
/// not behavior — the ladder in `gate_answer` is the behavior.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GroundingProfile {
    pub surface: GateSurface,
    /// Violation-probability threshold (bench-calibrated 0.9; the
    /// judge prompts are byte-pinned to the bench critic so it
    /// transfers).
    pub tau: f64,
    /// Long-form audit: claims checked per draft.
    pub max_claims: usize,
    /// Passages per claim-verdict judge call (claim-search hits
    /// widen this cap, never displace within it).
    pub max_chunks: usize,
    /// Corrective retry/rewrite allowed (false = verify-only).
    pub retry: bool,
    /// Char pivot between the single-claim and per-claim ladders;
    /// 0 = always per-claim.
    pub longform_chars: usize,
}

/// Every env knob the grounding gate reads — registry-test consumed,
/// doc-table renderable; same pattern as `retrieval_pipeline_flags()`.
/// Human reference (gate + agentic-loop + observability flags, with the
/// canonical chaos-bench invocation): `sovereign/docs/GROUNDING_GATE_ENV.md`.
pub fn grounding_gate_flags() -> Vec<(&'static str, EnvFlag)> {
    vec![
        (
            "gate",
            EnvFlag {
                name: "SOVEREIGN_GROUNDING_GATE",
                default: "on",
                purpose: "Global on/off for the hold→verify→retry→abstain gate on answer-producing surfaces. ON by default (the Grounded-Everywhere contract); set =0 to opt out (naked benches, latency debugging).",
            },
        ),
        (
            "gate",
            EnvFlag {
                name: "SOVEREIGN_GROUNDING_GATE_<SURFACE>",
                default: "unset",
                purpose: "Per-surface override (=1 forces on, =0 forces off); SURFACE ∈ {KNOWLEDGE_QUERY, DEEP_QUERY, ATTACHED_DOC, COMPLEX_TASK, SIMPLE_QUERY, REFINEMENT, GOVERNANCE}.",
            },
        ),
        (
            "gate",
            EnvFlag {
                name: "SOVEREIGN_GV_THRESHOLD",
                default: "0.9",
                purpose: "Violation-probability threshold τ (bench-calibrated; transfers via judge-prompt byte-identity with the bench critic).",
            },
        ),
        (
            "-",
            EnvFlag {
                name: "SOVEREIGN_AGENTIC_KQ_DEBUG",
                default: "off",
                purpose: "Mirror gate (and agentic-loop) trace lines to stderr for bench/CLI surfaces with no tracing subscriber.",
            },
        ),
        (
            "gate",
            EnvFlag {
                name: "SOVEREIGN_CITATION_GROUNDING",
                default: "off",
                purpose: "Active citation-grounding on entity-anchored fact queries: the model must copy a verbatim supporting sentence before answering, grounded by quote-existence (curing A3B context-under-utilisation + the substring verifier's title/paraphrase false-negatives). No findable quote → honest abstention.",
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden pin on every surface's verification budget. A change
    /// here must ship together with the bank run that justifies it.
    #[test]
    fn profile_defaults_are_pinned() {
        for s in [
            GateSurface::KnowledgeQuery,
            GateSurface::DeepQuery,
            GateSurface::AttachedDoc,
            GateSurface::SimpleQuery,
            GateSurface::Governance,
        ] {
            let p = s.profile();
            assert_eq!(p.max_claims, 4, "{}", s.id());
            assert_eq!(p.max_chunks, 8, "{}", s.id());
            assert!(p.retry, "{}", s.id());
            assert_eq!(p.longform_chars, 1_800, "{}", s.id());
        }
        let ct = GateSurface::ComplexTask.profile();
        assert_eq!(ct.longform_chars, 0, "complex_task is always per-claim");
        assert!(ct.retry);
        let rf = GateSurface::Refinement.profile();
        assert!(!rf.retry, "refinement is verify-only");
        assert_eq!(rf.longform_chars, 1_800);
    }

    /// τ default and env override flow through every profile.
    #[test]
    fn tau_defaults_to_calibrated_value() {
        if std::env::var("SOVEREIGN_GV_THRESHOLD").is_err() {
            assert!((GateSurface::KnowledgeQuery.profile().tau - 0.9).abs() < f64::EPSILON);
        }
    }

    /// The registry names every surface the override grammar accepts.
    #[test]
    fn flags_registry_covers_surface_overrides() {
        let flags = grounding_gate_flags();
        let overrides = flags
            .iter()
            .find(|(_, f)| f.name.contains("<SURFACE>"))
            .expect("per-surface override flag registered");
        for s in [
            GateSurface::KnowledgeQuery,
            GateSurface::DeepQuery,
            GateSurface::AttachedDoc,
            GateSurface::ComplexTask,
            GateSurface::SimpleQuery,
            GateSurface::Refinement,
            GateSurface::Governance,
        ] {
            let suffix = s
                .override_var()
                .strip_prefix("SOVEREIGN_GROUNDING_GATE_")
                .unwrap()
                .to_string();
            assert!(
                overrides.1.purpose.contains(&suffix),
                "override grammar must document {suffix}"
            );
        }
    }
}
