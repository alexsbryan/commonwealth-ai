// SPDX-License-Identifier: AGPL-3.0-or-later
//! The svrn daemon assembles and rebuilds no engine on any path
//! (pb-serve-distributes): serving is serve's, dialed or hosted by the
//! distribution, and a terminal forwards to its entry node. No
//! `ReloadSource::Assembly` and no call into the serving assembly survive
//! here (§2c engine assembly, 3 → 2).
//!
//! The compiler cannot keep one from coming back while the daemon still
//! links the loader, so this reads the daemon's sources. Watched red by a
//! `ReloadSource::Assembly` arm back in provider.rs.

use std::path::{Path, PathBuf};

/// The engine's assembly and rebuild, assembled so this file does not match
/// itself.
fn needles() -> Vec<String> {
    vec![
        ["ReloadSource::", "Assembly"].concat(),
        ["assemble", "_serving("].concat(),
        ["Reload", "Factory"].concat(),
        ["Embedded", "LlamaCpp"].concat(),
    ]
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_daemon_assembles_and_rebuilds_no_engine() {
    let krate = Path::new(env!("CARGO_MANIFEST_DIR"));
    let needles = needles();
    let mut found = Vec::new();
    let mut files = Vec::new();
    rust_files(&krate.join("src"), &mut files);
    for file in &files {
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        for (n, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if let Some(needle) = needles.iter().find(|needle| code.contains(needle.as_str())) {
                found.push(format!("{}:{}: `{needle}`", file.display(), n + 1));
            }
        }
    }
    assert!(
        files.len() > 50,
        "the census read only {} files — it is not looking",
        files.len()
    );
    assert!(
        found.is_empty(),
        "the daemon assembles or rebuilds an engine; serving is serve's \
         (dial it, or let the distribution host it):\n  {}",
        found.join("\n  ")
    );
}
