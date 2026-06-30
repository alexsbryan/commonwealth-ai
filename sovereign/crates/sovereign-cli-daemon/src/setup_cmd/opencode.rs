// SPDX-License-Identifier: AGPL-3.0-or-later
//! opencode config install/merge — extracted from `setup_cmd` (§3.2).
//! Writes or merges Sovereign's MCP server + provider into the user's
//! `~/.config/opencode/opencode.json` without clobbering other entries.

use std::path::{Path, PathBuf};

/// Outcome of attempting to install / update the opencode config.
/// Carries the path so the banner prints something actionable
/// instead of "ok".
#[derive(Debug)]
pub(super) enum OpencodeInstall {
    /// File didn't exist; we created it from scratch.
    Created(PathBuf),
    /// File existed; we merged Sovereign's MCP server + provider
    /// into the existing JSON without disturbing the user's other
    /// providers, models, skills, etc.
    MergedInto(PathBuf),
    /// File existed and already contained an entry pointing at our
    /// daemon — nothing to do.
    AlreadyConfigured(PathBuf),
}

fn opencode_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("opencode")
        .join("opencode.json")
}

/// Render the JSON snippet we'd write — used both to seed a new file
/// and to print as a fallback when writing fails.
pub(super) fn opencode_config_snippet(client_port: u16) -> String {
    let value = serde_json::json!({
        "mcp": {
            "servers": {
                "sovereign": {
                    "type": "http",
                    "url": format!("http://localhost:{client_port}/mcp")
                }
            }
        },
        "provider": {
            "commonwealth": {
                "npm": "@ai-sdk/openai-compatible",
                "options": {
                    "baseURL": format!("http://localhost:{client_port}/v1")
                }
            }
        }
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| String::new())
}

/// Install or merge our opencode entries into `~/.config/opencode/opencode.json`.
///
/// Behaviour matrix (decided so a re-run of `svrn setup` is
/// always safe and never clobbers third-party config):
///   - file missing               → create with our entries only.
///   - file present, no overlap   → merge: add `mcp.servers.sovereign`
///                                  and `provider.commonwealth`,
///                                  leave everything else untouched.
///   - file present, our entries  → noop, return `AlreadyConfigured`.
///   - file present, parse error  → bail (the user has invalid JSON;
///                                  we shouldn't try to "fix" it).
pub(super) fn install_opencode_config(client_port: u16) -> Result<OpencodeInstall, String> {
    install_opencode_config_at(&opencode_config_path(), client_port)
}

pub(super) fn install_opencode_config_at(path: &Path, client_port: u16) -> Result<OpencodeInstall, String> {
    // Fresh install — easy path.
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        std::fs::write(path, opencode_config_snippet(client_port))
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        return Ok(OpencodeInstall::Created(path.to_path_buf()));
    }

    // Existing file — parse, merge our keys, write back.
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut cfg: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    if !cfg.is_object() {
        return Err(format!(
            "{} is not a JSON object — refusing to overwrite",
            path.display()
        ));
    }

    let mcp_url = format!("http://localhost:{client_port}/mcp");
    let base_url = format!("http://localhost:{client_port}/v1");

    // Detect already-configured to give the user a "nothing to do"
    // banner instead of pretending we did work.
    let same_mcp = cfg
        .pointer("/mcp/servers/sovereign/url")
        .and_then(|v| v.as_str())
        == Some(mcp_url.as_str());
    let same_provider = cfg
        .pointer("/provider/commonwealth/options/baseURL")
        .and_then(|v| v.as_str())
        == Some(base_url.as_str());
    if same_mcp && same_provider {
        return Ok(OpencodeInstall::AlreadyConfigured(path.to_path_buf()));
    }

    // Walk into mcp.servers.sovereign and provider.commonwealth,
    // creating intermediate objects only as needed. Other keys at
    // each level are preserved verbatim.
    let obj = cfg.as_object_mut().expect("verified above");
    let mcp = obj
        .entry("mcp".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let mcp_obj = mcp
        .as_object_mut()
        .ok_or_else(|| "`mcp` is not an object".to_string())?;
    let servers = mcp_obj
        .entry("servers".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let servers_obj = servers
        .as_object_mut()
        .ok_or_else(|| "`mcp.servers` is not an object".to_string())?;
    servers_obj.insert(
        "sovereign".to_string(),
        serde_json::json!({ "type": "http", "url": mcp_url }),
    );

    let provider = obj
        .entry("provider".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let provider_obj = provider
        .as_object_mut()
        .ok_or_else(|| "`provider` is not an object".to_string())?;
    provider_obj.insert(
        "commonwealth".to_string(),
        serde_json::json!({
            "npm": "@ai-sdk/openai-compatible",
            "options": { "baseURL": base_url }
        }),
    );

    let pretty =
        serde_json::to_string_pretty(&cfg).map_err(|e| format!("serialize merged config: {e}"))?;
    std::fs::write(path, pretty).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(OpencodeInstall::MergedInto(path.to_path_buf()))
}
