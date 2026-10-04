// SPDX-License-Identifier: AGPL-3.0-or-later
//! No `.rs` file in the workspace matches an MCP method name as a string
//! pattern (`"tools/call" =>`); every mount parses into
//! `oicp_types::mcp::McpMethod` and matches on the variant (phase-b pb-mcp,
//! principle 9). Three mounts matched strings before the enum existed, and
//! a fourth copy is how a method gets spelled two ways.
//!
//! It lives here and not beside the enum because it reads the whole
//! workspace, and `oicp-types` is a shared leaf that must lift with its
//! tests (boundary-gate refuses a leaf test that climbs out of its crate).

use std::path::{Path, PathBuf};

use oicp_types::mcp::McpMethod;

#[path = "shared/repo_root.rs"]
mod repo_root;
use repo_root::repo_root;

/// Every `.rs` file under `dir`, skipping build output (`target*`), vendored
/// code, `node_modules` and dot-directories.
fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !(name.starts_with('.')
                || name.starts_with("target")
                || name == "node_modules"
                || name == "vendor")
            {
                rs_files(&path, out);
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_mcp_method_match_goes_through_the_enum() {
    let root = repo_root();
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    let host_kit = root.join("shared/crates/host-kit/src/mcp.rs");
    assert!(
        files.contains(&host_kit),
        "the walk under {} never reached {} — it judged the wrong tree",
        root.display(),
        host_kit.display()
    );
    let needles: Vec<String> = McpMethod::ALL
        .iter()
        .flat_map(|m| {
            [
                format!("\"{}\" =>", m.as_str()),
                format!("\"{}\" |", m.as_str()),
            ]
        })
        .collect();
    let mut hits = Vec::new();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            if !line.trim_start().starts_with("//")
                && needles.iter().any(|needle| line.contains(needle.as_str()))
            {
                hits.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "MCP method matched as a string; match on oicp_types::mcp::McpMethod:\n{}",
        hits.join("\n")
    );
}
