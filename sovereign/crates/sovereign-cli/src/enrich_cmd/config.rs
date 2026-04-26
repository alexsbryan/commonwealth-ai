//! `~/.sovereign/enrichment/<corpus>/config.json` — the pinned-at-init
//! configuration every other enrich subcommand reads.
//!
//! Everything that would otherwise be a CLI flag on every subcommand
//! (source file path, chat model id, regex pattern, etc.) gets pinned
//! here at `enrich init` time so iterations remain reproducible. The
//! developer can hand-edit this file if they need to swap a model or
//! widen the detector regex.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use corpus_engine::error::{Error, Result};

use super::paths;

pub const CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichConfig {
    pub schema_version: u32,
    pub corpus_id: String,
    pub pipeline_id: String,
    pub source_path: PathBuf,
    pub chapter_regex: String,
    pub chat_model: String,
    /// Optional per-phase model overrides. Keys are pipeline phase
    /// ids (`"phase1_seed"`, `"phase1"`, `"phase1_terse"`,
    /// `"phase3"`, `"phase3_facet"`, `"phase5"`, `"phase6"`,
    /// `"phase7"`, `"phase8_configuration"`); values are the model
    /// id strings the daemon will route the request to. When a phase
    /// id is missing from this map (or this whole map is `None`),
    /// the request falls back to `chat_model`.
    ///
    /// Hand-edit this in `~/.sovereign/enrichment/<corpus>/config.json`
    /// to recruit a small/fast model for bulk extraction phases and
    /// reserve the heavy reasoning model for synthesis phases — see
    /// `project_qwopus_size_ab.md` for the bench that motivates this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_models: Option<BTreeMap<String, String>>,
    pub embed_model: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// Minimum whitespace-separated token count a section's body must
    /// have for the detector to keep it. A section regex can match
    /// once in a list-of-headings index and again at the real body;
    /// this threshold is the structural guard. The right value
    /// depends on what the corpus is — prose books comfortably use
    /// ~40, a poetry anthology might want ~10, a code-module index
    /// might want ~200. `0` disables the filter entirely. Operators
    /// tune per corpus in `config.json`.
    #[serde(default = "default_min_section_body_words")]
    pub min_section_body_words: usize,
    /// Optional author-declared Table-of-Contents markers. When
    /// present, section detection reads the titles between these
    /// markers and anchors each section to the matching heading in
    /// the body — superseding `chapter_regex`. When absent, the
    /// regex detector runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toc_markers: Option<TocMarkers>,
    /// Cap on tokens the chat model may emit per request. Thinking
    /// models (Qwen3, DeepSeek R1) regularly spend 2-3k tokens on
    /// chain-of-thought before their JSON answer — a too-small cap
    /// truncates mid-think and the parser sees no JSON. Raise this
    /// for verbose models; lower it to force succinct outputs.
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u32,
    pub created_at: String,
}

/// Paired start/end delimiters for an author-declared Table of
/// Contents. See `TocAnchoredDetector`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TocMarkers {
    pub start: String,
    pub end: String,
}

fn default_base_url() -> String {
    format!(
        "http://localhost:{}",
        crate::util::urls::DEFAULT_CLIENT_PORT
    )
}

/// Default section-body floor. Chosen to comfortably clear
/// list-of-headings index entries (typically 5–10 words) without
/// ruling out genuinely short sections; operators who need a
/// different floor override per-corpus in config.json.
fn default_min_section_body_words() -> usize {
    40
}

/// Default per-request output cap. 16384 covers a full thinking
/// trace plus a phase-1 JSON answer on long sections (SEP article
/// introductions, brothers_karamazov chapter heads). 4096 was the
/// historical default but truncated mid-JSON on long inputs under
/// Q5_K_S quantization, leaving Phase 1 with parse_drift failures
/// the auto-retry could not recover. Operators on tight contexts
/// tune lower per-corpus in config.json.
fn default_max_output_tokens() -> u32 {
    16384
}

