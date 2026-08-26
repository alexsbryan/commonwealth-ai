// SPDX-License-Identifier: AGPL-3.0-or-later
//! Falsifier for daemon-convergence Phase 9, first rung: **nothing enters the
//! evidence pool except by acquisition.**
//!
//! # The named failing input (ARCH §18.1)
//!
//! Until 2026-08-25 the `readiness_disclosure` step in `retrieval_pipeline.rs`
//! did this, on any turn whose retrieval came up empty over a corpus it could
//! not search:
//!
//! ```ignore
//! st.chunks.push(corpus_engine::ScoredChunk {
//!     content: "(Assistant guidance — relay this in your own words …)",
//!     score: 1.0,
//!     metadata: HashMap::new(),
//!     ..
//! });
//! ```
//!
//! Model-directed prose, in the evidence pool, at the top score, with an empty
//! metadata map. Seven downstream readers treat that pool as retrieval output
//! and every one of them was told a falsehood — it was counted as a hit in
//! `source_map`, stamped for epistemic coverage, shaped by
//! `compute_evidence_shape`, ranked by `admission::admit`, projected into the
//! UI's `retrieved_chunks`, and handed to the grounding gate. The gate's two
//! defences both missed it by construction: the empty `metadata` map made the
//! RAPTOR filter read it as `Grain::Leaf` (quotable source text), and because
//! the pool was EMPTY whenever the step fired, `custody_engaged` was false, so
//! the custody refusal never ran. **A model could cite it.** That is hazard 1
//! (`quality/TOPOLOGY.md` §5) with a line number.
//!
//! # What this test pins
//!
//! Every `ScoredChunk` built in the retrieval pipeline is built at an
//! ACQUISITION door — a step that is turning somebody else's search results
//! into pool entries. The allow-list below is those doors, by name. A new
//! injector fails this test at the moment it is written, which is the point:
//! `step_scope_audit` already DETECTS foreign corpora in the final pool, and
//! Phase 9's bar is construction, not detection.
//!
//! # Scope, stated rather than implied
//!
//! This covers `retrieval_pipeline.rs`, which is where `PipelineState` and its
//! `chunks` field live and where every step that writes the pool is defined.
//! It is NOT the whole of rung `nc-4-evidence`: 47 `ScoredChunk` literals
//! remain across `sovereign-core` and `corpus-engine`, and the rung is not
//! done until the pool's element type is `corpus_engine::Evidence` — which
//! cannot be minted outside the engine at all. This is the bounded first rung.

use std::path::PathBuf;

/// Steps that turn a search result into pool entries. Each one is handed rows
/// by something that actually looked: the mesh fan-out, and the state store.
const ACQUISITION_DOORS: &[&str] = &[
    // Local + peer corpus search. Peer hits arrive as OICP rows and are
    // re-shaped here; the corpus is the one the peer served.
    "step_main_retrieval_mesh",
    // Estate documents out of the `StateStore`, stamped `Custody::Personal`
    // at acquisition by code (never by a model, ARCH §7.6).
    "step_store_search",
];

fn pipeline_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/runtime/retrieval_pipeline.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The production half of the file, with comments stripped.
///
/// Both halves of that matter, and both were learned the hard way. The
/// `#[cfg(test)]` module builds `ScoredChunk`s in a `fn scored(..)` helper and
/// those are fixtures, not injectors. And comments are stripped because a
/// sabotage of `daemon_variant_census.rs` on 2026-08-25 passed for exactly one
/// reason: the file's own explanatory comment contained the literal the check
/// was looking for, so prose about an invariant satisfied the check for it.
fn production_lines(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, raw) in src.lines().enumerate() {
        // `mod tests` is the last thing in the file; everything after the
        // gate is fixtures.
        if raw.trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        let code = match raw.find("//") {
            Some(idx) => &raw[..idx],
            None => raw,
        };
        out.push((i + 1, code.to_string()));
    }
    out
}

/// Walk the production lines, tracking the most recent `fn` header, and
/// return `(line, enclosing_fn)` for every `ScoredChunk` construction.
fn constructions(src: &str) -> Vec<(usize, String)> {
    let mut current = String::from("<file scope>");
    let mut hits = Vec::new();
    for (line, code) in production_lines(src) {
        let t = code.trim_start();
        if let Some(rest) = t
            .strip_prefix("pub(crate) fn ")
            .or_else(|| t.strip_prefix("pub async fn "))
            .or_else(|| t.strip_prefix("pub fn "))
            .or_else(|| t.strip_prefix("async fn "))
            .or_else(|| t.strip_prefix("fn "))
        {
            current = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
        }
        if code.contains("ScoredChunk {") {
            hits.push((line, current.clone()));
        }
    }
    hits
}

/// Instrument check (ARCH §18.4, §7): a scanner that finds nothing proves
/// nothing. Before trusting the bar below, confirm this scanner sees the
/// acquisition doors that DO build chunks — if a refactor renames or moves
/// them, this fails first and says so, rather than the bar silently passing
/// over an empty set.
#[test]
fn the_scanner_finds_the_acquisition_doors_that_do_build_chunks() {
    let src = pipeline_source();
    let found = constructions(&src);
    assert!(
        found.len() >= 2,
        "scanner found {} ScoredChunk constructions in the production pipeline; \
         expected at least the two acquisition doors ({ACQUISITION_DOORS:?}). \
         The scanner is broken or the file moved — fix this before reading the bar.",
        found.len()
    );
    for door in ACQUISITION_DOORS {
        assert!(
            found.iter().any(|(_, f)| f == door),
            "acquisition door `{door}` builds no ScoredChunk — either it was \
             renamed (update ACQUISITION_DOORS) or the scanner's fn tracking broke"
        );
    }
}

/// THE BAR. Every chunk that enters the pool was acquired; none was authored.
#[test]
fn nothing_enters_the_evidence_pool_except_by_acquisition() {
    let src = pipeline_source();
    let offenders: Vec<String> = constructions(&src)
        .into_iter()
        .filter(|(_, f)| !ACQUISITION_DOORS.contains(&f.as_str()))
        .map(|(line, f)| format!("retrieval_pipeline.rs:{line} in `{f}`"))
        .collect();
    assert!(
        offenders.is_empty(),
        "{} site(s) build a ScoredChunk outside an acquisition door:\n  {}\n\n\
         A ScoredChunk is a RETRIEVAL HIT. Building one anywhere else puts \
         content the pool's readers will treat as evidence — citable, scored, \
         counted — into the pool without anything having searched for it. That \
         is hazard 1 (quality/TOPOLOGY.md §5).\n\n\
         If you need to steer the model, that is GUIDANCE, not evidence: it \
         belongs in the prompt. `runtime::unavailability` is the worked \
         example — one typed signal, two renderings (a prompt-side \
         instruction and a code-appended answer marker), and nothing citable \
         minted. If you genuinely added an acquisition door, add it to \
         ACQUISITION_DOORS and say in the commit what searched.",
        offenders.len(),
        offenders.join("\n  ")
    );
}
