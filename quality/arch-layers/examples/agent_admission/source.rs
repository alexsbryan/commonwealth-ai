// SPDX-License-Identifier: AGPL-3.0-or-later
use super::fixture;
use serde_json::{json, Value};
use std::{path::Path, process::Command};

pub const BASE: &str = "8830c59c457098c2ea852c5ad864744e86630c77";
pub const PATHS: &[&str] = &[
    "corpus-index/src/source.rs",
    "corpus-engine/src/engine/mod.rs",
    "quality/ARCH_LAYERS.toml",
];

pub fn complete_interface(text: &str) -> bool {
    let base = include_str!("../../tests/fixtures/core-read/base-index-source.rs");
    let (_, declaration) = base
        .split_once("pub trait IndexSource")
        .expect("pinned trait exists");
    let end = declaration.find('}').expect("pinned trait closes");
    text.contains(&format!("pub trait IndexSource{}", &declaration[..=end]))
}

pub fn lookup(repository: &Path, path: &str, start: usize, end: usize) -> Result<Value, String> {
    if !PATHS.contains(&path) || start == 0 || start > end || end - start >= 120 {
        return Err("lookup requires a permitted BASE path and at most 120 lines".into());
    }
    let output = Command::new("git")
        .current_dir(repository)
        .args(["show", &format!("{BASE}:{path}")])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let body = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
    if path == PATHS[0]
        && body != include_str!("../../tests/fixtures/core-read/base-index-source.rs")
    {
        return Err("historical IndexSource disagrees with the pinned projection input".into());
    }
    let lines: Vec<_> = body.lines().collect();
    if end > lines.len() {
        return Err("lookup source window is absent at BASE".into());
    }
    let text = lines[start - 1..end].join("\n");
    Ok(
        json!({"kind":"historical-source", "base":BASE, "path":path, "lines":[start,end],
              "source_sha256":fixture::hash(body.as_bytes()), "text":text}),
    )
}
