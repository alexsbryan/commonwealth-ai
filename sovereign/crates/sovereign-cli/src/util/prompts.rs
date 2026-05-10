//! Stdin prompt helpers.
//!
//! Every subcommand that talks to the user shares a few interaction
//! primitives: yes/no confirm, a line of free text, a filesystem path,
//! a numbered pick from a list. Before this module, those were
//! reimplemented inline in `main.rs`, `project_cmd.rs`, `setup_cmd.rs`,
//! `mesh_cmd.rs`, and `reflect_cmd.rs` — with subtle differences
//! (stderr flush or not, `y`/`yes` vs `y` only, trim-and-lowercase or
//! just trim, etc.). This file picks one behaviour for each and makes
//! every call site consistent.
//!
//! Convention throughout:
//! - Prompts render to **stderr** (so piping stdout still works).
//! - `stderr` is flushed before `read_line` so the prompt always shows.
//! - `Ctrl-D` (EOF) returns `None` / `false` / cancels, matching shell
//!   idioms.

use std::io::{self, BufRead as _, IsTerminal as _, Write as _};
use std::path::PathBuf;

/// Yes/no confirmation. `default_yes = true` renders `[Y/n]`, accepts
/// empty / `y` / `yes` as true. `default_yes = false` renders `[y/N]`,
/// accepts empty / `n` / `no` as false (i.e. default-on-enter matches
/// the shown capital).
///
/// Returns `default_yes` on EOF or read error — the user has no way to
/// tell us "cancel" without typing, so we fall back to the polite
/// default.
pub fn confirm(prompt: &str, default_yes: bool) -> bool {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    eprint!("{prompt} {hint} ");
    io::stderr().flush().ok();
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        return default_yes;
    }
    match line.trim().to_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default_yes, // ambiguous input falls through to the shown default
    }
}

/// Read one line of free text. Returns `None` on EOF, `Some(String)`
/// otherwise (including empty string — callers decide whether blank
/// is valid).
pub fn prompt_string(prompt: &str) -> Option<String> {
    eprint!("{prompt}");
    io::stderr().flush().ok();
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        return None;
    }
    Some(line.trim().to_string())
}

/// Prompt for a filesystem path. Strips drag-and-drop quoting
/// (`'...'` / `"..."` / backticks), un-escapes `\ ` → ` `, expands
/// leading `~/`, verifies the path exists.
///
/// - `Ok(None)` → user entered a blank line (intent: cancel the flow).
/// - `Ok(Some(path))` → path is valid and exists on disk.
/// - `Err(msg)` → path doesn't exist; message is user-ready.
pub fn prompt_path(prompt: &str) -> Result<Option<PathBuf>, String> {
    eprint!("{prompt}");
    io::stderr().flush().ok();
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
    let trimmed = strip_quoting(line.trim());
    if trimmed.is_empty() {
        return Ok(None);
    }
    let expanded = if let Some(rest) = trimmed.strip_prefix("~/") {
        dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(&trimmed))
    } else {
        PathBuf::from(&trimmed)
    };
    if !expanded.exists() {
        return Err(format!("file not found: {}", expanded.display()));
    }
    Ok(Some(expanded))
}

/// Strip surrounding single quotes, double quotes, or backticks from a
/// pasted path, and un-escape `\ ` → ` `. macOS Terminal and iTerm
/// wrap drag-and-dropped paths with single quotes by default; without
/// this the surrounding literal quotes get baked into the `PathBuf`
/// and `exists()` always fails.
///
/// Only symmetric matching pairs are stripped — a path like
/// `foo'bar` (unbalanced) is returned as-is.
pub fn strip_quoting(input: &str) -> String {
    let s = input.trim();
    let stripped = if s.len() >= 2 {
        let first = s.chars().next().unwrap();
        let last = s.chars().last().unwrap();
        if (first == '\'' && last == '\'')
            || (first == '"' && last == '"')
            || (first == '`' && last == '`')
        {
            &s[1..s.len() - 1]
        } else {
            s
        }
    } else {
        s
    };
    stripped.replace("\\ ", " ")
}

/// Whether stdin is attached to a terminal. Tests and CI run with a
/// non-TTY stdin; callers should pre-check this before calling any
/// prompt function that blocks, and fall back to non-interactive
/// defaults.
pub fn stdin_is_tty() -> bool {
    io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_quoting_removes_single_quotes_from_drag_and_drop() {
        assert_eq!(
            strip_quoting("'/Users/alice/models/qwen.gguf'"),
            "/Users/alice/models/qwen.gguf"
        );
    }

    #[test]
    fn strip_quoting_removes_double_quotes() {
        assert_eq!(
            strip_quoting("\"/Users/alice/models/qwen.gguf\""),
            "/Users/alice/models/qwen.gguf"
        );
    }

    #[test]
    fn strip_quoting_removes_backticks() {
        assert_eq!(strip_quoting("`/Users/alice/my model.gguf`"), "/Users/alice/my model.gguf");
    }

    #[test]
    fn strip_quoting_preserves_unquoted_path() {
        assert_eq!(strip_quoting("/abs/path"), "/abs/path");
    }

    #[test]
    fn strip_quoting_mismatched_quotes_left_alone() {
        assert_eq!(strip_quoting("'foo\"bar"), "'foo\"bar");
    }

    #[test]
    fn strip_quoting_unescapes_backslash_space() {
        assert_eq!(
            strip_quoting("/Users/alice/my\\ models/qwen.gguf"),
            "/Users/alice/my models/qwen.gguf"
        );
    }

    #[test]
    fn strip_quoting_handles_empty_input() {
        assert_eq!(strip_quoting(""), "");
        assert_eq!(strip_quoting("'"), "'");
    }
}
