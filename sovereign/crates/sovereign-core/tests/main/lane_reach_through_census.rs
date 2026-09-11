// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon-convergence Phase 4a falsifier: **no pipeline stage reaches
//! through `&self` into a `Runtime` enrichment field.**
//!
//! `quality/TOPOLOGY.md` §3.5 states the bar as a count and not a shape, and
//! says why: grouping the fields into a sub-struct changes `self.gliner` into
//! `self.enrichment.gliner`, which is the same coupling down a longer path.
//! Only the count moving to zero proves a stage receives its providers as a
//! value — and that is what lets the `Runtime` collapse to core, because
//! nothing can make it fat by wanting one more provider on it.
//!
//! Baseline was 35 (measured 2026-08-24, over `runtime/retrieval/*`,
//! `evidence_loop`, `streaming.rs` and `turn.rs`).
//!
//! ## Named failing input (ARCH §18.1)
//!
//! Replace `lane.rerank.f()` with `self.rerank_fn.as_ref()` anywhere under
//! `src/runtime/` and this test fails, naming the file and line. That is a
//! real edit a real refactor makes by reflex, which is the whole reason the
//! bar is machine-checked rather than written down.

use super::runtime_source_scan::{runtime_dir, scan};

/// The seven providers §3.5 groups as `Lane`. Reading one of these off
/// `self` inside a stage is the defect; reading it off a `&Lane` parameter,
/// or off `st.lane`, is the fix.
const ENRICHMENT_FIELDS: &[&str] = &[
    "atlas_context_provider",
    "wikipedia_graph",
    "meta_atlas",
    "bridge",
    "rerank_fn",
    "rerank_config",
    "gliner",
    "conv_tiered_reader",
];

/// The ONE file allowed to name them: `Runtime::lane` is where the fields are
/// resolved into the value everything else receives.
const RESOLVER: &str = "lane.rs";

/// ARCH §18.4 — validate the instrument before the result. A scanner that
/// silently read nothing reports zero for the wrong reason, and zero is
/// exactly the answer this file exists to publish. So first prove the scan
/// finds something it MUST find: `self.inference` and `self.store` are CORE,
/// they are read all over the runtime, and Phase 4a does not touch them.
#[test]
fn the_scanner_finds_core_reach_throughs_it_is_not_looking_for() {
    let control = scan(&["inference", "store"], &[]);
    assert!(
        control.len() > 10,
        "instrument check failed: the scan found only {} `self.inference` / \
         `self.store` reads under src/runtime/, which cannot be right — the \
         scanner is not reading the sources, so its zero below would be \
         meaningless",
        control.len()
    );
}

/// The bar itself: 35 → 0.
#[test]
fn no_stage_reaches_through_self_into_an_enrichment_field() {
    let hits = scan(ENRICHMENT_FIELDS, &[RESOLVER]);
    assert!(
        hits.is_empty(),
        "daemon-convergence Phase 4a: {} enrichment reach-through(s) are back. \
         A stage must receive its providers as a `Lane` value — take \
         `lane: &Lane` (or read `st.lane` inside the pipeline) instead of \
         reaching into the Runtime:\n{}",
        hits.len(),
        hits.iter()
            .map(|(f, l, t)| format!("  {f}:{l}  {t}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The resolver is the exception, and it must actually BE one — if
/// `LaneSources::snapshot` stopped copying a member, every lane would carry
/// `None` for it and every stage would silently take its baseline path, which
/// no other test here would notice. A capability reported as available and
/// never applied is the exact defect Phase 4b exists to kill; it must not
/// come back through the resolver's own back door.
#[test]
fn the_resolver_carries_every_lane_member() {
    let text = std::fs::read_to_string(runtime_dir().join(RESOLVER)).expect("lane.rs");
    for member in [
        "atlas_context",
        "wikipedia_graph",
        "meta_atlas",
        "bridge",
        "rerank",
        "gliner",
        "conv_tiered",
    ] {
        assert!(
            text.contains(&format!("self.{member}")),
            "LaneSources::snapshot no longer carries `{member}` — every stage \
             now sees it absent, and the reach-through census would still pass"
        );
    }
}
