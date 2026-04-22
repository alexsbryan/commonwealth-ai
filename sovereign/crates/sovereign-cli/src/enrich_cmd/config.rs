//! `~/.sovereign/enrichment/<corpus>/config.json` — the pinned-at-init
//! configuration every other enrich subcommand reads.
//!
//! Everything that would otherwise be a CLI flag on every subcommand
//! (source file path, chat model id, regex pattern, etc.) gets pinned
//! here at `enrich init` time so iterations remain reproducible. The
//! developer can hand-edit this file if they need to swap a model or
//! widen the detector regex.

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
    pub embed_model: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    pub created_at: String,
}

fn default_base_url() -> String {
    format!(
        "http://localhost:{}",
        crate::util::urls::DEFAULT_CLIENT_PORT
    )
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
            embed_model: "embed-m".into(),
            base_url: "http://localhost:9741".into(),
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
}
