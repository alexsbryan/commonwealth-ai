// SPDX-License-Identifier: AGPL-3.0-or-later
//! `spec` — return the active spec, architecture doc, and charter
//! in a single call.
//!
//! Demo shape:
//!
//! ```text
//! spec()                            // active feature inferred from ctx
//! spec(feature_id: "p0-payments")   // explicit feature
//! ```
//!
//! Returns:
//!
//! - `spec`         — `.sovereign/features/<id>/spec.md` body, when
//!   a feature id was supplied or could be inferred from a single
//!   `.sovereign/features/*/` directory.
//! - `architecture` — `ARCHITECTURE.md` from the repo root, when
//!   present.
//! - `charter`      — `.sovereign/CHARTER.md` body, when present.
//! - `feature_id`   — id we resolved (so the agent can confirm the
//!   target wasn't ambiguous).
//! - `repo_root`    — absolute path the spec/architecture/charter
//!   were resolved against.
//!
//! All three documents are markdown; none are trusted to fit in a
//! response — each is capped at [`MAX_DOC_BYTES`] with a
//! `*_truncated` flag.
//!
//! ## Why one tool?
//!
//! Until now an agent had to call `project_context` (search) +
//! read the relevant file. The flat-namespace surface advertises
//! `spec()` as a single canonical affordance for "show me the
//! contract I'm working under." The search-style functionality
//! that `project_context` offers stays available through the
//! alias period.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use corpus_engine_notes::ProjectDocsStore;
use sovereign_core::tool_manifest::DeclaredTool;

/// Hard cap on the bytes returned for any one document. The spec
/// is normally a few KB at most; ARCHITECTURE.md is the long
/// outlier. 32 KB lets the typical response stay under ~80 KB
/// total when all three files are present.
const MAX_DOC_BYTES: usize = 32 * 1024;

pub struct SpecTool {
    /// Optional doc store. Phase 5 will use this to surface
    /// related-doc excerpts when the agent asks a question (i.e.
    /// `spec(query: "...")`). Phase 2 keeps the store wired so
    /// the registry builder doesn't have to refactor again.
    #[allow(dead_code)]
    docs: Option<Arc<ProjectDocsStore>>,
}

impl SpecTool {
    pub fn new() -> Self {
        Self { docs: None }
    }

    pub fn with_docs(mut self, docs: Arc<ProjectDocsStore>) -> Self {
        self.docs = Some(docs);
        self
    }
}

impl Default for SpecTool {
    fn default() -> Self {
        Self::new()
    }
}

impl SpecTool {
    /// Bind this tool's state to its `spec` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("spec", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `spec`.
    async fn run(
        &self,
        params: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let cwd = ctx
            .working_directory
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let Some(repo_root) = find_repo_root(&cwd) else {
            return Err(Error::Tool {
                tool_id: "spec".to_string(),
                message: format!(
                    "no `.sovereign/` directory found at or above {}; run `sovereign init` first",
                    cwd.display()
                ),
            });
        };

        let requested_feature = params
            .get("feature_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let feature_id = match requested_feature {
            Some(id) => Some(id),
            None => single_feature_dir(&repo_root.join(".sovereign").join("features")),
        };

        let spec_path = feature_id.as_ref().map(|id| {
            repo_root
                .join(".sovereign")
                .join("features")
                .join(id)
                .join("spec.md")
        });
        let (spec, spec_truncated) = match &spec_path {
            Some(p) => read_capped(p),
            None => (None, false),
        };

        let architecture_path = repo_root.join("ARCHITECTURE.md");
        let (architecture, architecture_truncated) = read_capped(&architecture_path);

        let charter_path = repo_root.join(".sovereign").join("CHARTER.md");
        let (charter, charter_truncated) = read_capped(&charter_path);

        Ok(StepOutput::Json(json!({
            "feature_id":             feature_id,
            "repo_root":              repo_root.display().to_string(),
            "spec":                   spec,
            "spec_truncated":         spec_truncated,
            "architecture":           architecture,
            "architecture_truncated": architecture_truncated,
            "charter":                charter,
            "charter_truncated":      charter_truncated,
        })))
    }
}

/// Walk up from `from` looking for a directory that contains a
/// `.sovereign/` child. Returns that directory (the repo root) on
/// the first hit, or `None` if we walk to the filesystem root
/// without finding one.
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