impl EnrichConfig {
    pub fn load(corpus_id: &str) -> Result<Option<Self>> {
        let path = paths::config_path(corpus_id);
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&path)?;
        let cfg: Self = serde_json::from_str(&raw).map_err(|e| {
            Error::Serialization(format!(
                "enrich config {} is malformed: {}",
                path.display(),
                e
            ))
        })?;
        if cfg.schema_version > CONFIG_SCHEMA_VERSION {
            return Err(Error::Serialization(format!(
                "enrich config {} has schema_version {} but this binary supports {}",
                path.display(),
                cfg.schema_version,
                CONFIG_SCHEMA_VERSION
            )));
        }
        Ok(Some(cfg))
    }

    pub fn require(corpus_id: &str) -> Result<Self> {
        Self::load(corpus_id)?.ok_or_else(|| {
            Error::InvalidInput(format!(
                "no enrichment config for corpus '{corpus_id}' — run `sovereign enrich init {corpus_id} --source <path>` first"
            ))
        })
    }

    /// Atomic save via tmp + rename.
    pub fn save(&self) -> Result<()> {
        let path = paths::config_path(&self.corpus_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| Error::Serialization(e.to_string()))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn path(&self) -> PathBuf {
        paths::config_path(&self.corpus_id)
    }

    pub fn source_path_abs(&self) -> &Path {
        &self.source_path
    }

    /// Resolve the chat-model id to use for a given pipeline phase.
    /// Returns the per-phase override from `chat_models` when present,
    /// otherwise falls back to `chat_model`. Operators wire per-phase
    /// model recruitment by editing `chat_models` in `config.json`.
    pub fn resolve_chat_model(&self, phase_id: &str) -> &str {
        self.chat_models
            .as_ref()
            .and_then(|map| map.get(phase_id))
            .map(String::as_str)
            .unwrap_or(self.chat_model.as_str())
    }

    /// Snapshot of the operator's per-phase model overrides for
    /// downstream wiring. Returns an empty map when no overrides are
    /// configured. The snapshot is owned so callers can pass it to
    /// `DaemonInferenceClient::with_chat_models_by_phase` without
    /// holding a borrow on the config.
    pub fn chat_models_by_phase_snapshot(&self) -> BTreeMap<String, String> {
        self.chat_models.clone().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> EnrichConfig {
        EnrichConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            corpus_id: "test".into(),
            pipeline_id: "literary".into(),
            source_path: PathBuf::from("/tmp/foo.txt"),
            chapter_regex: r"(?m)^Chapter".into(),
            chat_model: "chat-m".into(),
            chat_models: None,
            embed_model: "embed-m".into(),
            base_url: "http://localhost:9741".into(),
            min_section_body_words: default_min_section_body_words(),
            toc_markers: None,
            max_output_tokens: default_max_output_tokens(),
            created_at: "2026-04-22T00:00:00Z".into(),
        }
    }

    #[test]
    fn serde_roundtrip_preserves_fields() {
        let cfg = sample_config();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: EnrichConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.corpus_id, cfg.corpus_id);
        assert_eq!(back.chat_model, cfg.chat_model);
        assert_eq!(back.source_path, cfg.source_path);
    }

    #[test]
    fn default_base_url_used_when_absent() {
        let json = r#"{
            "schema_version": 1,
            "corpus_id": "x",
            "pipeline_id": "literary",
            "source_path": "/tmp/x.txt",
            "chapter_regex": "^Chapter",
            "chat_model": "c",
            "embed_model": "e",
            "created_at": "t"
        }"#;
        let cfg: EnrichConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.base_url.contains("localhost"));
    }

    #[test]
    fn resolve_chat_model_falls_back_to_default_when_no_override() {
        let cfg = sample_config();
        // No chat_models map set → every phase returns the default.
        assert_eq!(cfg.resolve_chat_model("phase1"), "chat-m");
        assert_eq!(cfg.resolve_chat_model("phase8_configuration"), "chat-m");
        assert_eq!(cfg.resolve_chat_model("anything-unknown"), "chat-m");
    }

    #[test]
    fn resolve_chat_model_uses_per_phase_override() {
        let mut overrides = BTreeMap::new();
        overrides.insert("phase1".into(), "qwen-9b".into());
        overrides.insert("phase8_configuration".into(), "qwopus-27b".into());
        let cfg = EnrichConfig {
            chat_models: Some(overrides),
            ..sample_config()
        };
        assert_eq!(cfg.resolve_chat_model("phase1"), "qwen-9b");
        assert_eq!(cfg.resolve_chat_model("phase8_configuration"), "qwopus-27b");
        // Phase id missing from the map still falls back.
        assert_eq!(cfg.resolve_chat_model("phase5"), "chat-m");
    }

    #[test]
    fn chat_models_by_phase_snapshot_is_empty_when_unset() {
        let cfg = sample_config();
        assert!(cfg.chat_models_by_phase_snapshot().is_empty());
    }

    #[test]
    fn chat_models_by_phase_snapshot_carries_overrides() {
        let mut overrides = BTreeMap::new();
        overrides.insert("phase7".into(), "big-model".into());
        let cfg = EnrichConfig {
            chat_models: Some(overrides.clone()),
            ..sample_config()
        };
        assert_eq!(cfg.chat_models_by_phase_snapshot(), overrides);
    }

    #[test]
    fn old_config_without_chat_models_loads_with_default_none() {
        // Operators upgrading the binary keep their existing
        // config.json files. The `#[serde(default)]` on chat_models
        // means deserialisation must succeed without the field.
        let json = r#"{
            "schema_version": 1,
            "corpus_id": "x",
            "pipeline_id": "literary",
            "source_path": "/tmp/x.txt",
            "chapter_regex": "^Chapter",
            "chat_model": "c",
            "embed_model": "e",
            "created_at": "t"
        }"#;
        let cfg: EnrichConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.chat_models.is_none());
        assert_eq!(cfg.resolve_chat_model("phase1"), "c");
    }

    #[test]
    fn old_config_without_min_section_body_words_gets_default() {
        // Config files written before the field existed must
        // continue to load — the default is picked up via
        // `#[serde(default)]` so operators aren't forced to
        // re-init every corpus.
        let json = r#"{
            "schema_version": 1,
            "corpus_id": "x",
            "pipeline_id": "literary",
            "source_path": "/tmp/x.txt",
            "chapter_regex": "^Chapter",
            "chat_model": "c",
            "embed_model": "e",
            "created_at": "t"
        }"#;
        let cfg: EnrichConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.min_section_body_words, default_min_section_body_words());
    }
}
