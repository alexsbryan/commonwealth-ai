// SPDX-License-Identifier: AGPL-3.0-or-later
//! Capability vocabulary (v0.3 §§3.1/4/5): capability hints, latency
//! classes, per-model claims, and the runtime-internal proficiency
//! profile used to derive them.

use std::collections::HashMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[cfg(doc)]
use crate::scoring::{effective_affinity, score_with_adjustments};

// -----------------------------------------------------------------
// Internal model-metadata vocabulary
//
// `Capability`, `CapabilityProfile`, `ProficiencyLevel`, and
// `proficiency` are **not** on the v0.3 wire. They're the shared
// vocabulary the runtime uses internally to describe what a model is
// good at (sourced from `models.toml`, skill TOML declarations, etc.)
// and to derive claim affinities when synthesizing a `ProviderModel`'s
// `claims` vector for advertisement. Prefer these over ad-hoc
// per-crate enums so the vocabulary stays consistent across the
// Sovereign runtime, Commonwealth's model registry, and skill
// authoring.
// -----------------------------------------------------------------

/// Capability domains a model can be proficient in. Local metadata
/// only — not part of the OICP wire (the wire uses
/// [`CapabilityHint`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    General,
    Code,
    Analysis,
    Math,
    Creative,
    Instruction,
    Multilingual,
    Vision,
    LongContext,
    #[serde(other)]
    Unknown,
}

/// Proficiency level on a 0–4 ordinal scale. 0 = None, 1 = Basic,
/// 2 = Moderate, 3 = Strong, 4 = Exceptional.
pub type ProficiencyLevel = u8;

/// A map from capability domains to proficiency levels. Used by the
/// runtime to describe loaded models internally; translated to v0.3
/// [`CapabilityClaim`]s at advertisement time.
pub type CapabilityProfile = HashMap<Capability, ProficiencyLevel>;

/// Returns the proficiency for a capability, defaulting to 0 if
/// absent or if the capability deserialized to `Unknown`.
pub fn proficiency(profile: &CapabilityProfile, cap: Capability) -> ProficiencyLevel {
    if matches!(cap, Capability::Unknown) {
        return 0;
    }
    profile.get(&cap).copied().unwrap_or(0)
}

/// Derive a [`CapabilityHint`] from a [`CapabilityProfile`].
///
/// A model is considered code-specialized only when its `code`
/// proficiency is `Exceptional` (4) **and** that proficiency
/// strictly exceeds its `general` proficiency — i.e., the model is
/// meaningfully better at code than at general work. Everything else
/// maps to `general`.
///
/// This is deliberately strict, mirroring v0.3 §4.4 ("start only
/// with clearly differentiated specializations"). A well-rounded 9B
/// with `general: 3, code: 3, analysis: 3` is **not** a code
/// specialist; it handles code adequately alongside everything else
/// and should route on the general hint. Only a model that is
/// measurably better at code than general — Qwen Coder, DeepSeek
/// Coder, Code Llama variants — earns the `code` hint.
pub fn infer_hint_from_profile(profile: &CapabilityProfile) -> CapabilityHint {
    let code = proficiency(profile, Capability::Code);
    let general = proficiency(profile, Capability::General);
    if code == 4 && code > general {
        CapabilityHint::code()
    } else {
        CapabilityHint::general()
    }
}

// -----------------------------------------------------------------
// v0.3 §4 — Capability Hints
// -----------------------------------------------------------------

/// A capability hint naming a kind of inference work (§4).
///
/// Two hints are standardized at protocol launch: `general` and `code`.
/// Any other specialization (prose, biomedical, math, dialogue, …)
/// starts as an extension hint, which must carry the `x:` prefix so it
/// is distinguishable at parse time from a standardized hint.
///
/// # Forward compatibility
///
/// Parsing is deliberately permissive: any non-empty, whitespace-free
/// string is accepted. A bare hint that isn't currently standardized
/// in this build (e.g., because a future spec promoted `math` to
/// standardized and this client predates the bump) is preserved
/// verbatim. The scheduler may still match it by exact string against
/// advertised claims; where no match exists it falls back to `general`
/// per §4.2.
///
/// # Construction
///
/// Use [`CapabilityHint::general`] and [`CapabilityHint::code`] for
/// the standardized constants, [`CapabilityHint::extension`] to wrap
/// an extension tag (the `x:` prefix is applied automatically), and
/// [`CapabilityHint::parse`] to validate a raw string received over
/// the wire.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapabilityHint(String);

