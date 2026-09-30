// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exec dispatch into bench's own binary, `sovereign-cli-bench` (phase-b
//! pb-cli-llm-bench-move), for `svrn bench`, `svrn eval` and `svrn quality
//! lane`. Same shape as `llm_bin::exec`: the verb is passed as the sibling's
//! first argument.
//!
//! svrn's white-box lanes kept their spellings under `bench` and `eval` but
//! stayed in sovereign-cli-llm (they exercise svrn's own internals;
//! phase-b-60, -64), so [`owns`] routes those to `llm_bin` instead; `bench
//! atlas`, ingest's white-box lane, goes to `svrn-ingest` since
//! pb-cli-llm-ingest-move (`ingest_bin::owns`, matched first).

use std::path::PathBuf;

const BIN_NAME: &str = "sovereign-cli-bench";

/// `(verb, first argument)` spellings that are svrn's white-box lanes, run
/// by sovereign-cli-llm rather than bench.
const SVRN_WHITE_BOX: &[(&str, &str)] = &[
    ("bench", "judge-replay"),
    ("bench", "resolver-precision"),
    ("bench", "atlas"),
    ("eval", "inner-chaos"),
];

/// Whether bench's binary runs `svrn <verb> <args…>`: `bench` and `eval`,
/// except svrn's white-box lanes.
pub fn owns(verb: &str, args: &[String]) -> bool {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let white_box = SVRN_WHITE_BOX.contains(&(verb, sub));
    tracing::debug!(verb, sub, white_box, "bench or svrn owns the verb");
    matches!(verb, "bench" | "eval") && !white_box
}

fn locate() -> Option<PathBuf> {
    sovereign_turn_client::reach::locate_sibling(BIN_NAME, "SOVEREIGN_CLI_BENCH_BIN")
}

pub fn exec(verb: &str, args: &[String]) -> i32 {
    let Some(bin) = locate() else {
        eprintln!(
            "svrn {verb}: owned by bench, whose binary '{BIN_NAME}' was not \
             found. Build it with `cargo build -p sovereign-cli-bench`, or set \
             SOVEREIGN_CLI_BENCH_BIN to its path."
        );
        return 127;
    };

    crate::sibling::warn_if_stale(&bin, "sovereign-cli-bench");

    crate::sibling::exec_into(&bin, verb, args)
}

#[cfg(test)]
mod tests {
    use super::owns;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    /// bench's verbs go to bench; svrn's white-box lanes under the same
    /// spellings stay svrn's; no other verb is bench's.
    #[test]
    fn bench_owns_its_verbs_and_not_svrns_white_box_lanes() {
        assert!(owns("bench", &args(&["all"])));
        assert!(owns("bench", &args(&["chaos-monkey", "run"])));
        assert!(owns("eval", &args(&["run", "--bank", "b.toml"])));
        assert!(owns("bench", &args(&[])));
        for (verb, sub) in [
            ("bench", "judge-replay"),
            ("bench", "resolver-precision"),
            ("bench", "atlas"),
            ("eval", "inner-chaos"),
        ] {
            assert!(!owns(verb, &args(&[sub, "--help"])), "{verb} {sub}");
        }
        assert!(!owns("knowledge-gym", &args(&["run"])));
        assert!(!owns("chat", &args(&[])));
    }
}
