// SPDX-License-Identifier: AGPL-3.0-or-later
//! Custody reds — the failing tests that ARE the specification for the
//! custody design note (`research/deep-research/notes/custody.md`, order
//! `deep-research-t0b`, reds R-2/R-3/R-4).
//!
//! Each test compiles at HEAD, fails when run (`--ignored`), and fails for
//! the attributed reason: the custody surface it asserts on does not exist.
//! When T1 lands the custody schema, these `#[ignore]`s come off and the
//! assertions bind. Fixture trajectories are documented per test so T1
//! knows exactly which surface to expose.
//!
//! Assertion surface note: `EvidenceContext` is `pub(crate)` and the
//! custody enum is design-only (no code), so the reds assert on the
//! RELEASED metadata — `grounding_gate` meta and `retrieved_chunks` — the
//! surfaces a reader of the answer actually sees. At HEAD the
//! `retrieved_chunks[].url` field is always `null` and no custody field
//! exists anywhere in the release.

mod harness;

use harness::TestHarness;

/// Canonical custody classes (design note §1 — the enum is design-only at
/// this commit; these constants are the reds' spelling of it).
const CUSTODY_PUBLIC_WEB: &str = "public-web";
const CUSTODY_PERSONAL: &str = "personal";
const CUSTODY_PEER: &str = "peer";
const CUSTODY_CLASSES: [&str; 3] = [CUSTODY_PUBLIC_WEB, CUSTODY_PERSONAL, CUSTODY_PEER];

// ---------------------------------------------------------------------------
// R-2 — a web-fetched chunk carries no custody/URL through the gate
// ---------------------------------------------------------------------------
//
// The defect: the fetcher stamps nothing (no custody, no source URL), so a
// fetched chunk arrives at the gate with `url: null` and no custody class,
// and the released record shows it. Fixture: a corpus turn — the harness's
// fetched-chunk stand-in. The stamp site is the same code path for web
// fetches and the estate ingester (custody.md §2 "stamp sites"); T1 adds
// the web-fetch leg to this fixture the same way the hand-run recorded
// DDG.

#[tokio::test]
#[ignore = "RED R-2: web-fetched chunks carry no custody/URL through the gate (research/deep-research/notes/custody.md §2)"]
async fn web_chunk_carries_custody_and_source_url_through_the_gate() {
    let h = TestHarness::new();
    h.ingest_test_corpus(
        "custody-web",
        vec![(
            "web-1",
            "The Meridian Bridge across the Selune river was completed in 1873 \
             by the engineer Helena Voss. Its central span is 240 meters.",
        )],
    )
    .await;
    let resp = h
        .send("What can you tell me about the Meridian Bridge?")
        .await;
    let meta = resp
        .message
        .metadata
        .as_ref()
        .expect("assistant message must carry metadata");
    let gate = meta
        .get("grounding_gate")
        .expect("a corpus turn must run the gate");

    let chunks = meta
        .get("retrieved_chunks")
        .and_then(|v| v.as_array())
        .expect("a corpus turn must release its retrieved chunks");
    assert!(!chunks.is_empty(), "fixture must retrieve evidence");

    for chunk in chunks {
        // RED at HEAD: the released chunk record's `url` is always null —
        // the fetch path never stamps a source URL, so nothing reaches the
        // gate's evidence or the released record.
        let url = chunk
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            !url.is_empty(),
            "RED R-2: released chunk carries no source URL (fetcher stamps nothing; url is null at HEAD): {chunk}"
        );
        // RED at HEAD: no custody class exists on the released record.
        let custody = chunk
            .get("custody")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            CUSTODY_CLASSES.contains(&custody),
            "RED R-2: released chunk carries no custody class (must be one of {CUSTODY_CLASSES:?}; absent at HEAD): {chunk}"
        );
    }

    // The gate's own meta must carry the same per-chunk custody ledger —
    // the evidence the judge SAW, not just the release record.
    let chunk_custody = gate.get("chunk_custody").and_then(|v| v.as_array());
    assert!(
        chunk_custody.is_some_and(|c| !c.is_empty()),
        "RED R-2: gate meta carries no per-chunk custody ledger: {gate}"
    );
}

// ---------------------------------------------------------------------------
// R-3 — an unstamped chunk can ground a factual claim (must refuse instead)
// ---------------------------------------------------------------------------
//
// The defect: `EvidenceContext::source_of` defaults unknown provenance to
// Leaf (grounding/mod.rs:248-253), so an unstamped chunk grounds a factual
// claim like any other. The contract: a chunk whose provenance is unknown
// must force a refusal — the gate may not release a factual claim resting
// on it.
//
// Fixture trajectory: at HEAD no chunk anywhere carries a provenance stamp
// (the stamp machinery does not exist), so the harness corpus turn IS the
// unstamped shape. At green the estate ingester stamps its chunks, so T1
// must point this fixture at the still-unstamped path (sealed/pinned
// evidence — chunks appended after the evidence builder ran, which have no
// source row; custody.md §4). The refusal assertion below is what binds
// there.

