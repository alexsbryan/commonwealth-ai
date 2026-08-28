// SPDX-License-Identifier: AGPL-3.0-or-later
//! The OmO layer of `svrn doctor` — the skill file, the editor hooks, and a
//! live MCP round-trip. These check the AGENT-FACING surface: a stack that is
//! healthy on every other layer is still unusable if the skill file is absent
//! or the MCP endpoint does not answer.

use std::path::PathBuf;

use super::probe::http_post_json;
use super::{CheckResult, CheckStatus, Layer, Repair};

pub(super) fn find_opencode_skill_dir() -> Option<PathBuf> {
    // Walk up from cwd looking for .opencode/
    let cwd = std::env::current_dir().ok()?;
    let mut dir = cwd.as_path();
    loop {
        let candidate = dir.join(".opencode").join("skills").join("sovereign-code");
        if candidate.exists() {
            return Some(candidate);
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => break,
        }
    }
    None
}

/// Does this SKILL.md carry the frontmatter a loader needs?
///
/// A skill is a markdown file whose YAML frontmatter declares at least
/// `name`. opencode parses the frontmatter and `safeParse`-fails
/// **silently** when `name` is absent — the skill is dropped with no
/// diagnostic at all. So a SKILL.md with no frontmatter is not "a bit
/// stale", it is a file that never loads, and reporting its mere
/// existence as health is a false green (ARCH §9).
///
/// Returns `Err(reason)` describing the first thing that would make a
/// loader reject it.
pub(super) fn skill_frontmatter_ok(body: &str) -> Result<(), String> {
    let body = body.strip_prefix('\u{feff}').unwrap_or(body);
    let rest = body
        .strip_prefix("---\n")
        .or_else(|| body.strip_prefix("---\r\n"))
        .ok_or_else(|| "no YAML frontmatter — the file must open with `---`".to_string())?;
    let end = rest
        .find("\n---\n")
        .or_else(|| rest.find("\n---\r\n"))
        .ok_or_else(|| "frontmatter is never closed with `---`".to_string())?;
    let front = &rest[..end];

    let has_key = |k: &str| {
        front.lines().any(|l| {
            l.trim_start().starts_with(&format!("{k}:"))
                && l.split_once(':').is_some_and(|(_, v)| !v.trim().is_empty())
        })
    };
    if !has_key("name") {
        return Err(
            "frontmatter has no non-empty `name:` — loaders drop the skill silently".into(),
        );
    }
    if !has_key("description") {
        return Err("frontmatter has no non-empty `description:`".into());
    }
    Ok(())
}

