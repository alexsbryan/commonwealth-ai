// SPDX-License-Identifier: AGPL-3.0-or-later
//! The duplicate-route test (pb-mesh-exit-transport): no path cw-rails serves
//! is also mounted by svrn's daemon, except the kept `/v1/rail/*` forward —
//! svrn's grant-checking door in front of cw-rails' rail (a guest's
//! namespace is svrn's to decide). The flip moved every mesh route to
//! cw-rails; the daemon answers its old paths from one list
//! (`sovereign_daemon::mesh_http::MOVED_TO_RAILS`) with a 410 naming
//! cw-rails, and mounts none of them with a handler of its own (principle 8:
//! one home per route).
//!
//! It lives here because it reads two programs' source, and neither program
//! may link the other or climb out of its own tree.

use std::path::{Path, PathBuf};

#[path = "shared/repo_root.rs"]
mod repo_root;
use repo_root::repo_root;

/// Production `.rs` files under `dir`: split-out test modules (`tests.rs`,
/// `*_tests.rs`, anything under a `tests/` directory) are skipped.
fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "tests" {
                rs_files(&path, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
            out.push(path);
        }
    }
}

/// `text` before its first `#[cfg(test)]` that opens a module (`mod …` or a
/// `#[path]`-declared one): an inline `#[cfg(test)]` on one item further up
/// must not drop the production routes beneath it.
fn production_part(text: &str) -> &str {
    let mut from = 0;
    while let Some(at) = text[from..].find("#[cfg(test)]") {
        let start = from + at;
        let after = text[start + "#[cfg(test)]".len()..].trim_start();
        if after.starts_with("mod ") || after.starts_with("#[path") {
            return &text[..start];
        }
        from = start + 1;
    }
    text
}

/// Every string-literal path mounted with `.route(` in `dir`'s production
/// source.
fn mounted(dir: &Path) -> std::collections::BTreeSet<String> {
    let mut files = Vec::new();
    rs_files(dir, &mut files);
    let mut out = std::collections::BTreeSet::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let mut rest = production_part(&text);
        while let Some(at) = rest.find(".route(") {
            rest = &rest[at + ".route(".len()..];
            let trimmed = rest.trim_start();
            if let Some(lit) = trimmed.strip_prefix('"') {
                if let Some(end) = lit.find('"') {
                    out.insert(lit[..end].to_string());
                }
            }
        }
    }
    out
}

#[test]
fn no_route_cw_rails_serves_is_mounted_by_svrn_but_the_rail_forward() {
    let root = repo_root();
    let rails = mounted(&root.join("cmnwlth/crates/commonwealth-rails/src"));
    let svrn = mounted(&root.join("sovereign/crates/sovereign-daemon/src"));
    assert!(
        rails.contains("/v1/mesh/status") && rails.contains("/internal/gossip"),
        "the census never read cw-rails' routes under {}: {rails:?}",
        root.display()
    );
    assert!(
        svrn.contains("/v1/mesh/venues"),
        "the census never read svrn's routes: {svrn:?}"
    );
    let both: Vec<&String> = rails.intersection(&svrn).collect();
    let duplicated: Vec<&&String> = both
        .iter()
        .filter(|p| !p.starts_with("/v1/rail/"))
        .collect();
    assert!(
        duplicated.is_empty(),
        "svrn mounts routes cw-rails serves: {duplicated:?}. Since pb-mesh-exit-transport \
         a mesh route has one home; an old path answers 410 through \
         `sovereign_daemon::mesh_http::MOVED_TO_RAILS`"
    );
    assert!(
        both.iter().any(|p| p.starts_with("/v1/rail/")),
        "the one exception, svrn's `/v1/rail/*` forward, is gone from the census: {both:?}"
    );
}