impl CapabilityHint {
    /// Standardized hint: no specific specialization target. Every
    /// node serving inference supports this hint; every request
    /// without a more specific hint uses this (§4.1).
    pub const GENERAL: &'static str = "general";

    /// Standardized hint: code generation, understanding,
    /// modification, review. Models tuned for code emphasis serve
    /// this with higher affinity than general-purpose models (§4.1).
    pub const CODE: &'static str = "code";

    /// Extension prefix: extension hints must start with `x:` (§4.2).
    /// Chosen over `ext/` or `@ns/` for brevity and because `:`
    /// never appears in a standardized hint.
    pub const EXTENSION_PREFIX: &'static str = "x:";

    /// Known standardized hints as of this crate build. Grows by
    /// governance decision (§4.3); do not extend locally.
    pub const STANDARDIZED: &'static [&'static str] = &[Self::GENERAL, Self::CODE];

    /// The standardized `general` hint.
    pub fn general() -> Self {
        Self(Self::GENERAL.to_string())
    }

    /// The standardized `code` hint.
    pub fn code() -> Self {
        Self(Self::CODE.to_string())
    }

    /// Wrap `tag` as an extension hint, prepending the `x:` prefix.
    ///
    /// Rejects empty/whitespace tags, tags that already carry the
    /// prefix, tags that collide with a standardized hint, and tags
    /// containing whitespace.
    pub fn extension(tag: impl AsRef<str>) -> Result<Self, InvalidCapabilityHint> {
        let tag = tag.as_ref().trim();
        if tag.is_empty() {
            return Err(InvalidCapabilityHint::Empty);
        }
        if tag.starts_with(Self::EXTENSION_PREFIX) {
            return Err(InvalidCapabilityHint::AlreadyPrefixed);
        }
        if tag.chars().any(|c| c.is_whitespace()) {
            return Err(InvalidCapabilityHint::Whitespace);
        }
        if Self::STANDARDIZED.contains(&tag) {
            return Err(InvalidCapabilityHint::CollidesWithStandardized);
        }
        Ok(Self(format!("{}{}", Self::EXTENSION_PREFIX, tag)))
    }

    /// Parse a raw hint string (e.g., from a manifest or request).
    ///
    /// Accepts any non-empty, whitespace-free string. Rejection is
    /// reserved for structurally unusable values — bare strings that
    /// aren't currently standardized here are preserved verbatim so
    /// forward-compatible matching still works (§10.3).
    pub fn parse(raw: impl AsRef<str>) -> Result<Self, InvalidCapabilityHint> {
        let raw = raw.as_ref().trim();
        if raw.is_empty() {
            return Err(InvalidCapabilityHint::Empty);
        }
        if raw.chars().any(|c| c.is_whitespace()) {
            return Err(InvalidCapabilityHint::Whitespace);
        }
        Ok(Self(raw.to_string()))
    }

    /// The hint's wire form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True iff this hint is in [`STANDARDIZED`](Self::STANDARDIZED).
    pub fn is_standardized(&self) -> bool {
        Self::STANDARDIZED.iter().any(|s| *s == self.0)
    }

    /// True iff this hint carries the `x:` extension prefix and has
    /// a non-empty tag component.
    pub fn is_extension(&self) -> bool {
        self.0.starts_with(Self::EXTENSION_PREFIX) && self.0.len() > Self::EXTENSION_PREFIX.len()
    }

    /// True iff this hint is neither standardized in this build nor
    /// an extension — likely a future-standardized hint. Schedulers
    /// should still attempt exact-string matches against advertised
    /// claims before falling back to `general`.
    pub fn is_unknown_bare(&self) -> bool {
        !self.is_standardized() && !self.is_extension()
    }
}

impl std::fmt::Display for CapabilityHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Reasons [`CapabilityHint`] construction can fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidCapabilityHint {
    /// The input was empty or whitespace-only.
    Empty,
    /// The extension tag was given with the `x:` prefix already
    /// attached — [`CapabilityHint::extension`] adds the prefix.
    AlreadyPrefixed,
    /// The input contained internal whitespace.
    Whitespace,
    /// The extension tag equals a standardized hint; use
    /// [`CapabilityHint::general`] / [`CapabilityHint::code`].
    CollidesWithStandardized,
}

impl std::fmt::Display for InvalidCapabilityHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::Empty => "capability hint is empty",
            Self::AlreadyPrefixed => "capability hint already carries the 'x:' extension prefix",
            Self::Whitespace => "capability hint contains whitespace",
            Self::CollidesWithStandardized => "extension tag collides with a standardized hint",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for InvalidCapabilityHint {}

