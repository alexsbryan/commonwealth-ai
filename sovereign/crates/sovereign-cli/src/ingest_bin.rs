// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exec dispatch into ingest's own binary, `svrn-ingest` (phase-b
//! pb-ingest-cli), for `svrn ingest <recipe.toml>` and, since
//! pb-cli-llm-ingest-move, ingest's CLI verbs ([`owns`]). Same shape as
//! `serve_bin::exec`; the verb is passed as the sibling's first argument, as
//! `llm_bin::exec` does, so `svrn-ingest` routes it with its own subcommands.

use std::path::PathBuf;

const BIN_NAME: &str = "svrn-ingest";

/// ingest's CLI verbs, moved from sovereign-cli-llm (pb-cli-llm-ingest-move).
const INGEST_VERBS: &[&str] = &[
    "enrich",
    "corpus",
    "atlas",
    "meta-atlas",
    "recipe",
    "pipeline",
    "alignment",
];

/// `(verb, first argument)` spellings under ingest's verbs that are svrn's
/// (they open svrn's store, call svrn's routes or use svrn's tools,
/// phase-b-70 (2)), run by sovereign-cli-llm rather than `svrn-ingest`.
const SVRN_SIDE: &[(&str, &str)] = &[
    ("enrich", "raptor"),
    ("enrich", "raptor-index"),
    ("enrich", "summary-atoms"),
    ("atlas", "budget"),
    ("atlas", "status"),
    ("atlas", "list-corpora"),
    ("atlas", "list-atoms"),
    ("atlas", "show-atom"),
    ("atlas", "typed-extension"),
    ("corpus", "ingest"),
    ("corpus", "share"),
    ("corpus", "pull"),
    ("corpus", "catalog"),
    ("corpus", "extract-entities"),
    ("corpus", "watch"),
    ("corpus", "watch-list"),
    ("corpus", "watch-status"),
    ("corpus", "watch-pause"),
    ("corpus", "watch-resume"),
    ("corpus", "watch-confirm-deletion"),
    ("corpus", "watch-sync-now"),
    ("corpus", "watch-add-root"),
    ("corpus", "watch-remove-root"),
    ("corpus", "watch-remove"),
];

/// Whether `svrn-ingest` runs `svrn <verb> <args…>`: ingest's verbs except
/// svrn's sub-verbs under them, and `bench atlas` (ingest's white-box lane,
/// b024722fa).
pub fn owns(verb: &str, args: &[String]) -> bool {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let owns = if verb == "bench" {
        sub == "atlas"
    } else {
        INGEST_VERBS.contains(&verb) && !SVRN_SIDE.contains(&(verb, sub))
    };
    tracing::debug!(verb, sub, owns, "ingest or svrn owns the verb");
    owns
}

fn locate() -> Option<PathBuf> {
    sovereign_turn_client::reach::locate_sibling(BIN_NAME, "SOVEREIGN_INGEST_BIN")
}

pub fn exec(verb: &str, args: &[String]) -> i32 {
    let Some(bin) = locate() else {
        eprintln!(
            "svrn {verb}: owned by ingest, whose binary '{BIN_NAME}' was not \
             found. Build it with `cargo build -p sovereign-pipeline`, or set \
             SOVEREIGN_INGEST_BIN to its path."
        );
        return 127;
    };

    crate::sibling::warn_if_stale(&bin, "sovereign-pipeline");

    crate::sibling::exec_into(&bin, verb, args)
}

#[cfg(test)]
mod tests {
    use super::owns;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    /// ingest's verbs go to `svrn-ingest`, their bare `--help` included;
    /// svrn's sub-verbs under the same spellings and every other verb do not.
    #[test]
    fn ingest_owns_its_verbs_and_not_svrns_sub_verbs() {
        for (verb, sub) in [
            ("enrich", "build"),
            ("enrich", "--help"),
            ("corpus", "list"),
            ("corpus", "install"),
            ("atlas", "backfill-ann"),
            ("meta-atlas", "build"),
            ("recipe", "test"),
            ("pipeline", "run"),
            ("alignment", "report"),
            ("bench", "atlas"),
        ] {
            assert!(owns(verb, &args(&[sub])), "{verb} {sub}");
        }
        assert!(owns("enrich", &args(&[])));
        for (verb, sub) in [
            ("enrich", "raptor"),
            ("enrich", "summary-atoms"),
            ("atlas", "status"),
            ("atlas", "typed-extension"),
            ("corpus", "pull"),
            ("corpus", "ingest"),
            ("corpus", "watch-list"),
            ("bench", "all"),
            ("chat", ""),
            ("workflow", "run"),
        ] {
            assert!(!owns(verb, &args(&[sub])), "{verb} {sub}");
        }
    }
}
