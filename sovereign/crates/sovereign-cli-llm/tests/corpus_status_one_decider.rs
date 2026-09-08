// SPDX-License-Identifier: AGPL-3.0-or-later
//! sv-surface rung 1's family census: the corpus-status decider has ONE
//! home (`corpus_engine::engine::status`), and the CLI's `corpus_cmd/
//! status.rs` is its PRINTING consumer — the private walk the rung deleted
//! cannot quietly come back.
//!
//! # The state this makes unrepresentable
//!
//! A second implementation of "what corpora exist and what state are they
//! in" in the CLI. Before rung 1 this file carried its own directory walk
//! (`scan_corpus_rows`), its own readiness decider (`corpus_readiness`)
//! and its own row type, while the desktop walked the same directories
//! through `installed_indexes` with different rules — the §10.6 twin. The
//! daemon's `GET /internal/corpus/status` (sovereign-mesh `reading_http`)
//! and this surface now print from the same function; the parity test
//! lives beside the route (`loopback_parity::corpus_status_route_serves_
//! the_one_deciders_rows`).
//!
//! The needle list's known weakness is recorded rather than hidden (the
//! cw-lift 2c finding): it sees these spellings, not a new name a
//! re-derivation might use. The corpus-engine module's own tests pin the
//! RULES; this census pins the HOME.
//!
//! Watched to fail: re-add any of the needles to `corpus_cmd/status.rs`
//! and this goes red naming the rule. Sabotage-verified at landing (the
//! census's first draft was self-referential and failed on its own needles
//! — which demonstrated the instrument fires before the move to this file).

use std::path::Path;

fn status_module_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/corpus_cmd/status.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn the_status_decider_is_not_re_derived_in_the_cli() {
    let src = status_module_source();
    for needle in [
        "fn scan_corpus_rows(",
        "fn corpus_readiness(",
        "struct CorpusStatusRow",
        "enum CorpusReadiness",
    ] {
        assert_eq!(
            src.match_indices(needle).count(),
            0,
            "sv-surface rung 1: `{needle}` is defined in the CLI's printing \
             half again. The one decider lives in corpus_engine::engine::status \
             — the daemon's /internal/corpus/status route and this surface both \
             consume it. A private re-derivation is the twin the campaign \
             deleted, wearing a new name."
        );
    }
    assert!(
        src.contains("use corpus_engine::engine::status::"),
        "the printing half must import the decider it prints from"
    );
}
