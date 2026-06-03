//! Parser for `sovereign/models.toml`.
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
        self.info_for_file(filename).map(|i| i.capabilities)
    }

    /// Extended lookup returning both the declared capability
    /// profile AND the slot's `size_gb`. Same matching rules as
    /// `capabilities_for_file`; `size_gb == 0.0` (the serde
    /// default) is normalised to `None` so callers can treat
    /// "unknown" and "explicitly zero" identically.
    ///
    /// Exists because the mesh routing path uses `size_gb` as a
    /// tiebreaker when two models score equally — picking the
    /// smaller one is the closest we can get to "pick the faster
    /// one" without a live latency measurement. Returning
    /// capabilities + size in one lookup keeps the hot path from
    /// scanning the manifest twice.
    pub fn info_for_file(&self, filename: &str) -> Option<SlotInfo> {
        let target = strip_gguf(filename).to_ascii_lowercase();

        // Collect every annotated slot in priority order. Each
        // annotated slot produces one or two patterns (filename +
        // optional base_name). We now carry size_gb alongside so
        // the same matching walk surfaces both fields.
        struct Candidate<'a> {
            pattern: &'a str,
            caps: &'a CapabilityProfile,
            size_gb: f64,
        }
        let mut candidates: Vec<Candidate<'_>> = Vec::new();
        for user in &self.user_slots {
            if user.capabilities.is_empty() {
                continue;
            }
            // Base_name candidates come first in priority when
            // present — matches the existing insert(0, ...) policy.
            if !user.base_name.is_empty() {
                candidates.push(Candidate {
                    pattern: user.base_name.as_str(),
                    caps: &user.capabilities,
                    size_gb: user.size_gb,
                });
            }
            candidates.push(Candidate {
                pattern: user.file.as_str(),
                caps: &user.capabilities,
                size_gb: user.size_gb,
            });
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
                candidates.push(Candidate {
                    pattern: slot.file.as_str(),
                    caps: &slot.capabilities,
                    size_gb: slot.size_gb,
                });
                if !slot.base_name.is_empty() {
                    candidates.push(Candidate {
                        pattern: slot.base_name.as_str(),
                        caps: &slot.capabilities,
                        size_gb: slot.size_gb,
                    });
                }
            }
        }

        // Matching: exact wins instantly; otherwise longest
        // substring wins. Mirrors the original algorithm but now
        // also carries size_gb through.
        let mut best: Option<(usize, &CapabilityProfile, f64)> = None;
        for c in &candidates {
            let pattern_lower = c.pattern.to_ascii_lowercase();
            let pattern_stripped = strip_gguf(&pattern_lower);
            if pattern_stripped == target {
                return Some(SlotInfo {
                    capabilities: c.caps.clone(),
                    size_gb: normalise_size(c.size_gb),
                });
            }
            if target.contains(pattern_stripped) {
                let len = pattern_stripped.len();
                match best {
                    Some((best_len, _, _)) if len <= best_len => {}
                    _ => best = Some((len, c.caps, c.size_gb)),
                }
            }
        }
        best.map(|(_, caps, size_gb)| SlotInfo {
            capabilities: caps.clone(),
            size_gb: normalise_size(size_gb),
        })
    }

    /// Resolve a loaded embed GGUF to its declared
    /// [`ModelFamily`] so the caller can drive
    /// [`ModelFamily::default_quirks`]`().embed` — the only way to
    /// pick the right pooling + normalisation without actually
    /// running the model.
    ///
    /// **Why this exists as a dedicated method, not a field on
    /// [`SlotInfo`]:** the embed family drives a cross-peer
    /// interoperability contract (two nodes with different
    /// [`PoolingStrategy`] or [`NormalizationStrategy`] produce
    /// incompatible vectors — collaborative ingestion must reject
    /// them). Getting it wrong silently excludes the node from
    /// corpus routing with no user-visible error. Giving the
    /// lookup its own method keeps the call site obvious in code
    /// review and forces BYOM paths to acknowledge the
    /// `None` → `ModelFamily::Unknown` fallback.
    ///
    /// Matching uses the same two-tier walk as
    /// [`info_for_file`](Self::info_for_file) (exact file, then
    /// longest `base_name` substring). Unlike `info_for_file` this
    /// considers slots even when their `capabilities` are empty —
    /// an embed slot's family is independent of whether the
    /// manifest author filled in a `[profiles.X.embed.capabilities]`
    /// block.
    pub fn embed_family_for_file(
        &self,
        filename: &str,
    ) -> Option<crate::model_family::ModelFamily> {
        let target = strip_gguf(filename).to_ascii_lowercase();

        // Gather candidates. Each carries the family string from
        // its TOML row; we resolve the string to `ModelFamily` at
        // the end rather than per-candidate because the walk needs
        // to compare patterns, not family identities.
        struct Candidate<'a> {
            pattern: &'a str,
            family: &'a str,
        }
        let mut candidates: Vec<Candidate<'_>> = Vec::new();
        for user in &self.user_slots {
            if user.slot != "embed" {
                continue;
            }
            if !user.base_name.is_empty() {
                candidates.push(Candidate {
                    pattern: user.base_name.as_str(),
                    family: user.family.as_str(),
                });
            }
            candidates.push(Candidate {
                pattern: user.file.as_str(),
                family: user.family.as_str(),
            });
        }
        for profile in self.profiles.values() {
            let Some(slot) = profile.embed.as_ref() else {
                continue;
            };
            if !slot.base_name.is_empty() {
                candidates.push(Candidate {
                    pattern: slot.base_name.as_str(),
                    family: slot.family.as_str(),
                });
            }
            candidates.push(Candidate {
                pattern: slot.file.as_str(),
                family: slot.family.as_str(),
            });
        }

        // Exact match → return immediately. Otherwise longest
        // substring match wins.
        let mut best: Option<(usize, &str)> = None;
        for c in &candidates {
            let pattern_lower = c.pattern.to_ascii_lowercase();
            let pattern_stripped = strip_gguf(&pattern_lower);
            if pattern_stripped.is_empty() {
                continue;
            }
            if pattern_stripped == target {
                return Some(parse_family(c.family));
            }
            if target.contains(pattern_stripped) {
                let len = pattern_stripped.len();
                match best {
                    Some((best_len, _)) if len <= best_len => {}
                    _ => best = Some((len, c.family)),
                }
            }
        }
        best.map(|(_, family)| parse_family(family))
    }
}