pub(super) fn check_skill_file() -> CheckResult {
    match find_opencode_skill_dir() {
        Some(skill_dir) => {
            let skill_md = skill_dir.join("SKILL.md");
            // Existence is not health — read it and validate.
            match std::fs::read_to_string(&skill_md) {
                Ok(body) => match skill_frontmatter_ok(&body) {
                    Ok(()) => CheckResult {
                        name: "skill_file",
                        layer: Layer::Omo,
                        status: CheckStatus::Passed,
                        message: format!("SKILL.md valid at {}", skill_md.display()),
                        repair: Repair::None,
                    },
                    Err(why) => CheckResult {
                        name: "skill_file",
                        layer: Layer::Omo,
                        status: CheckStatus::Failed,
                        message: format!("{}: {why}", skill_md.display()),
                        repair: Repair::Manual(
                            "Add YAML frontmatter with `name:` and `description:` — \
                             without it the skill is silently discarded"
                                .into(),
                        ),
                    },
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => CheckResult {
                    name: "skill_file",
                    layer: Layer::Omo,
                    status: CheckStatus::Failed,
                    message: format!("SKILL.md missing from {}", skill_dir.display()),
                    repair: Repair::Manual(
                        "Copy SKILL.md from sovereign/.opencode/skills/sovereign-code/".into(),
                    ),
                },
                Err(e) => CheckResult {
                    name: "skill_file",
                    layer: Layer::Omo,
                    status: CheckStatus::Failed,
                    message: format!("cannot read {}: {e}", skill_md.display()),
                    repair: Repair::None,
                },
            }
        }
        None => CheckResult {
            name: "skill_file",
            layer: Layer::Omo,
            status: CheckStatus::Warning,
            message: "no .opencode/skills/sovereign-code/ directory found — OmO not configured for this project".into(),
            repair: Repair::Manual(
                "Create .opencode/skills/sovereign-code/SKILL.md from sovereign template".into(),
            ),
        },
    }
}

/// Is the opencode config opencode ACTUALLY reads registering us, in
/// the shape it actually accepts?
///
/// This replaced a check for `.opencode/oh-my-opencode.jsonc`, a file
/// nothing has ever read. The real config is
/// `<config_dir>/opencode/opencode.json`, and `mcp` there is a flat
/// map of name → server whose `type` is `local` or `remote`. We shipped
/// `mcp.servers.sovereign` with `type: "http"` until 2026-07-28, which
/// opencode rejects — and no check noticed, because the old one only
/// asked whether an unrelated file existed.
pub(super) fn check_opencode_config() -> CheckResult {
    let path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("opencode")
        .join("opencode.json");

    let result = |status, message, repair| CheckResult {
        name: "opencode_config",
        layer: Layer::Omo,
        status,
        message,
        repair,
    };

    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(_) => {
            return result(
                CheckStatus::Warning,
                format!("{} not found — opencode not configured", path.display()),
                Repair::Manual("Run `svrn setup` to write it".into()),
            );
        }
    };
    let cfg: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            return result(
                CheckStatus::Failed,
                format!("{} is not valid JSON: {e}", path.display()),
                Repair::Manual("Fix the JSON by hand; setup refuses to overwrite it".into()),
            );
        }
    };

    if cfg.pointer("/mcp/servers/sovereign").is_some() {
        return result(
            CheckStatus::Failed,
            format!(
                "{} uses the legacy `mcp.servers.sovereign` shape — opencode reads `mcp` as a \
                 flat name → server map, so this entry fails its schema and the server is not \
                 registered",
                path.display()
            ),
            Repair::Manual("Re-run `svrn setup` — it now rewrites this entry".into()),
        );
    }
    match cfg.pointer("/mcp/sovereign/type").and_then(|v| v.as_str()) {
        Some("remote") => result(
            CheckStatus::Passed,
            format!("opencode registers sovereign at {}", path.display()),
            Repair::None,
        ),
        Some(other) => result(
            CheckStatus::Failed,
            format!("`mcp.sovereign.type` is \"{other}\" — opencode accepts only \"local\" or \"remote\""),
            Repair::Manual("Re-run `svrn setup`".into()),
        ),
        None => result(
            CheckStatus::Warning,
            format!("{} has no `mcp.sovereign` entry", path.display()),
            Repair::Manual("Run `svrn setup` to add it".into()),
        ),
    }
}

pub(super) async fn check_mcp_live() -> CheckResult {
    // MCP is JSON-RPC over `/mcp`; tools/list is a method, not a
    // path. See the same fix in `check_server_tools`.
    let resp = http_post_json(
        "http://localhost:9741/mcp",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
        }),
    )
    .await;
    match resp {
        Some(r) if r.status().is_success() => CheckResult {
            name: "mcp_live",
            layer: Layer::Omo,
            status: CheckStatus::Passed,
            message: "MCP /mcp tools/list round-trip succeeded".into(),
            repair: Repair::None,
        },
        _ => CheckResult {
            name: "mcp_live",
            layer: Layer::Omo,
            status: CheckStatus::Failed,
            message: "MCP /mcp unreachable — agents cannot use svrn tools".into(),
            repair: Repair::executable("svrn daemon restart"),
        },
    }
}
