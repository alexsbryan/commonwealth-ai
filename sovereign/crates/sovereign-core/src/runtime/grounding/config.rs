// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate configuration: the closed set of gated surfaces, per-surface
//! verification budgets, and the env-flag registry (SSOT for every
//! knob the gate reads — mirrors `retrieval_pipeline_flags()`).

use crate::runtime::retrieval_pipeline::EnvFlag;

/// Whether `SOVEREIGN_AGENTIC_KQ_DEBUG` is set — the opt-in switch for the
/// gate's glassbox extras: the `dbg()` stderr/tracing mirror AND recording the
/// pre-gate draft into message metadata (see `gate_answer`). Default off, so a
/// production message never carries the rejected draft, which can be the very
/// confabulation the gate just suppressed. Cached once.
pub(crate) fn debug_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("SOVEREIGN_AGENTIC_KQ_DEBUG")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

/// Stderr mirror for bench/CLI surfaces that install no tracing
/// subscriber — same pattern (and same env var) as the agentic
/// loop's dbg().
pub(crate) fn dbg(msg: &str) {
    if debug_enabled() {
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
        // Default ON: the attach-mode QA bank justified the flip (2026-06-24).
        // Pooled iter8+9 over the resident corpora: answers the verifier grounded
        // with a copied quote broke at 3.6% vs 25.9% for ungrounded ones — a 7x
        // reduction, because the model COPIES the supporting span instead of
        // confabulating the specific. Set SOVEREIGN_CITATION_GROUNDING=0 to A/B off.
        .unwrap_or(true)
}

/// Run quote-first citation grounding on ALL gated factual answers, not just
/// entity-anchored ones. The default `entity_anchored` gate is too strict — the
/// chaos stream tripped it 0 times — so quote-first never got to cure the
/// confabulated-specific class ("Ernest Rhys Jones" for "Ernest Rhys"). Quote-
/// first is ADDITIVE and SAFE where the per-claim rewrite is not: it makes the
/// model COPY a supporting sentence (it can't add a token the quote lacks) or
/// falls through to the legacy ladder — it never re-searches near-miss noise nor
/// rewrites a correct answer. A/B via `SOVEREIGN_CITATION_BROAD`.
pub(crate) fn citation_broad_enabled() -> bool {
    std::env::var("SOVEREIGN_CITATION_BROAD")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        // Default ON (2026-06-24): the chaos stream tripped `entity_anchored` 0
        // times, so without broad the verifier never fires. The fall-through is
        // additive and safe (no clean quote → legacy ladder), so broad-by-default
        // only adds coverage. Set SOVEREIGN_CITATION_BROAD=0 to A/B off.
        .unwrap_or(true)
}

/// Exact-value + GK-fabrication fidelity fixes (2026-07-01). ON by default;
/// `SOVEREIGN_EXACTVAL_FIX=0` restores the prior behaviour for a clean replay
/// A/B. Gates two changes together (both target the same exact-value residual):
/// (1) citation `answer_supported_by_quote` requires a numeric answer token to
/// match a COMPLETE digit-run in the quote, not a substring (kills truncated-
/// number grounding, "289494" vs "28949423"); (2) `gate_answer` strips the GK
/// caveat UNCONDITIONALLY before verifying (the gated path always has retrieved
/// docs, so a "from general knowledge" escape hatch must be held to the evidence,
/// not exempted as NO_CLAIM — kills confident GK fabrication like "Eddie
/// Henderson").
pub(crate) fn exactval_fix_enabled() -> bool {
    !matches!(
        std::env::var("SOVEREIGN_EXACTVAL_FIX").ok().as_deref(),
        Some("0") | Some("false") | Some("off")
    )
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
    /// Proxy-voting Q&A over a company's ballot (SEC DEF 14A). State the
    /// sides of a proposal from the filing's verbatim text or abstain —
    /// its own surface so the proxy bank (RL-1: no confabulated
    /// opposition for a management item; RL-2: both sides cited for a
    /// shareholder proposal) calibrates the gate independently. Mirrors
    /// the Governance discipline; the bank and override var are what make
    /// it separately measured.
    ProxyArgument,
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
            GateSurface::ProxyArgument => "proxy_argument",
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
            GateSurface::ProxyArgument => "SOVEREIGN_GROUNDING_GATE_PROXY_ARGUMENT",
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
            // Proxy answers are short statements of a ballot item's sides
            // grounded in the filing's verbatim text — cite both sides
            // (or the single side present) or abstain. Same budget +
            // discipline as Governance; `retry` on so a failed verify
            // becomes an honest abstention ("the filing carries only the
            // board's recommendation"), never a fabricated against-case.
            GateSurface::ProxyArgument => GroundingProfile {
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
                purpose: "Per-surface override (=1 forces on, =0 forces off); SURFACE ∈ {KNOWLEDGE_QUERY, DEEP_QUERY, ATTACHED_DOC, COMPLEX_TASK, SIMPLE_QUERY, REFINEMENT, GOVERNANCE, PROXY_ARGUMENT}.",
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
        (
            "gate",
            EnvFlag {
                name: "SOVEREIGN_SPECIFICS_SCAN",
                default: "on",
                purpose: "Long-form holistic specifics scan inside gate_longform: one judge pass (whole answer vs full evidence) catching fabricated supporting specifics / misattributions the per-claim audit misses. =0 disables (clean A/B lever).",
            },
        ),
        (
            "gate",
            EnvFlag {
                name: "SOVEREIGN_SHORT_SPECIFICS_SCAN",
                default: "off",
                purpose: "SHELVED (default off; =1 enables). Short-path second-opinion specifics scan on RELEASED single-claim/citation answers: catches fabricated cited specifics (a named entity/flag/number absent from evidence) the value-only verify waves through, then correct-or-abstains via one grounded rewrite. Skips abstention-shaped answers. Dormant pending clean-evidence validation — its target category proved ~90% measurement artifact.",
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
            GateSurface::ProxyArgument,
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
            GateSurface::ProxyArgument,
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
