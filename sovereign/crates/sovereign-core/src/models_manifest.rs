//! Parser for `lcol-llm/models.toml`.
//!
//! The workspace-root `models.toml` is the source of truth for
//! which GGUF files ship with each hardware profile (cpu_only,
//! low_mem, default, high, very_high) and — as of Stage 2.1 —
//! what OICP capabilities each slot's model declares.
//!
//! Historically models.toml was human-readable documentation for
//! the setup wizard; nothing Rust-side parsed it. This module
//! bundles it via `include_str!` at compile time and exposes:
//!
//! * [`ModelsManifest`] — the structured view.
//! * [`DEFAULT_MANIFEST`] — a process-wide `LazyLock` over the
//!   bundled file, so repeated lookups from the mesh routing path
//!   don't re-parse the TOML.
//! * [`ModelsManifest::capabilities_for_file`] — the hot-path
//!   lookup: given a loaded GGUF filename (with or without the
//!   extension), return the declared `CapabilityProfile`.
//!
//! Scope: this parser reads only the fields the rest of Sovereign
//! cares about today. `quirks_override` tables are tolerated but
//! not deserialised — serde ignores unknown struct fields by
//! default, so a future addition there doesn't break this parser.
use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Deserialize;

use crate::oicp::CapabilityProfile;

/// Root of `models.toml`. Nested exactly how TOML sees it:
///
/// ```text
/// [profiles.<profile>.<slot>]
/// ...
/// [profiles.<profile>.<slot>.capabilities]
/// general = 3
/// analysis = 3
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelsManifest {
    #[serde(default)]
    pub profiles: HashMap<String, ProfileConfig>,
    #[serde(default)]
    pub user_slots: Vec<UserSlotConfig>,
}

/// One of the five hardware profiles in the top-level file. All
/// three slots are optional — a profile might define only the
/// fast slot for a low-spec device.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProfileConfig {
    #[serde(default)]
    pub fast: Option<SlotConfig>,
    #[serde(default)]
    pub thoughtful: Option<SlotConfig>,
    #[serde(default)]
    pub embed: Option<SlotConfig>,
}

/// A single slot's declared model. The fields we read right now
/// are `file` (for matching loaded GGUFs) and `capabilities`
/// (for OICP routing). Everything else is kept as documentation
/// — serde ignores unknown TOML keys for structs by default, so
/// `family`, `quant`, `hf_url`, `quirks_override`, etc. pass
/// through untouched.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SlotConfig {
    pub file: String,
    /// Optional model identity — the stable substring that
    /// uniquely names this base model across quantisations and
    /// repo uploads. `"Qwen3.5-27B"` matches both
    /// `Qwen_Qwen3.5-27B-Q4_K_M.gguf` (bartowski's Q4) and
    /// `Qwen3.5-27B.Q8_0.gguf` (a local Q8 dump) and any future
    /// `*-Q5_K_S.gguf`. When absent, capability lookup falls
    /// back to exact filename match only.
    ///
    /// Declaring `base_name` lets a single manifest row cover
    /// every quantisation of the same weights — capabilities are
    /// a property of the model, not the quant.
    #[serde(default)]
    pub base_name: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub quant: String,
    #[serde(default)]
    pub size_gb: f64,
    #[serde(default)]
    pub thinking: bool,
    #[serde(default)]
    pub hf_url: String,
    /// OICP capability declarations for this base model. Empty
    /// when absent — the mesh routing path falls back to
    /// conservative defaults for BYOM or legacy entries that
    /// haven't been annotated yet.
    #[serde(default)]
    pub capabilities: CapabilityProfile,
}

/// A `[[user_slots]]` entry — "bring your own model" override
/// that replaces one slot of the active profile.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserSlotConfig {
    pub slot: String,
    pub file: String,
    /// Same semantics as `SlotConfig::base_name` — cross-
    /// quantisation identity for this model. Particularly useful
    /// for BYOM users who want their downloads to pick up OICP
    /// annotations without renaming files to match manifest
    /// entries exactly.
    #[serde(default)]
    pub base_name: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub quant: String,
    #[serde(default)]
    pub size_gb: f64,
    #[serde(default)]
    pub thinking: bool,
    #[serde(default)]
    pub hf_url: String,
    #[serde(default)]
    pub capabilities: CapabilityProfile,
}

impl ModelsManifest {
    /// Parse a TOML source string.
    pub fn from_toml_str(src: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(src)
    }