/// Parse the TOML `family = "..."` string into a [`ModelFamily`]
/// enum value. Returns `ModelFamily::Unknown` for any unrecognised
/// string (including empty / missing), matching the default
/// behaviour of [`ModelFamily::default`].
fn parse_family(s: &str) -> crate::model_family::ModelFamily {
    use crate::model_family::ModelFamily;
    match s {
        "Qwen3" => ModelFamily::Qwen3,
        "Qwen35" => ModelFamily::Qwen35,
        "Qwen3Embedding" => ModelFamily::Qwen3Embedding,
        "Gemma3" => ModelFamily::Gemma3,
        "Gemma4" => ModelFamily::Gemma4,
        "Llama3" => ModelFamily::Llama3,
        "Phi4" => ModelFamily::Phi4,
        "Phi4Reasoning" => ModelFamily::Phi4Reasoning,
        "SmolLM3" => ModelFamily::SmolLM3,
        _ => ModelFamily::Unknown,
    }
}

/// Result of a `models.toml` lookup — capability profile plus the
/// declared on-disk size. `size_gb == None` means the manifest
/// author didn't declare one (or declared zero, which we treat
/// identically). The OICP tiebreaker treats `None` as "unknown,
/// sort after any known size" so an unannotated model doesn't
/// spuriously beat an annotated one in a score tie.
#[derive(Debug, Clone)]
pub struct SlotInfo {
    pub capabilities: CapabilityProfile,
    pub size_gb: Option<f32>,
}

fn normalise_size(v: f64) -> Option<f32> {
    if v <= 0.0 || !v.is_finite() {
        None
    } else {
        Some(v as f32)
    }
}

fn strip_gguf(s: &str) -> &str {
    s.strip_suffix(".gguf").unwrap_or(s)
}