/// Return the single feature directory's id under `features_dir`,
/// or `None` if the directory is missing, empty, or contains more
/// than one entry. Auto-resolution is intentionally conservative:
/// silently picking one of several features would mask "agent
/// asked about the wrong feature" bugs.
fn single_feature_dir(features_dir: &Path) -> Option<String> {
    let entries: Vec<_> = std::fs::read_dir(features_dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .collect();
    if entries.len() == 1 {
        entries[0].file_name().into_string().ok()
    } else {
        None
    }
}

/// Read `path` at most [`MAX_DOC_BYTES`]. Returns `(Some(body),
/// truncated)` on success, `(None, false)` when the file is
/// missing or unreadable. Truncation lands on a UTF-8 boundary so
/// the returned string is always valid.
fn read_capped(path: &Path) -> (Option<String>, bool) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return (None, false),
    };
    let truncated = bytes.len() > MAX_DOC_BYTES;
    let slice = if truncated {
        &bytes[..MAX_DOC_BYTES]
    } else {
        bytes.as_slice()
    };
    let mut text = String::from_utf8_lossy(slice).into_owned();
    if truncated {
        // String::from_utf8_lossy may have produced a trailing
        // U+FFFD replacement character if we cut a multi-byte
        // codepoint. Backwards-trim so the document ends on a
        // clean codepoint boundary before appending the marker.
        while text.ends_with('\u{FFFD}') {
            text.pop();
        }
        text.push_str("\n…[truncated]");
    }
    (Some(text), truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// `find_repo_root` walks up the path until it finds a
    /// `.sovereign/` directory. Direct hit and one-level-up case
    /// both succeed; missing case returns `None`.
    #[test]
    fn find_repo_root_walks_up() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".sovereign")).unwrap();
        let nested = tmp.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_repo_root(tmp.path()), Some(tmp.path().to_path_buf()));
        assert_eq!(find_repo_root(&nested), Some(tmp.path().to_path_buf()));

        let elsewhere = TempDir::new().unwrap();
        assert_eq!(find_repo_root(elsewhere.path()), None);
    }

    /// `single_feature_dir` resolves only when there's exactly one
    /// feature directory. Multiple → `None` (forces the caller to
    /// disambiguate); zero → `None`.
    #[test]
    fn single_feature_dir_is_conservative() {
        let tmp = TempDir::new().unwrap();
        let features = tmp.path().join("features");
        std::fs::create_dir_all(&features).unwrap();
        // Empty.
        assert_eq!(single_feature_dir(&features), None);
        // Exactly one.
        std::fs::create_dir(features.join("p0-payments")).unwrap();
        assert_eq!(
            single_feature_dir(&features),
            Some("p0-payments".to_string())
        );
        // More than one → None.
        std::fs::create_dir(features.join("p0-search")).unwrap();
        assert_eq!(single_feature_dir(&features), None);
    }

    /// `read_capped` returns `(None, false)` for a missing file
    /// rather than erroring. Phase 5 wants the agent to see "no
    /// charter set" without raising a tool error.
    #[test]
    fn read_capped_handles_missing_file() {
        let tmp = TempDir::new().unwrap();
        let (body, truncated) = read_capped(&tmp.path().join("nope.md"));
        assert_eq!(body, None);
        assert!(!truncated);
    }

    /// Files at or below `MAX_DOC_BYTES` are returned verbatim;
    /// larger files truncate with a marker and the truncation
    /// flag flips to true.
    #[test]
    fn read_capped_truncates_large_files() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("big.md");
        let body = "a".repeat(MAX_DOC_BYTES + 100);
        std::fs::write(&path, &body).unwrap();
        let (out, truncated) = read_capped(&path);
        let out = out.unwrap();
        assert!(truncated);
        assert!(out.ends_with("…[truncated]"));
        // Body length without the marker matches our cap.
        assert!(out.len() <= MAX_DOC_BYTES + "\n…[truncated]".len());
    }

    /// A file at the cap exactly is returned untruncated.
    #[test]
    fn read_capped_at_exact_cap_is_not_truncated() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("ok.md");
        let body = "a".repeat(MAX_DOC_BYTES);
        std::fs::write(&path, &body).unwrap();
        let (out, truncated) = read_capped(&path);
        assert!(!truncated);
        assert_eq!(out.unwrap().len(), MAX_DOC_BYTES);
    }
}