    /// Look up capabilities by loaded-model filename.
    ///
    /// Accepts either the bare stem (`Qwen3.5-27B.Q8_0`, as
    /// returned by `InferenceProvider::model_id_for`) or the full
    /// filename with `.gguf`. Two-tier matching:
    ///
    ///   1. **`base_name` match** (preferred) — any slot whose
    ///      `base_name` is a case-insensitive substring of the
    ///      filename is a candidate. Longest matching `base_name`
    ///      wins (so `"Qwen3.5-35B-A3B"` outranks `"Qwen3.5-35B"`
    ///      when both would match). This is what makes a single
    ///      row in `models.toml` cover every quantisation.
    ///
    ///   2. **Exact filename match** (fallback) — used when no
    ///      `base_name` is declared. Back-compat with entries
    ///      written before the `base_name` field existed.
    ///
    /// `user_slots` are always checked before profile slots so a
    /// user's BYOM declarations outrank the bundled defaults.
    /// Empty `capabilities` is treated as "no annotation" rather
    /// than "empty profile" — callers get `None` and fall back.
    pub fn capabilities_for_file(&self, filename: &str) -> Option<CapabilityProfile> {
        let target = strip_gguf(filename).to_ascii_lowercase();

        // Collect every annotated slot in priority order
        // (user_slots first, then profile slots). Each becomes a
        // candidate if its base_name matches or its filename
        // matches exactly.
        let mut candidates: Vec<(&str, &CapabilityProfile)> = Vec::new();
        for user in &self.user_slots {
            if user.capabilities.is_empty() {
                continue;
            }
            candidates.push((&user.file, &user.capabilities));
            if !user.base_name.is_empty() {
                candidates.insert(
                    0,
                    (user.base_name.as_str(), &user.capabilities),
                );
            }
        }
        for profile in self.profiles.values() {
            for slot in [
                profile.fast.as_ref(),
                profile.thoughtful.as_ref(),
                profile.embed.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                if slot.capabilities.is_empty() {
                    continue;
                }
                candidates.push((&slot.file, &slot.capabilities));
                if !slot.base_name.is_empty() {
                    candidates.push((slot.base_name.as_str(), &slot.capabilities));
                }
            }
        }

        // Pass 1: base_name / partial substring match. Longest
        // matcher wins — "Qwen3.5-35B-A3B" beats "Qwen3.5-35B" on
        // a filename that contains both, so the MoE base_name
        // doesn't get swallowed by a shorter prefix match.
        let mut best: Option<(usize, &CapabilityProfile)> = None;
        for (pattern, caps) in &candidates {
            let pattern_lower = pattern.to_ascii_lowercase();
            let pattern_stripped = strip_gguf(&pattern_lower);
            // Exact match (after .gguf stripping) is the strongest
            // signal — wins over any substring-only match.
            if pattern_stripped == target {
                return Some((*caps).clone());
            }
            // Substring match — any pattern that appears anywhere
            // in the target filename.
            if target.contains(pattern_stripped) {
                let len = pattern_stripped.len();
                match best {
                    Some((best_len, _)) if len <= best_len => {}
                    _ => best = Some((len, *caps)),
                }
            }
        }
        best.map(|(_, caps)| caps.clone())
    }
}

fn strip_gguf(s: &str) -> &str {
    s.strip_suffix(".gguf").unwrap_or(s)
}