/// Process-wide view of the bundled `sovereign/models.toml`.
/// Parsed once on first access via `LazyLock`. Panics at startup
/// time if the bundled file has become malformed — that's a
/// compile-adjacent invariant, not a runtime condition.
pub static DEFAULT_MANIFEST: LazyLock<ModelsManifest> = LazyLock::new(|| {
    static SRC: &str = include_str!("../../../models.toml");
    ModelsManifest::from_toml_str(SRC).expect(
        "bundled sovereign/models.toml must parse — regression in the model manifest schema",
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
    fn info_for_file_returns_size_gb_alongside_caps() {
        // The mesh-routing tiebreaker depends on `size_gb` being
        // surfaced from the same lookup that returns capabilities.
        // Guard the contract with an explicit test — silent
        // regressions here turn size-based tie-breaking into
        // "whichever model the HashMap iterates first", which
        // reliably defeats the point of having a tiebreaker.
        let src = r#"
[profiles.default.thoughtful]
file = "nine-b.gguf"
base_name = "Qwen3.5-9B"
size_gb = 5.5
[profiles.default.thoughtful.capabilities]
general = 3
analysis = 3

[profiles.high.thoughtful]
file = "twenty-seven-b.gguf"
base_name = "Qwen3.5-27B"
size_gb = 16.5
[profiles.high.thoughtful.capabilities]
general = 3
analysis = 4
"#;
        let m = ModelsManifest::from_toml_str(src).unwrap();
        let nine = m.info_for_file("Qwen3.5-9B.Q4_K_M.gguf").unwrap();
        assert_eq!(nine.size_gb, Some(5.5));
        assert_eq!(
            nine.capabilities.get(&Capability::Analysis).copied(),
            Some(3)
        );
        let twenty_seven = m.info_for_file("Qwen3.5-27B.Q8_0.gguf").unwrap();
        assert_eq!(twenty_seven.size_gb, Some(16.5));
        assert_eq!(
            twenty_seven
                .capabilities
                .get(&Capability::Analysis)
                .copied(),
            Some(4)
        );
    }

    #[test]
    fn embed_family_for_file_resolves_qwen3_embedding_from_profile() {
        use crate::model_family::{ModelFamily, PoolingStrategy};
        // Ground truth: the bundled manifest declares Qwen3-Embedding
        // with `family = "Qwen3Embedding"` in the default profile's
        // embed slot. The CLI daemon's collaborative-ingestion
        // advertisement depends on getting this right — `Mean` vs
        // `Last` pooling produces incompatible vectors across peers.
        let m = &DEFAULT_MANIFEST;
        let family = m
            .embed_family_for_file("Qwen3-Embedding-0.6B-Q8_0.gguf")
            .expect("bundled manifest must contain a Qwen3-Embedding entry");
        assert_eq!(family, ModelFamily::Qwen3Embedding);
        let embed_quirks = family
            .default_quirks()
            .embed
            .expect("Qwen3Embedding must declare EmbedQuirks");
        // Qwen3-Embedding is Last-pooled + application-normalised;
        // any drift here means the CLI daemon advertises vectors
        // that won't match desktop peers doing the same probe.
        // (This is the bug the new `embed_family_for_file` helper
        // exists to prevent: the old CLI hardcoded Mean + Application
        // and silently excluded itself from Qwen3-Embedding meshes.)
        assert_eq!(embed_quirks.pooling, PoolingStrategy::Last);
        assert_eq!(
            embed_quirks.normalize,
            crate::model_family::NormalizationStrategy::Application
        );
    }

    #[test]
    fn embed_family_for_file_returns_none_for_unknown_gguf() {
        let m = &DEFAULT_MANIFEST;
        assert!(m
            .embed_family_for_file("totally-unknown-embed-model.gguf")
            .is_none());
    }

    #[test]
    fn embed_family_parses_known_strings_and_falls_back_on_unknown() {
        use crate::model_family::ModelFamily;
        // `parse_family` is the normalisation layer between manifest
        // TOML strings and the enum. Drift between manifest
        // `family = "..."` values and enum variants would silently
        // map to `Unknown` — guard the canonical strings.
        let src = r#"
[profiles.default.embed]
file = "q3e.gguf"
family = "Qwen3Embedding"

[profiles.default.fast]
file = "fast.gguf"
family = "Qwen3"

[profiles.high.embed]
file = "misspelled.gguf"
family = "QwenThreeEmbedding"
"#;
        let m = ModelsManifest::from_toml_str(src).unwrap();
        assert_eq!(
            m.embed_family_for_file("q3e.gguf"),
            Some(ModelFamily::Qwen3Embedding)
        );
        // Misspelled / unrecognised strings degrade to Unknown
        // rather than erroring — the manifest is the source of
        // truth and we don't refuse to load on drift, but the
        // embed slot will advertise Mean+Application which may
        // or may not match reality.
        assert_eq!(
            m.embed_family_for_file("misspelled.gguf"),
            Some(ModelFamily::Unknown)
        );
    }

    #[test]
    fn info_for_file_treats_zero_size_as_unknown() {
        // BYOM entries routinely omit size_gb; serde fills in
        // 0.0. Returning Some(0.0) would be misleading — the
        // tiebreaker would think that model is "smaller than a
        // 0.4 GB router model". Must be normalised to None.
        let src = r#"
[profiles.byom_qwen25.thoughtful]
file = "qwen2.5-3b-instruct-q4_k_m.gguf"
base_name = "Qwen2.5-3B"
[profiles.byom_qwen25.thoughtful.capabilities]
general = 2
"#;
        let m = ModelsManifest::from_toml_str(src).unwrap();
        let info = m.info_for_file("qwen2.5-3b-instruct-q4_k_m.gguf").unwrap();
        assert_eq!(info.size_gb, None);
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
