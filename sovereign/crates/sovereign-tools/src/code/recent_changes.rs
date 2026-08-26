// SPDX-License-Identifier: AGPL-3.0-or-later
//! Symbols modified within the last N hours — exact, by file mtime.
//!
//! Trust contract: this is labelled "always correct" in the skill prompt.
//! The underlying column is the file's on-disk `mtime` at ingest time, so
//! results are exactly "what `find . -newer` would report" at the moment
//! the index was built (or last updated by the watcher in Phase 3).

use std::sync::Arc;


use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use corpus_engine::CorpusEngine;

use super::{group_by_file, query_all_code_indexes};
use sovereign_core::tool_manifest::DeclaredTool;

/// Default window if the caller omits `hours`.
const DEFAULT_HOURS: u64 = 24;

/// Hard cap on the number of rows fetched per corpus.
const MAX_ROWS_PER_CORPUS: usize = 400;

/// Upper bound on files + symbols-per-file surfaced in the response body.
const MAX_FILES_RENDERED: usize = 20;
const MAX_SYMBOLS_PER_FILE: usize = 15;

pub struct RecentChangesTool {
    engine: Arc<CorpusEngine>,
}

impl RecentChangesTool {
    pub fn new(engine: Arc<CorpusEngine>) -> Self {
        Self { engine }
    }
}

impl RecentChangesTool {
    /// Bind this tool's state to its `recent_changes` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("recent_changes", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_validate({
            let state = Arc::clone(&state);
            Arc::new(move |p: &serde_json::Value| state.validate_extra(p))
        })
    }

    /// The executable half of `recent_changes`.
    async fn run(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let hours = params
            .get("hours")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_HOURS);
        let since = chrono::Utc::now().timestamp() - (hours as i64 * 3600);

        // `symbol_name IS NOT NULL` implicitly skips non-code corpora
        // where every code column is Null — they can't match `mtime > …`
        // anyway but we make the intent explicit.
        let filter = format!("symbol_name IS NOT NULL AND mtime > {since}");

        let rows = query_all_code_indexes(&self.engine, &filter, MAX_ROWS_PER_CORPUS)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "recent_changes".to_string(),
                message: e.to_string(),
            })?;

        if rows.is_empty() {
            return Ok(StepOutput::Text(format!(
                "No code changes in the last {hours} hours."
            )));
        }

        let groups = group_by_file(&rows);
        let total_files = groups.len();

        let mut out = format!(
            "**Changed in the last {hours} hours** — {total_files} file{s}\n\n",
            s = if total_files == 1 { "" } else { "s" }
        );
        for (file, syms) in groups.into_iter().take(MAX_FILES_RENDERED) {
            out.push_str(&format!("**`{file}`**\n"));
            let shown = syms.len().min(MAX_SYMBOLS_PER_FILE);
            for sym in syms.iter().take(shown) {
                out.push_str(&format!(
                    "  `{}` ({}) line {}\n",
                    sym.symbol_name,
                    sym.symbol_kind,
                    sym.line_start + 1,
                ));
            }
            if syms.len() > shown {
                out.push_str(&format!("  …and {} more\n", syms.len() - shown));
            }
            out.push('\n');
        }
        if total_files > MAX_FILES_RENDERED {
            out.push_str(&format!(
                "_…and {} more files not shown_\n",
                total_files - MAX_FILES_RENDERED
            ));
        }

        Ok(StepOutput::Text(out))
    }

    fn validate_extra(&self, params: &serde_json::Value) -> Result<()> {

        if let Some(h) = params.get("hours").and_then(|v| v.as_u64()) {
            if h == 0 {
                return Err(Error::InvalidInput("hours must be positive".into()));
            }
        }
        Ok(())
    }
}