/// Process-wide view of the bundled `lcol-llm/models.toml`.
/// Parsed once on first access via `LazyLock`. Panics at startup
/// time if the bundled file has become malformed — that's a
/// compile-adjacent invariant, not a runtime condition.
pub static DEFAULT_MANIFEST: LazyLock<ModelsManifest> = LazyLock::new(|| {
    static SRC: &str = include_str!("../../../models.toml");
    ModelsManifest::from_toml_str(SRC).expect(
        "bundled lcol-llm/models.toml must parse — regression in the model manifest schema",
    )
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oicp::Capability;

    #[test]
    fn bundled_manifest_parses() {
        let m = &*DEFAULT_MANIFEST;
        // Sanity: the five hardware profiles plus whatever user
        // slots are on the machine. profiles should have at least
        // cpu_only + default + high + very_high + low_mem.
        assert!(
            m.profiles.len() >= 4,
            "expected at least 4 hardware profiles, got {}: {:?}",
            m.profiles.len(),
            m.profiles.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn capabilities_lookup_matches_with_or_without_extension() {
        let src = r#"
[profiles.default]
[profiles.default.thoughtful]
file = "Qwen_Qwen3.5-9B-Q4_K_M.gguf"
family = "Qwen35"
[profiles.default.thoughtful.capabilities]
general = 3
analysis = 3
"#;
        let m = ModelsManifest::from_toml_str(src).unwrap();
        // With .gguf
        let caps1 = m
            .capabilities_for_file("Qwen_Qwen3.5-9B-Q4_K_M.gguf")
            .expect("should match with extension");
        // Without .gguf (file stem)
        let caps2 = m
            .capabilities_for_file("Qwen_Qwen3.5-9B-Q4_K_M")
            .expect("should match stem");
        assert_eq!(caps1, caps2);
        assert_eq!(caps1.get(&Capability::General).copied(), Some(3));
        assert_eq!(caps1.get(&Capability::Analysis).copied(), Some(3));
    }

    #[test]
    fn base_name_matches_across_quantisations() {
        // One manifest row should cover Q4, Q8, and whatever a
        // user dumps locally as `*Qwen3.5-27B*.gguf`.
        let src = r#"
[profiles.high.thoughtful]
file = "Qwen_Qwen3.5-27B-Q4_K_M.gguf"
base_name = "Qwen3.5-27B"
family = "Qwen35"
[profiles.high.thoughtful.capabilities]
analysis = 4
math = 3
"#;
        let m = ModelsManifest::from_toml_str(src).unwrap();
        // The actual filename the Founder has:
        let q8 = m
            .capabilities_for_file("Qwen3.5-27B.Q8_0.1.gguf")
            .expect("Q8 variant should match base_name");
        // And a hypothetical Q5 dump:
        let q5 = m
            .capabilities_for_file("Qwen3.5-27B-Q5_K_S.gguf")
            .expect("Q5 variant should match base_name");
        // And the original manifest-filename:
        let q4 = m
            .capabilities_for_file("Qwen_Qwen3.5-27B-Q4_K_M.gguf")
            .expect("Q4 variant should match by filename or base_name");
        assert_eq!(q8.get(&Capability::Analysis).copied(), Some(4));
        assert_eq!(q5.get(&Capability::Analysis).copied(), Some(4));
        assert_eq!(q4.get(&Capability::Analysis).copied(), Some(4));
    }

    #[test]
    fn longest_base_name_wins() {
        // Declare two slots whose base_names both appear in the
        // query filename ("Qwen3.5-35B" and "Qwen3.5-35B-A3B").
        // Scoring should pick the more specific MoE variant.
        let src = r#"
[profiles.dense.thoughtful]
file = "qwen35-35b-dense.gguf"
base_name = "Qwen3.5-35B"
[profiles.dense.thoughtful.capabilities]
analysis = 3

[profiles.moe.thoughtful]
file = "qwen35-35b-a3b.gguf"
base_name = "Qwen3.5-35B-A3B"
[profiles.moe.thoughtful.capabilities]
analysis = 4
"#;
        let m = ModelsManifest::from_toml_str(src).unwrap();
        let caps = m
            .capabilities_for_file("Qwen3.5-35B-A3B.Q8_0.gguf")
            .expect("MoE-identifying filename should match MoE slot");
        assert_eq!(
            caps.get(&Capability::Analysis).copied(),
            Some(4),
            "longer base_name 'Qwen3.5-35B-A3B' should outrank 'Qwen3.5-35B'"
        );
    }

    #[test]
    fn case_insensitive_base_name() {
        let src = r#"
[profiles.default.thoughtful]
file = "x.gguf"
base_name = "Qwen3.5-9B"
[profiles.default.thoughtful.capabilities]
general = 3
"#;
        let m = ModelsManifest::from_toml_str(src).unwrap();
        let caps = m
            .capabilities_for_file("qwen3.5-9b.q8_0.gguf")
            .expect("lowercase filename should still match mixed-case base_name");
        assert_eq!(caps.get(&Capability::General).copied(), Some(3));
    }

    #[test]
    fn missing_file_returns_none() {
        let src = r#"
[profiles.default]
[profiles.default.thoughtful]
file = "some-model.gguf"
"#;
        let m = ModelsManifest::from_toml_str(src).unwrap();
        // Entry with no [capabilities] is treated as "don't have
        // an opinion" rather than "empty opinion" — routing falls
        // back to defaults, which is the desired BYOM UX.
        assert!(m.capabilities_for_file("some-model.gguf").is_none());
        assert!(m.capabilities_for_file("nothing-like-that").is_none());
    }

    #[test]
    fn user_slot_overrides_profile() {
        let src = r#"
[profiles.default.thoughtful]
file = "model.gguf"
[profiles.default.thoughtful.capabilities]
general = 2

[[user_slots]]
slot = "thoughtful"
file = "model.gguf"
[user_slots.capabilities]
general = 4
"#;
        let m = ModelsManifest::from_toml_str(src).unwrap();
        let caps = m.capabilities_for_file("model.gguf").unwrap();
        assert_eq!(
            caps.get(&Capability::General).copied(),
            Some(4),
            "user_slot should outrank profile slot on same filename"
        );
    }
}
