// SPDX-License-Identifier: AGPL-3.0-or-later
//! sv-surface rung 2's family census: model RESIDENCY has one wire decider
//! — the daemon's `GET /v1/models`, the OpenAI-compatible listing every
//! client routes by. The CLI's private `/status` scrape (a permissive
//! `inference.resident` key scavenger) was the twin; it is deleted, and
//! this census pins it cannot come back.
//!
//! # The state this makes unrepresentable
//!
//! A second parser of "which models is the daemon serving" in a surface.
//! Before rung 2 the CLI scavenged `/status` while the desktop parsed
//! `/v1/models` — two parsers that could disagree about the same daemon,
//! and the scavenger's permissiveness (any of `path`/`model`/`id`/`file`
//! under any nesting) meant it would silently "parse" an unrelated schema
//! change. `parse_model_ids` in `model_cmd.rs` is the one parse; this
//! census pins the HOME.
//!
//! Watched to fail: re-introduce a `/status` residency read in
//! `model_cmd.rs` and this goes red naming the rule.

use std::path::Path;

fn model_cmd_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/model_cmd.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn model_residency_reads_the_wire_decider_not_the_status_scrape() {
    let src = model_cmd_source();
    // Three spellings of the same retreat: the retired const, a bare path
    // literal, and a full URL ending in /status. The list is the lesson of
    // this census's own sabotage — the first draft forbade only the const
    // and the bare path, and a planted `http://…:9741/status` const sailed
    // through green (§18.1: a check needs the failing input it names).
    for needle in ["DAEMON_STATUS_URL", "\"/status\"", ":9741/status"] {
        assert!(
            !src.contains(needle),
            "sv-surface rung 2: a `/status` residency read is back in \
             model_cmd.rs (matched `{needle}`). The one decider for \"which \
             models is the daemon serving\" is GET /v1/models — the listing \
             every client routes by. A private scrape is the twin the \
             campaign deleted."
        );
    }
    assert!(
        src.contains("/v1/models"),
        "the residency read must name the wire decider it reads"
    );
}
