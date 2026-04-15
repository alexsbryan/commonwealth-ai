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
    /// OICP capability declarations for this specific model +
    /// quantisation. Empty when absent — the mesh routing path
    /// falls back to conservative defaults for BYOM or legacy
    /// entries that haven't been annotated yet.
    #[serde(default)]
    pub capabilities: CapabilityProfile,
}

/// A `[[user_slots]]` entry — "bring your own model" override
/// that replaces one slot of the active profile.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserSlotConfig {
    pub slot: String,
    pub file: String,
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
    /// Accepts either the bare stem (`Qwen_Qwen3.5-9B-Q4_K_M`, as
    /// returned by `InferenceProvider::model_id_for`) or the full
    /// filename with `.gguf`. Matching is case-sensitive because
    /// HuggingFace / bartowski filenames are — no normalisation.
    ///
    /// Returns `None` when the file isn't in the manifest, which
    /// callers should treat as "conservative defaults" rather
    /// than "no capabilities." A user pointing at a downloaded
    /// BYOM file is the common case here.
    pub fn capabilities_for_file(&self, filename: &str) -> Option<CapabilityProfile> {
        let target = strip_gguf(filename);
        // user_slots win over profile slots of the same filename:
        // a user who opts in to BYOM expects their declarations
        // to take precedence over the bundled manifest.
        for user in &self.user_slots {
            if strip_gguf(&user.file) == target && !user.capabilities.is_empty() {
                return Some(user.capabilities.clone());
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
                if strip_gguf(&slot.file) == target && !slot.capabilities.is_empty() {
                    return Some(slot.capabilities.clone());
                }
            }
        }
        None
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
