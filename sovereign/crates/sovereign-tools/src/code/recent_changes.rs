//! Symbols modified within the last N hours — exact, by file mtime.
//!
//! Trust contract: this is labelled "always correct" in the skill prompt.
//! The underlying column is the file's on-disk `mtime` at ingest time, so
//! results are exactly "what `find . -newer` would report" at the moment
//! the index was built (or last updated by the watcher in Phase 3).

use std::sync::Arc;

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::CorpusEngine;

use super::{group_by_file, query_all_code_indexes};

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

#[async_trait]
impl Tool for RecentChangesTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "recent_changes".to_string(),
            name: "Recent Changes".to_string(),
            description: "List symbols in files modified within the last N \
                          hours. Exact — based on file mtime at index time. \
                          Useful for orientation after a pull or reviewing \
                          recent work."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "hours": {
                        "type": "integer",
                        "description": "How far back to look (hours).",
                        "default": 24,
                        "minimum": 1
                    }
                }
            }),
            examples: vec![
                ToolExample {
                    situation: "You're starting a session and want to understand what's been actively worked on before diving in. More useful than 'git log' because it shows the actual symbols changed, not just file names.".into(),
                    call: serde_json::json!({ "hours": 24 }),
                },
                ToolExample {
                    situation: "Something broke and you want to know what changed recently that could have caused it. Narrows the search to files actually modified in the last hour.".into(),
                    call: serde_json::json!({ "hours": 2 }),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "string",
                "description": "Markdown, grouped by file, showing symbols modified \
                                within the last N hours. Each symbol line includes \
                                `name  [kind]  mtime_ago`."
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        if let Some(h) = params.get("hours").and_then(|v| v.as_u64()) {
            if h == 0 {
                return Err(Error::InvalidInput("hours must be positive".into()));
            }
        }
        Ok(())
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let hours = params.get("hours").and_then(|v| v.as_u64()).unwrap_or(DEFAULT_HOURS);
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
}
