// SPDX-License-Identifier: AGPL-3.0-or-later
//! BYOM (bring-your-own-model) GGUF path prompts — extracted from
//! `setup_cmd` (§3.2). Returns a fully-populated `super::ModelPaths`.

use std::path::PathBuf;

use super::{ModelPaths, Opts};

/// Prompt for all three GGUF paths. BYOM is committed — if the user
/// picked `[b]` from the numbered list, they wanted to supply their own
/// weights for every slot. No "blank to use default" shortcuts; a
/// blank line cancels the entire flow (setup exits). Paths are
/// validated for existence before we return; drag-and-drop quoting and
/// backslash-escaped spaces are stripped by `strip_quoting`.
pub(super) fn prompt_byom_paths(opts: &Opts) -> Result<ModelPaths, String> {
    if opts.yes {
        return Err("--yes cannot be combined with BYOM; choose a numbered option instead".into());
    }
    println!();
    println!("  Bring your own GGUF files. Provide a path for each role.");
    println!("  Leave any line blank to cancel.");
    println!();

    let primary = require_path(
        "  Main responder GGUF path: ",
        "main-responder path is required for BYOM",
    )?;
    let fast = require_path(
        "  Quick responder GGUF path: ",
        "quick-responder path is required for BYOM",
    )?;
    let embed = require_path(
        "  Knowledge embedder GGUF path: ",
        "knowledge-embedder path is required for BYOM",
    )?;
    // Code specialist is optional — blank input means "my Main
    // responder handles code fine, don't load a second substantive
    // model." Users who want a dedicated coder can add one later
    // via Settings.
    let code = prompt_path("  Code specialist GGUF path (optional, Enter to skip): ")?;
    Ok(ModelPaths {
        primary,
        fast,
        embed,
        code,
    })
}

/// Prompt for a path, error out if the user leaves it blank.
/// Thin wrapper over `prompt_path` that turns `Ok(None)` into an error.
fn require_path(label: &str, missing_msg: &str) -> Result<PathBuf, String> {
    match prompt_path(label)? {
        Some(p) => Ok(p),
        None => Err(missing_msg.to_string()),
    }
}

/// BYOM path prompt. Delegates to `util::prompts::prompt_path` which
/// handles quote stripping, `~/` expansion, and existence checking.
fn prompt_path(label: &str) -> Result<Option<PathBuf>, String> {
    sovereign_cli_shared::prompts::prompt_path(label)
}
