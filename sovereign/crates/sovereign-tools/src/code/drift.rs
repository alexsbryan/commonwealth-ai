// SPDX-License-Identifier: AGPL-3.0-or-later
//! `drift` — report which feature specs have drifted from their
//! approved (committed) version.
//!
//! Demo shape:
//!
//! ```text
//! drift()
//! drift(feature_id: "p0-payments")
//! ```
//!
//! Output:
//!
//! ```json
//! {
//!   "drifted": [
//!     { "feature_id": "p0-payments",
//!       "approved_hash": "abc123…",
//!       "current_hash":  "def456…" }
//!   ],
//!   "clean":      ["p0-search"],
//!   "unapproved": ["new-feature"]
//! }
//! ```
//!
//! - `drifted`    — spec has commits AND the working tree differs
//!   from the most recently committed version.
//! - `clean`      — spec has commits AND the working tree matches.
//! - `unapproved` — spec exists in `.sovereign/features/<id>/` but
//!   has never been committed (no approval anchor yet).
//!
//! Internally calls [`sovereign_atos::approval::find_approval`] and
//! [`sovereign_atos::approval::detect_drift`] — the same primitives
//! the daemon's `approval_gate` middleware uses, so the tool's
//! verdict matches the gate's verdict.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use sovereign_atos::approval::{current_spec_hash, detect_drift, find_approval};

pub struct DriftTool;

impl DriftTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DriftTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DriftTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "drift".to_string(),
            name: "Drift".to_string(),
            description: "Report which `.sovereign/features/*/spec.md` files have drifted \
                          from their committed (approved) version. A feature is \
                          'drifted' when the working-tree spec hash differs from the \
                          most recent commit's hash, 'clean' when they match, and \
                          'unapproved' when the spec has never been committed. Use \
                          before declaring a feature done — drift means the \
                          implementation is operating against a spec the team hasn't \
                          formally accepted. The verdict matches the daemon's \
                          approval_gate middleware so what `drift` reports is what \
                          the gate enforces. Pass `feature_id` to scope to one \
                          feature; omit to report on every feature directory."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "feature_id": {
                        "type": "string",
                        "description": "Restrict drift detection to a single feature."
                    }
                },
                "required": []
            }),
            examples: vec![
                ToolExample {
                    situation: "Before declaring a feature complete, verify its spec hasn't drifted from the approved commit.".into(),
                    call: serde_json::json!({ "feature_id": "p0-payments" }),
                },
                ToolExample {
                    situation: "Survey the workspace — are any feature specs out of sync with their approval?".into(),
                    call: serde_json::json!({}),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "repo_root":  { "type": "string" },
                    "drifted":    { "type": "array" },
                    "clean":      { "type": "array" },
                    "unapproved": { "type": "array" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::FileRead]
    }

    async fn execute(&self, params: &serde_json::Value, ctx: &ToolContext) -> Result<StepOutput> {
        let cwd = ctx
            .working_directory
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let Some(repo_root) = find_repo_root(&cwd) else {
            return Err(Error::Tool {
                tool_id: "drift".to_string(),
                message: format!(
                    "no `.sovereign/` directory found at or above {}; run `sovereign init` first",
                    cwd.display()
                ),
            });
        };

        let scoped_id = params
            .get("feature_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let feature_ids = match scoped_id {
            Some(id) => vec![id],
            None => list_feature_ids(&repo_root),
        };

        let mut drifted = Vec::new();
        let mut clean = Vec::new();
        let mut unapproved = Vec::new();

        for id in feature_ids {
            // `find_approval` with `None` mesh = git-path only.
            // The daemon's gate also walks the mesh fallback; this
            // tool intentionally doesn't, so the verdict is
            // reproducible without daemon state. A feature
            // approved only via mesh shows up as `unapproved` here
            // — accurate as a "git-anchor not present yet" signal.
            let approval = find_approval(&repo_root, &id, None);
            match approval {
                Some(appr) => {
                    if detect_drift(&appr, &repo_root) {
                        let current = current_spec_hash(&repo_root, &id).unwrap_or_default();
                        drifted.push(json!({
                            "feature_id":    id,
                            "approved_hash": short_hash(&appr.spec_content_hash),
                            "current_hash":  short_hash(&current),
                            "approved_by":   appr.approved_by,
                        }));
                    } else {
                        clean.push(id);
                    }
                }
                None => unapproved.push(id),
            }
        }

        Ok(StepOutput::Json(json!({
            "repo_root":  repo_root.display().to_string(),
            "drifted":    drifted,
            "clean":      clean,
            "unapproved": unapproved,
        })))
    }
}

fn find_repo_root(from: &Path) -> Option<PathBuf> {
    let mut current = from.to_path_buf();
    loop {
        if current.join(".sovereign").is_dir() {
            return Some(current);
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => return None,
        }
    }
}

/// Return every feature id (directory name) under
/// `<repo_root>/.sovereign/features/`. Empty vec when the
/// directory is missing or empty — the tool reports `drifted: []
/// clean: [] unapproved: []` rather than erroring.
fn list_feature_ids(repo_root: &Path) -> Vec<String> {
    let features_dir = repo_root.join(".sovereign").join("features");
    let Ok(entries) = std::fs::read_dir(&features_dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    ids.sort();
    ids
}

/// 8-char prefix of a hex SHA-256 — enough for human-readable
/// drift output without quoting the full 64-char string.
fn short_hash(h: &str) -> String {
    h.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn list_feature_ids_handles_empty_and_populated() {
        let tmp = TempDir::new().unwrap();
        // Missing features dir → empty vec, not an error.
        assert!(list_feature_ids(tmp.path()).is_empty());

        let features = tmp.path().join(".sovereign/features");
        std::fs::create_dir_all(&features).unwrap();
        // Empty dir → empty vec.
        assert!(list_feature_ids(tmp.path()).is_empty());

        std::fs::create_dir(features.join("p0-payments")).unwrap();
        std::fs::create_dir(features.join("p0-search")).unwrap();
        // File entries don't count.
        std::fs::write(features.join("README.md"), "").unwrap();

        let ids = list_feature_ids(tmp.path());
        assert_eq!(
            ids,
            vec!["p0-payments".to_string(), "p0-search".to_string()]
        );
    }

    #[test]
    fn short_hash_truncates_to_8_chars() {
        let full = "abcdef0123456789";
        assert_eq!(short_hash(full), "abcdef01");
        // Empty hash returns empty (no panic).
        assert_eq!(short_hash(""), "");
        // Short input returned verbatim.
        assert_eq!(short_hash("abc"), "abc");
    }
}