impl Serialize for CapabilityHint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CapabilityHint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

// -----------------------------------------------------------------
// v0.3 §5 — Latency Classes
// -----------------------------------------------------------------

/// Time-sensitivity class for a request or a node's typical response
/// time for a kind of work (§5).
///
/// These are categories, not precise SLAs. A node advertises the
/// class matching its typical behaviour; a request names the class
/// it needs. The scheduler prefers matching classes and treats
/// mismatch as a soft deprioritization rather than a hard failure
/// (§6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyClass {
    /// Time-to-first-token in hundreds of milliseconds. Suitable for
    /// routing, classification, short extractions, interactive UI.
    Fast,
    /// Time-to-first-token in single-digit seconds. Suitable for
    /// most substantive inference work. Default.
    #[default]
    Normal,
    /// TTFT may be longer; total generation may span tens of
    /// seconds or more. Suitable for reasoning-heavy work,
    /// long-context synthesis, deep planning.
    Extended,
}

// -----------------------------------------------------------------
// v0.3 §3.1 — Capability Claims
// -----------------------------------------------------------------

/// A capability claim: what kind of inference work this node serves,
/// with a stated affinity (§3.1).
///
/// A node may publish multiple claims — one per (model, latency
/// class) combination, or one per model for nodes running a single
/// model. A claim is the unit of scheduling: the scheduler ranks
/// (node, claim) pairs against a request's property set (§6).
///
/// `affinity` is the node's self-assessment of how well it serves
/// this kind of work, clamped to `[0.0, 1.0]` with documented
/// reference points:
///
/// - `1.0` — exceptional fit (specialized model serving its specialty).
/// - `~0.85` — strong fit (large general model serving general work).
/// - `~0.7` — solid fit (small fast model serving fast work well).
/// - `~0.5` — adequate fit (model can serve but prefer elsewhere).
/// - below `0.5` — feasible but substantially degraded.
///
/// Affinity is self-reported and therefore less reliable than
/// structural facts (context / output capacity, hint match). The
/// scheduler treats it as a tiebreaker, not a primary ranker.
///
/// **Reality note (so maintainers aren't surprised):** in this
/// codebase the advertised affinity is a STATIC value derived from
/// model-profile config at startup (`models.toml` proficiencies ÷ 4)
/// — there is no feedback loop that re-advertises a different
/// affinity after observing failures. The "observed health" the
/// spec's §7 describes happens entirely SCORER-side, at request
/// time, via [`effective_affinity`] inside
/// [`score_with_adjustments`]; peers always see the original claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityClaim {
    /// The kind of work this claim covers.
    pub hint: CapabilityHint,
    /// The latency class this claim covers.
    pub latency_class: LatencyClass,
    /// Maximum context (input) tokens the model accepts for this claim.
    pub max_context: u32,
    /// Maximum output tokens the model will produce for this claim.
    pub max_output: u32,
    /// Self-assessed affinity in `[0.0, 1.0]`. Accessed via
    /// [`CapabilityClaim::effective_affinity`] to apply the clamp.
    pub affinity: f32,
}

impl CapabilityClaim {
    /// Construct a claim. `affinity` is stored verbatim; use
    /// [`effective_affinity`](Self::effective_affinity) for the
    /// clamped-to-`[0.0, 1.0]` value used in scheduling.
    pub fn new(
        hint: CapabilityHint,
        latency_class: LatencyClass,
        max_context: u32,
        max_output: u32,
        affinity: f32,
    ) -> Self {
        Self {
            hint,
            latency_class,
            max_context,
            max_output,
            affinity,
        }
    }

    /// Affinity clamped to `[0.0, 1.0]` per §3.1 semantics. NaN
    /// collapses to 0.0.
    pub fn effective_affinity(&self) -> f32 {
        if self.affinity.is_nan() {
            0.0
        } else {
            self.affinity.clamp(0.0, 1.0)
        }
    }

    /// True iff this claim can fit the structural size of a request
    /// (§6 — context/output are hard constraints).
    pub fn fits(&self, context_tokens: u32, max_output_tokens: u32) -> bool {
        self.max_context >= context_tokens && self.max_output >= max_output_tokens
    }
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ───── CapabilityHint ─────────────────────────────────────

    #[test]
    fn capability_hint_standardized_constructors() {
        let g = CapabilityHint::general();
        assert_eq!(g.as_str(), "general");
        assert!(g.is_standardized());
        assert!(!g.is_extension());

        let c = CapabilityHint::code();
        assert_eq!(c.as_str(), "code");
        assert!(c.is_standardized());
    }

