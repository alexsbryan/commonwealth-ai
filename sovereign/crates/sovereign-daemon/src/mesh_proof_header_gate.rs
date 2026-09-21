// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ratchet behind "one outbound stamp" — a test, not a comment.
//!
//! `x-mesh-proof` is minted in exactly one place
//! (`commonwealth_transport::mesh_proof`) and read in exactly one other
//! (`sovereign_daemon::internal_principal`). A third production file
//! spelling the literal would be a second sender or a second reader — the
//! shape ARCH principle 8 forbids, and the shape `x-node-id` had before
//! [`crate::mesh_principal_gate`] closed it: nine sites, each deciding for
//! itself what the header meant.
//!
//! A grep and not a lint for the reason that module gives: a comment saying
//! "mint it through the one function" is exactly what a drifting call site
//! ignores. This fails the normal test run instead, naming the file.
//!
//! Both trees are walked, because the minting side is in `commonwealth/` and
//! the reading side in `sovereign/`. Test code is exempt twice over —
//! `tests/` trees are not walked, and everything from a file's first
//! `#[cfg(test)]` on is skipped — because typing the header is how a refusal
//! is proved.

/// The two production files that may spell the literal: the one that mints
/// it and the one that reads it. Repo-relative, matched as a suffix so the
/// test runs from any cwd.
pub const PROOF_HEADER_ALLOWED: &[&str] = &[
    "commonwealth/crates/commonwealth-transport/src/mesh_proof.rs",
    "sovereign/crates/sovereign-daemon/src/internal_principal.rs",
];

/// The wire form, in both spellings a Rust file could hold it in.
pub const PROOF_WIRE_FORM: &[&str] = &["x-mesh-proof", "X-Mesh-Proof"];

/// The trees walked. `commonwealth/` because the stamp lives there,
/// `sovereign/` because the resolver does.
pub const PROOF_SCANNED_TREES: &[&str] = &["commonwealth/crates", "sovereign/crates"];

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn production_sources(root: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if name != "tests" && name != "target" {
                    production_sources(&path, out);
                }
            } else if name.ends_with(".rs") && !name.ends_with("_tests.rs") && name != "tests.rs" {
                out.push(path);
            }
        }
    }

    fn repo_root() -> PathBuf {
        let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        while !dir.join(".git").exists() {
            assert!(dir.pop(), "no .git above {}", env!("CARGO_MANIFEST_DIR"));
        }
        dir
    }

    /// THE failing input: spell `"x-mesh-proof"` in any third production
    /// file and this goes red, naming the file and the line.
    #[test]
    fn only_the_minter_and_the_resolver_spell_the_mesh_proof_header() {
        let root = repo_root();
        let mut files = Vec::new();
        for tree in PROOF_SCANNED_TREES {
            production_sources(&root.join(tree), &mut files);
        }
        assert!(
            files.len() > 500,
            "the walk found only {} files — it is not scanning the trees it \
             claims to scan, which would make this gate pass vacuously",
            files.len()
        );

        let mut hits = Vec::new();
        for path in files {
            let rel = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if PROOF_HEADER_ALLOWED.iter().any(|a| rel.ends_with(a)) || rel.ends_with(file!()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (n, line) in text.lines().enumerate() {
                if line.trim_start().starts_with("#[cfg(test)]") {
                    break;
                }
                // Prose may name the header — that is how the next reader
                // learns where it comes from. Only code is scanned.
                let code = line.trim_start();
                if code.starts_with("//") || code.starts_with("*") {
                    continue;
                }
                if PROOF_WIRE_FORM.iter().any(|f| line.contains(f)) {
                    hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                }
            }
        }

        assert!(
            hits.is_empty(),
            "a production file outside {PROOF_HEADER_ALLOWED:?} spells the mesh-proof \
             header. Mint it with `commonwealth_transport::mesh_proof::mesh_proof_stamp` \
             and apply the pair it returns; read it only in the internal resolver.\n{}",
            hits.join("\n")
        );
    }
}
