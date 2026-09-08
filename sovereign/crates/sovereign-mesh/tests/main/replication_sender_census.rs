// SPDX-License-Identifier: AGPL-3.0-or-later
//! **How many senders of replicated state does this workspace have?**
//!
//! `cw-twin-visibility`'s instrument, and until this file existed the only way
//! to answer it was a hand-run `git grep` that nobody ran. The answer is ONE
//! and this is what keeps it one.
//!
//! Deterministic, no model: every workspace member's `src/` tree, test modules
//! excluded, counting the URL-join form `{…}/internal/<route>` that only a
//! sender writes. A route STRING (the receiver's `.route(…)`) and a doc
//! mention both lack the brace, so they do not count — which is the
//! distinction the census exists to make.
//!
//! This file used to be `gossip_push_surfacing.rs` and its other half drove a
//! real gossip round against a real peer that answered `/internal/app/state`
//! with 413, asserting the refusal reached an operator at WARN. That push, its
//! route and its receiver were deleted at cw-lift rung 2e; the tests went with
//! them because their subject did, not by association. The surfacing question
//! they answered is the ring's now, and `ring_sync`'s `RoundOutcome` carries
//! `peers_refused` for exactly it.

/// Every production site that puts replicated state on the wire, by route
/// and by file.
///
/// **ONE, and it took four rungs to get there.** The count was 4 when this
/// table was written, 3 after rung 2c deleted `corpus_collaborate.rs`'s
/// hand-rolled fourth POST of the identical wire shape, and 1 at rung 2e,
/// which deleted the other two: gossip's 10 s full-snapshot push to every
/// online peer, and `broadcast_now`'s event-driven single-entry POST for the
/// work atlas. Both wrote `/internal/app/state`, whose receiver went with
/// them. `MeshStore` is a projection of the ring journals now, so the state
/// those two carried has somewhere else to be.
///
/// A new row here is a review moment, never a silent pass. Adding a sender
/// is allowed; adding one without saying so in this table is not.
const REPLICATION_SENDERS: &[(&str, &str, usize)] = &[
    // The ring journal's own digest exchange — its own route on its own
    // 60 s cadence, budgeted in both directions, chunked so one body is not
    // the unit of convergence.
    (
        "/internal/ring/sync",
        "sovereign/crates/sovereign-mesh/src/ring_sync.rs",
        1,
    ),
];

fn workspace_root() -> std::path::PathBuf {
    let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let toml = dir.join("Cargo.toml");
        if toml.is_file()
            && std::fs::read_to_string(&toml)
                .map(|t| t.contains("[workspace]"))
                .unwrap_or(false)
        {
            return dir;
        }
        assert!(dir.pop(), "workspace root not found");
    }
}

fn workspace_members(root: &std::path::Path) -> Vec<String> {
    let text = std::fs::read_to_string(root.join("Cargo.toml")).expect("root Cargo.toml");
    let start = text.find("members = [").expect("no members list");
    let end = text[start..].find(']').expect("unterminated members") + start;
    text[start + "members = [".len()..end]
        .lines()
        .map(|l| l.trim().trim_matches(',').trim_matches('"'))
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

fn walk_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_rs(&p, out);
        } else if p.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(p);
        }
    }
}

/// The census, as `(route, repo-relative file, sites)`.
///
/// Everything from the first `#[cfg(test)]` is dropped: a test that spins a
/// fake peer builds the same URL and is not a production sender. `ring_sync.rs`
/// puts its test module last, which is this workspace's convention and what
/// makes that truncation safe.
fn scan_senders() -> Vec<(String, String, usize)> {
    let root = workspace_root();
    let mut files = Vec::new();
    for member in workspace_members(&root) {
        walk_rs(&root.join(&member).join("src"), &mut files);
    }
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let production = match text.find("#[cfg(test)]") {
            Some(i) => &text[..i],
            None => &text[..],
        };
        for (route, _, _) in REPLICATION_SENDERS {
            let needle = format!("}}{route}");
            let n = production.matches(needle.as_str()).count();
            if n > 0 {
                let rel = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(((*route).to_string(), rel, n));
            }
        }
    }
    out.sort();
    out
}

/// RED-FIRST, twice (ARCH §18.1). At cw-lift 2c the scan returned a THIRD row
/// — `commonwealth-api/src/routes_internal/corpus_collaborate.rs` on
/// `/internal/app/state` — and this assertion failed printing it.
///
/// **A one-row table is the weaker instrument, and the sabotage has to change
/// with it.** The scan looks only for the routes NAMED here, so deleting a
/// sender AND its row can never turn this red: the second half of 2e's
/// deletion is proved by the compiler and by `MeshBroadcaster`'s own tests,
/// not by this. What this still catches, and the only thing it claims to, is a
/// SECOND site on the surviving route. Watched red at 2e by adding
/// `let _ = format!("{}/internal/ring/sync", "x");` to
/// `sovereign-mesh/src/join.rs`: the census returned two rows and the diff
/// named the file.
#[test]
fn every_sender_of_replicated_state_is_declared() {
    let mut expected: Vec<(String, String, usize)> = REPLICATION_SENDERS
        .iter()
        .map(|(route, file, n)| ((*route).to_string(), (*file).to_string(), *n))
        .collect();
    expected.sort();
    assert_eq!(
        scan_senders(),
        expected,
        "the count of senders of replicated state is this campaign's instrument \
         (cw-twin-visibility). A row that appears here is a second answer to \
         \"how does state reach a peer\" (ARCH §10.6) and must be argued, not \
         discovered later by grep"
    );
}