    #[test]
    fn capability_hint_extension_applies_prefix() {
        let h = CapabilityHint::extension("prose").expect("valid tag");
        assert_eq!(h.as_str(), "x:prose");
        assert!(h.is_extension());
        assert!(!h.is_standardized());
    }

    #[test]
    fn capability_hint_extension_rejects_malformed() {
        assert_eq!(
            CapabilityHint::extension("").unwrap_err(),
            InvalidCapabilityHint::Empty
        );
        assert_eq!(
            CapabilityHint::extension("x:prose").unwrap_err(),
            InvalidCapabilityHint::AlreadyPrefixed
        );
        assert_eq!(
            CapabilityHint::extension("a b").unwrap_err(),
            InvalidCapabilityHint::Whitespace
        );
        assert_eq!(
            CapabilityHint::extension("general").unwrap_err(),
            InvalidCapabilityHint::CollidesWithStandardized
        );
        assert_eq!(
            CapabilityHint::extension("code").unwrap_err(),
            InvalidCapabilityHint::CollidesWithStandardized
        );
    }

    #[test]
    fn capability_hint_parse_accepts_standardized_extension_and_future() {
        assert_eq!(
            CapabilityHint::parse("general").unwrap().as_str(),
            "general"
        );
        assert_eq!(CapabilityHint::parse("code").unwrap().as_str(), "code");

        let ext = CapabilityHint::parse("x:biomed").unwrap();
        assert_eq!(ext.as_str(), "x:biomed");
        assert!(ext.is_extension());

        let future = CapabilityHint::parse("math").unwrap();
        assert_eq!(future.as_str(), "math");
        assert!(future.is_unknown_bare());

        assert!(CapabilityHint::parse("").is_err());
        assert!(CapabilityHint::parse("has space").is_err());
    }

    #[test]
    fn capability_hint_serde_round_trips_as_plain_string() {
        let hints = vec![
            CapabilityHint::general(),
            CapabilityHint::code(),
            CapabilityHint::extension("prose").unwrap(),
            CapabilityHint::parse("math_future").unwrap(),
        ];
        for h in hints {
            let json = serde_json::to_string(&h).unwrap();
            assert!(json.starts_with('"') && json.ends_with('"'), "got {json}");
            let back: CapabilityHint = serde_json::from_str(&json).unwrap();
            assert_eq!(back, h);
        }
    }

    #[test]
    fn latency_class_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&LatencyClass::Fast).unwrap(),
            "\"fast\""
        );
        assert_eq!(
            serde_json::to_string(&LatencyClass::Normal).unwrap(),
            "\"normal\""
        );
        assert_eq!(
            serde_json::to_string(&LatencyClass::Extended).unwrap(),
            "\"extended\""
        );
        assert_eq!(LatencyClass::default(), LatencyClass::Normal);
    }

    #[test]
    fn capability_claim_fits_gate() {
        let claim = CapabilityClaim::new(
            CapabilityHint::general(),
            LatencyClass::Normal,
            16_000,
            4_000,
            0.85,
        );
        assert!(claim.fits(10_000, 1_000));
        assert!(claim.fits(16_000, 4_000));
        assert!(!claim.fits(16_001, 1_000));
        assert!(!claim.fits(10_000, 4_001));
    }

    #[test]
    fn capability_claim_effective_affinity_clamps() {
        let mk = |a| {
            CapabilityClaim::new(
                CapabilityHint::general(),
                LatencyClass::Normal,
                4_000,
                1_000,
                a,
            )
        };
        assert!((mk(0.7).effective_affinity() - 0.7).abs() < 1e-6);
        assert_eq!(mk(-0.2).effective_affinity(), 0.0);
        assert_eq!(mk(1.5).effective_affinity(), 1.0);
        assert_eq!(mk(f32::NAN).effective_affinity(), 0.0);
    }

    #[test]
    fn capability_claim_serde_round_trip() {
        let claim = CapabilityClaim::new(
            CapabilityHint::extension("biomed").unwrap(),
            LatencyClass::Extended,
            32_000,
            8_000,
            0.9,
        );
        let json = serde_json::to_string(&claim).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["hint"], "x:biomed");
        assert_eq!(v["latency_class"], "extended");
        assert_eq!(v["max_context"], 32_000);
        assert_eq!(v["max_output"], 8_000);

        let back: CapabilityClaim = serde_json::from_str(&json).unwrap();
        assert_eq!(back.hint, claim.hint);
        assert_eq!(back.latency_class, claim.latency_class);
    }
}
