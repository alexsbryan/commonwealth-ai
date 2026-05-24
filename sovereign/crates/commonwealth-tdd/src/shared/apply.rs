//! Apply a [`ParsedResponse`] to a workdir via the shared executor.
//!
//! Routes each [`EditAction`] variant to its matching
//! [`commonwealth_agent_tools::Primitive`]. Same code path the bench
//! search runner uses today — so the TDD machine and the bench can
//! never drift in how a model emission maps to a write.

use commonwealth_agent_tools::executor::{execute, ExecCtx};
use commonwealth_agent_tools::{
    PatchFileArgs, Primitive, ReplaceFunctionArgs, ToolError, WriteFileArgs,
};

use crate::shared::edit::{EditAction, ParsedResponse};

pub async fn apply_edit(
    ctx: &ExecCtx,
    source_file: &str,
    response: &ParsedResponse,
) -> Result<(), ToolError> {
    let primitive = match &response.action {
        EditAction::RewriteFunction { name } => Primitive::ReplaceFunction(ReplaceFunctionArgs {
            path: source_file.to_string(),
            name: name.clone(),
            new_body: response.body.clone(),
        }),
        EditAction::PatchLines { start, end } => Primitive::PatchFile(PatchFileArgs {
            path: source_file.to_string(),
            start_line: *start,
            end_line: *end,
            new_content: response.body.clone(),
        }),
        EditAction::InsertBefore { line } => {
            // No InsertBefore primitive — emulate via patch_lines on
            // the target line, prepending body to its original content.
            // Matches the bench runner's behavior exactly (lifted from
            // sovereign-agent-bench/src/runners/shared.rs).
            let abs = ctx.workdir.join(source_file);
            let existing = tokio::fs::read_to_string(&abs)
                .await
                .map_err(|e| ToolError::Filesystem {
                    primitive: "insert_before",
                    reason: format!("read {source_file}: {e}"),
                })?;
            let lines: Vec<&str> = existing.lines().collect();
            let line_idx = (*line as usize).saturating_sub(1);
            if line_idx > lines.len() {
                return Err(ToolError::InvalidArguments {
                    primitive: "insert_before",
                    reason: format!(
                        "line {line} out of range for {source_file} ({} lines)",
                        lines.len()
                    ),
                });
            }
            let existing_at_line = lines.get(line_idx).copied().unwrap_or("");
            let new_content = if line_idx >= lines.len() {
                response.body.clone()
            } else {
                format!("{}\n{existing_at_line}", response.body.trim_end_matches('\n'))
            };
            Primitive::PatchFile(PatchFileArgs {
                path: source_file.to_string(),
                start_line: *line,
                end_line: *line,
                new_content,
            })
        }
        EditAction::WriteFile { path } => {
            // Model-supplied path wins; falls back to the caller's
            // source-file discovery default for the v1 in-place shape.
            let target = path.clone().unwrap_or_else(|| source_file.to_string());
            Primitive::WriteFile(WriteFileArgs {
                path: target,
                content: response.body.clone(),
            })
        }
    };
    execute(ctx, &primitive).await.map(|_| ())
}
