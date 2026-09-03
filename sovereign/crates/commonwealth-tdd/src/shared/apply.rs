// SPDX-License-Identifier: AGPL-3.0-or-later
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
            let existing =
                tokio::fs::read_to_string(&abs)
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
                format!(
                    "{}\n{existing_at_line}",
                    response.body.trim_end_matches('\n')
                )
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
        EditAction::MoveLines { src, start, end, dest } => {
            // Relocation without generation — the model names the span,
            // this moves the bytes. Deterministic, so it is testable
            // without a model and cannot truncate.
            let src_path = src.clone().unwrap_or_else(|| source_file.to_string());
            let abs_src = ctx.workdir.join(&src_path);
            let existing = tokio::fs::read_to_string(&abs_src).await.map_err(|e| {
                ToolError::Filesystem {
                    primitive: "move_lines",
                    reason: format!("read {src_path}: {e}"),
                }
            })?;
            let mut lines: Vec<&str> = existing.lines().collect();
            let s = (*start as usize).saturating_sub(1);
            let e = (*end as usize).min(lines.len());
            if *start < 1 || s >= lines.len() || e < s {
                return Err(ToolError::InvalidArguments {
                    primitive: "move_lines",
                    reason: format!(
                        "range {start}..{end} out of range for {src_path} ({} lines)",
                        lines.len()
                    ),
                });
            }
            let moved_block = lines[s..e].join("\n");
            let dest_abs = ctx.workdir.join(dest);
            if let Some(parent) = dest_abs.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| {
                    ToolError::Filesystem {
                        primitive: "move_lines",
                        reason: format!("mkdir {}: {e}", parent.display()),
                    }
                })?;
            }
            // Existing dest → append; fresh dest → the block opens it.
            let mut dest_content =
                tokio::fs::read_to_string(&dest_abs).await.unwrap_or_default();
            if !dest_content.is_empty() && !dest_content.ends_with('\n') {
                dest_content.push('\n');
            }
            dest_content.push_str(&moved_block);
            dest_content.push('\n');
            tokio::fs::write(&dest_abs, dest_content).await.map_err(|e| {
                ToolError::Filesystem {
                    primitive: "move_lines",
                    reason: format!("write {dest}: {e}"),
                }
            })?;
            lines.drain(s..e);
            tokio::fs::write(&abs_src, lines.join("\n") + "\n")
                .await
                .map_err(|e| ToolError::Filesystem {
                    primitive: "move_lines",
                    reason: format!("rewrite {src_path}: {e}"),
                })?;
            return Ok(());
        }
    };
    execute(ctx, &primitive).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::edit::parse_response;
    use std::path::PathBuf;

    fn ctx(dir: &std::path::Path) -> ExecCtx {
        ExecCtx {
            workdir: dir.to_path_buf(),
            subprocess_wall_cap: std::time::Duration::from_secs(10),
            build_cmd: String::new(),
            verify_cmd: String::new(),
            syntax_validator: None,
        }
    }

    /// The emission the split rounds actually produce: a bare move_lines
    /// header, no source block. Parses, applies, and the bytes land in
    /// the destination exactly as they left the source.
    #[tokio::test]
    async fn move_lines_relocates_a_span_without_emitting_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub mod big;\n\nfn keep_me() {}\n\nfn moved_a() {}\nfn moved_b() {}\n",
        )
        .unwrap();
        let response = parse_response(
            "Plan: move the two moved_ fns.\n```json\n{\"action\": \"move_lines\", \"start\": 5, \"end\": 6, \"dest\": \"src/big.rs\"}\n```",
        )
        .expect("bare move_lines parses");
        assert!(matches!(response.action, EditAction::MoveLines { .. }));
        apply_edit(&ctx(dir.path()), "lib.rs", &response)
            .await
            .expect("apply");
        let src = std::fs::read_to_string(dir.path().join("lib.rs")).unwrap();
        let dest = std::fs::read_to_string(dir.path().join("src/big.rs")).unwrap();
        assert_eq!(src.lines().count(), 4, "the span left the source");
        assert!(src.contains("fn keep_me"));
        assert!(dest.contains("fn moved_a"));
        assert!(dest.contains("fn moved_b"));
    }

    /// A second move_lines into the same destination APPENDS — a round
    /// that relocates several spans to one new module composes.
    #[tokio::test]
    async fn move_lines_appends_to_an_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "a\nb\nc\nd\n").unwrap();
        std::fs::write(dir.path().join("out.rs"), "// header\n").unwrap();
        let response = parse_response(
            "```json\n{\"action\": \"move_lines\", \"start\": 1, \"end\": 2, \"dest\": \"out.rs\"}\n```",
        )
        .unwrap();
        apply_edit(&ctx(dir.path()), "lib.rs", &response)
            .await
            .unwrap();
        let dest = std::fs::read_to_string(dir.path().join("out.rs")).unwrap();
        assert_eq!(dest, "// header\na\nb\n");
        let src = std::fs::read_to_string(dir.path().join("lib.rs")).unwrap();
        assert_eq!(src.lines().count(), 2);
        let _ = PathBuf::new();
    }

    /// The split shape: SEVERAL move_lines headers in one emission —
    /// consecutive json blocks must ALL parse, not collapse to the last.
    #[test]
    fn consecutive_move_lines_actions_all_parse() {
        let edits = crate::shared::parse_response_edits(
            "```json\n{\"action\": \"move_lines\", \"start\": 1845, \"end\": 3010, \"dest\": \"src/x/judge_tests.rs\"}\n```\n```json\n{\"action\": \"move_lines\", \"start\": 100, \"end\": 240, \"dest\": \"src/x/prompts.rs\"}\n```",
        );
        assert_eq!(edits.len(), 2, "both relocations parse: {edits:?}");
        assert!(edits.iter().all(|e| matches!(e.action, EditAction::MoveLines { .. })));
    }

    #[tokio::test]
    async fn an_out_of_range_span_is_a_named_error_not_a_silent_noop() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "one line\n").unwrap();
        let response = parse_response(
            "```json\n{\"action\": \"move_lines\", \"start\": 5, \"end\": 9, \"dest\": \"out.rs\"}\n```",
        )
        .unwrap();
        let err = apply_edit(&ctx(dir.path()), "lib.rs", &response)
            .await
            .expect_err("out-of-range must fail");
        assert!(format!("{err}").contains("out of range"));
    }
}
