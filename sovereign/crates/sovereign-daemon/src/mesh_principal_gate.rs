// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ratchet behind "no decider reads the header" — a test, not a comment.
//!
//! `x-node-id` is what a peer TYPES about itself. Until 2026-09-20 nine
//! production sites under `sovereign/crates` read it and decided something on
//! it: ledger attribution, per-peer manifest affinity, the scheduler's
//! reciprocity key, the peer tally, and the three literal reads that routed a
//! request to the peer gate at all. Every one of them would have believed
//! `x-node-id: <somebody-else>` typed by any caller that could reach the port.
//!
//! They now read the [`Principal`](sovereign_contracts::principal::Principal)
//! a resolver attached, and the header itself is read in exactly ONE file:
//! [`sovereign_contracts::principal::claimed_node_id`], which produces a
//! *claim*, never an identity.
//!
//! ## Why a grep and not a lint
//!
//! ARCH principle 10: make it structural, not remembered. A comment saying
//! "don't read the header" is exactly what `admission.rs:306` used to say
//! ("the verified node id") while the code below it did the opposite. This
//! module fails the normal test run instead, and it greps for the LITERAL
//! header as well as the parser's name, so moving the read behind a fresh
//! helper does not satisfy it (bar `mp-no-decider-reads-the-header`'s named
//! goodhart).
//!
//! ## Three lists, because there are three different things to say
//!
//! [`READ_ALLOWED`] is the one file that may hold the literal header or the
//! parser: `sovereign-contracts`'s `principal`, where the wire form lives with
//! the key it produces. Two crates resolve requests to a principal —
//! `sovereign-daemon` (the client and internal surfaces) and `sovereign-server`
//! (its own auth layer) — so the form cannot live privately in either without
//! the two drifting.
//!
//! [`RESOLVERS`] are those two files, and they may CALL [`claimed_node_id`]
//! without holding the wire form. A resolver is not a decider: it turns a
//! claim into a `Principal` and decides nothing else. Nothing else may call it,
//! which is what stops a decider reaching the header behind a helper the grep
//! does not name (the bar's own goodhart).
//!
//! [`SENDERS`] STAMP the header on outbound requests and are unchanged by this
//! work. They stay for one release: a receiver on an older build routes on the
//! header's PRESENCE, so a sender that stopped would have its turns admitted
//! as that node's own local traffic with pause, foreground yield and
//! `max_peer_inflight` all dark — the 2026-08-06 failure recorded at
//! `sovereign-serving-host/src/peer_inference.rs`. Stamping is not deciding.
//!
//! Test code is exempt twice over: `tests/` trees are not walked, and inside a
//! production file everything from the first `#[cfg(test)]` on is skipped. A
//! test that types a forged header is precisely how the refusal is proved.

/// The one production path that may hold the literal header or the parser.
/// Repo-relative, matched as a suffix so the test runs from any cwd.
pub const READ_ALLOWED: &[&str] = &["sovereign/crates/sovereign-contracts/src/principal.rs"];

/// The two request-to-principal resolvers, which may call
/// [`claimed_node_id`](sovereign_contracts::principal::claimed_node_id) and
/// nothing more. One per HTTP surface that has one.
pub const RESOLVERS: &[&str] = &[
    "sovereign/crates/sovereign-daemon/src/client_principal.rs",
    "sovereign/crates/sovereign-server/src/auth.rs",
];

/// The four sites that STAMP the header outbound. Unchanged by this work and
/// kept for one release — see the module docs for the failure a silent stop
/// reproduces.
pub const SENDERS: &[&str] = &[
    "sovereign/crates/sovereign-grants/src/shard_manager.rs",
    "sovereign/crates/sovereign-daemon/src/routes_knowledge.rs",
    "sovereign/crates/sovereign-daemon/src/server.rs",
    "sovereign/crates/sovereign-serving-host/src/peer_inference.rs",
];

/// The wire form itself, in both spellings, plus the retired parser's name so
/// that reviving it under the old name does not open a second door.
pub const WIRE_FORM: &[&str] = &["x-node-id", "X-Node-Id", "parse_x_node_id"];

/// The published reader. Allowed in [`RESOLVERS`] only — naming it anywhere
/// else is a decider reaching the header behind a helper.
pub const READER: &str = "claimed_node_id";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// Walk `sovereign/crates` for `.rs` files that are not tests.
    ///
    /// "Not a test" is: not under a `tests/` directory, and not a file whose
    /// own name says so. A `#[cfg(test)]` module INSIDE a production file is
    /// deliberately still scanned — that is where a decider would hide.
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

    /// The repo root, found by walking up from this crate until `.git` appears.
    fn repo_root() -> PathBuf {
        let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        while !dir.join(".git").exists() {
            assert!(dir.pop(), "no .git above {}", env!("CARGO_MANIFEST_DIR"));
        }
        dir
    }

    /// covers: mp-no-decider-reads-the-header
    ///
    /// THE failing input: restore any production read of `x-node-id` outside
    /// the one allowed path and this goes red, naming the file and the line.
    /// The comment this replaces asserted the same thing in English and was
    /// false for as long as it stood.
    #[test]
    fn no_production_file_outside_the_one_allowed_path_reads_the_peer_header() {
        let root = repo_root();
        let mut files = Vec::new();
        production_sources(&root.join("sovereign/crates"), &mut files);
        assert!(
            files.len() > 500,
            "the walk found only {} files — it is not scanning the tree it \
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
            if READ_ALLOWED.iter().any(|a| rel.ends_with(a)) || rel.ends_with(file!()) {
                continue;
            }
            let is_resolver = RESOLVERS.iter().any(|a| rel.ends_with(a));
            let is_sender = SENDERS.iter().any(|a| rel.ends_with(a));
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (n, line) in text.lines().enumerate() {
                // Everything from the first `#[cfg(test)]` on is test code.
                if line.trim_start().starts_with("#[cfg(test)]") {
                    break;
                }
                // A doc comment or a prose comment may NAME the header — that
                // is how the next reader learns why it is gone. Only code is
                // scanned.
                let code = line.trim_start();
                if code.starts_with("//") || code.starts_with("*") {
                    continue;
                }
                if line.contains(READER) && !is_resolver {
                    hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                }
                if WIRE_FORM.iter().any(|f| line.contains(f)) && !is_sender {
                    hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                }
            }
        }

        assert!(
            hits.is_empty(),
            "a production decider reads the peer header. It must read the \
             attached `Principal` instead — the header is a CLAIM and the \
             principal is what this daemon verified. The wire form lives in \
             {READ_ALLOWED:?}; only {RESOLVERS:?} may call it.\n{}",
            hits.join("\n")
        );
    }
}