#[tokio::test]
#[ignore = "RED R-3: an unstamped derived chunk can ground a factual claim — unknown provenance must refuse (custody.md §4)"]
async fn unknown_provenance_cannot_ground_a_factual_claim() {
    let h = TestHarness::new();
    h.ingest_test_corpus(
        "custody-unstamped",
        vec![(
            "unstamped-1",
            "The Meridian Bridge across the Selune river was completed in 1873 \
             by the engineer Helena Voss. Its central span is 240 meters.",
        )],
    )
    .await;
    let resp = h
        .send("What can you tell me about the Meridian Bridge?")
        .await;
    let meta = resp
        .message
        .metadata
        .as_ref()
        .expect("assistant message must carry metadata");
    let gate = meta
        .get("grounding_gate")
        .expect("a corpus turn must run the gate");

    // RED at HEAD: the gate carries no provenance classification for the
    // chunks it judged — it is provenance-blind, which is why unknown
    // provenance silently grounds (mod.rs:248-253 defaults it to Leaf).
    let chunk_custody = gate.get("chunk_custody").and_then(|v| v.as_array()).expect(
        "RED R-3: gate meta must carry a per-chunk provenance classification; \
             absent at HEAD — the gate judged provenance-blind and could not refuse",
    );
    assert!(
        !chunk_custody.is_empty(),
        "RED R-3: classification must cover the judged chunks"
    );

    // The refusal contract: unknown-provenance evidence forces a refusal.
    // T1 fixture trajectory: point this at the unstamped (sealed/pinned)
    // evidence path once the ingester stamps corpus chunks (custody.md §4).
    let has_unknown = chunk_custody
        .iter()
        .any(|c| c.get("provenance_class").and_then(|v| v.as_str()) == Some("unknown"));
    if has_unknown {
        let action = gate
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            action.starts_with("abstained") || action.starts_with("refused"),
            "RED R-3: unknown-provenance evidence must refuse, but the gate released ({action}): {gate}"
        );
    }
}

// ---------------------------------------------------------------------------
// R-4 — a mixed-custody tier-2 summary has no custody to key on
// ---------------------------------------------------------------------------
//
// The defect: a tier-2 summary derived from mixed-custody inputs (e.g. a
// public-web passage + a personal file) carries no derived custody — there
// is nothing for an egress check to key on, so the summary can flow to a
// destination its inputs forbid. The contract: derived custody = the
// max-restrictiveness join over the derivation inputs, computed at
// creation, riding the released evidence record.
//
// Fixture mechanism per the order: through a STUB egress check. The join
// rule and the stub below are the specification in code — deterministic,
// no model. "Fails by construction": the stub can only key on the
// production-released custody, and at HEAD no such value exists anywhere,
// so the red's production-surface assertion is guaranteed to fail.

/// The join rule (custody.md §3): max-restrictiveness — personal >
/// peer > public-web. Deterministic, no model.
fn derive_custody(inputs: &[&str]) -> String {
    if inputs.contains(&CUSTODY_PERSONAL) {
        CUSTODY_PERSONAL.to_string()
    } else if inputs.contains(&CUSTODY_PEER) {
        CUSTODY_PEER.to_string()
    } else {
        CUSTODY_PUBLIC_WEB.to_string()
    }
}

/// The stub egress check (custody.md §5): a summary may leave the estate
/// only when its derived custody is public-web. The real egress surfaces
/// (clipboard, export, mesh share) key on exactly this.
fn egress_refuses(derived_custody: &str) -> bool {
    derived_custody != CUSTODY_PUBLIC_WEB
}

#[tokio::test]
#[ignore = "RED R-4: a mixed-custody tier-2 summary has no custody to key on — derived custody must ride the release (custody.md §3)"]
async fn mixed_custody_summary_carries_derived_custody() {
    // Fixture: a tier-2 summary derived from mixed-custody inputs — one
    // public-web passage, one personal file. The derivation join runs at
    // creation; the result must ride the summary's released record.
    let inputs = [CUSTODY_PUBLIC_WEB, CUSTODY_PERSONAL];
    let derived = derive_custody(&inputs);
    assert_eq!(
        derived, CUSTODY_PERSONAL,
        "the join rule must be max-restrictive (personal wins over public-web)"
    );
    assert!(
        egress_refuses(&derived),
        "the stub egress check must refuse a mixed-custody summary"
    );

    // The production half: the released evidence record must carry the
    // derived custody, so a real egress check can key on it.
    let h = TestHarness::new();
    h.ingest_test_corpus(
        "custody-mixed",
        vec![(
            "mixed-1",
            "The Meridian Bridge across the Selune river was completed in 1873 \
             by the engineer Helena Voss. Its central span is 240 meters.",
        )],
    )
    .await;
    let resp = h
        .send("What can you tell me about the Meridian Bridge?")
        .await;
    let meta = resp
        .message
        .metadata
        .as_ref()
        .expect("assistant message must carry metadata");
    let chunks = meta
        .get("retrieved_chunks")
        .and_then(|v| v.as_array())
        .expect("a corpus turn must release its retrieved chunks");

    // RED at HEAD, fails by construction: no evidence record carries a
    // custody value — the stub has nothing to key on, so the summary's
    // mixed custody is invisible to every egress surface.
    let any_custody = chunks.iter().any(|c| {
        c.get("custody")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
    });
    assert!(
        any_custody,
        "RED R-4: released evidence carries no custody to key on — a mixed-custody \
         summary's derived custody is invisible to the stub egress check (fails by \
         construction: the field does not exist at HEAD)"
    );
}
