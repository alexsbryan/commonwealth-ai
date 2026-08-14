// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two-arm control's ONE-SHOT arm (order deep-research-t1c,
//! pre-registered 2026-08-14 in research/deep-research/adversarial/
//! pre-registration.md).
//!
//! The loop arm is the shipped CLI (`svrn deep-research "<question>"
//! --backend mock --mock-deck <deck>`); this test is its comparator:
//! the SAME deck, the SAME daemon, the SAME model, the SAME retrieval
//! (the deck's bodies assembled by the production `evidence_block`),
//! the SAME constrained draft surface — but the draft is asked ONCE,
//! against the full deck window, instead of being driven by the gap
//! loop. ONLY the loop differs.
//!
//! Faithfulness notes (the pre-registered "zero prompt fork"):
//! - `draft_round` is called with round = 2, open_gaps = [] — the
//!   loop's content-bearing draft shape ("Evidence gathered so far" +
//!   the question). Round 1's shape carries NO question text and NO
//!   content on the mock estate (estate_search answers nothing), so
//!   it is not a one-shot-RAG shape.
//! - The port mirrors `CliResearchPort::draft`'s inference leg exactly
//!   (Speed::Slow, temperature 0.4, url_allowlist — deep_research_cmd
//!   .rs:363-385); the CLI crate is not importable from
//!   sovereign-core's test build, so that one leg is mirrored here.
//!   Every other trait leg is `unimplemented!()` — unreachable:
//!   `draft_round` calls only `port.draft`. `terminal_poll` would need
//!   reqwest (not a sovereign-core dep); the loop arm's liveness check
//!   is not part of the one-shot surface.
//! - Window chunks mirror the loop's fetch shape exactly (fetch.rs:
//!   id `ev-N`, locator = url, provenance_class "known", custody
//!   public-web, cap_content) — via the production `cap_content`,
//!   `derive_custody`, `CHUNK_CONTENT_CAP`: no reimplemented
//!   threshold (§10.6).
//! - The draft is received through the same `MockBackendImpl` +
//!   `MockDraftSurface::Delegated` wiring the CLI uses.
//!
//! Env contract (written by `research/deep-research/arms/run-arms.sh`):
//!   DR_ARM_PAIRS — path to pairs.json: [{"id", "deck", "question"}]
//!   DR_ARM_OUT   — output dir; writes oneshot-<id>.md (the draft) and
//!                  oneshot-<id>-window.json (the EvidenceWindow).
//! Run: `cargo test --test oneshot_rag -- --ignored` (or via the
//! driver script). Requires the local daemon up (the pre-registered
//! model pin).

#![cfg(test)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use oicp_client::RemoteApiProvider;
use sovereign_core::deep_research::estate::{
    AlignmentDecision, EstateListing, PortHit, ResearchPort,
};
use sovereign_core::deep_research::fetch::{cap_content, derive_custody};
use sovereign_core::deep_research::gym::{Deck, MockBackendImpl, MockDraftSurface};
use sovereign_core::deep_research::icd::{EvidenceWindow, ICD_VERSION, Plan, WindowChunk};
use sovereign_core::deep_research::synthesize::draft_round;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};

/// The daemon-connected port: the CLI's two inference legs, mirrored.
struct DaemonPort {
    provider: Arc<dyn InferenceProvider>,
}

#[async_trait]
impl ResearchPort for DaemonPort {
    async fn estate_listing(&self, _c: &[String]) -> Result<EstateListing, String> {
        unimplemented!("unreachable: draft_round calls only draft")
    }
    async fn estate_search(
        &self,
        _c: &[String],
        _q: &str,
        _l: usize,
    ) -> Result<Vec<PortHit>, String> {
        unimplemented!("unreachable: draft_round calls only draft")
    }
    async fn web_search(
        &self,
        _b: &str,
        _q: &str,
        _l: usize,
    ) -> Result<Vec<PortHit>, String> {
        unimplemented!("unreachable: draft_round calls only draft")
    }
    async fn web_fetch(&self, _u: &str) -> Result<String, String> {
        unimplemented!("unreachable: draft_round calls only draft")
    }
    async fn terminal_poll(&self) -> Result<(), String> {
        unimplemented!("unreachable: draft_round calls only draft")
    }
    async fn draft(
        &self,
        prompt: &str,
        system_message: Option<&str>,
        allowed_urls: &[String],
    ) -> Result<String, String> {
        // Mirrors CliResearchPort::draft (deep_research_cmd.rs) — the
        // same ask the loop arm's drafts receive.
        let resp = self
            .provider
            .complete(&CompletionRequest {
                prompt: prompt.to_string(),
                system_message: system_message.map(|s| s.to_string()),
                preferred_speed: Speed::Slow,
                max_tokens: None,
                temperature: Some(0.4),
                structured_output: None,
                think_budget: None,
                url_allowlist: Some(allowed_urls.to_vec()),
                ..Default::default()
            })
            .await
            .map_err(|e| format!("draft ask: {e}"))?;
        Ok(resp.text)
    }
    async fn alignment_decision(&self, _p: &Plan, _r: &Path) -> Result<AlignmentDecision, String> {
        Ok(AlignmentDecision::Proceed)
    }
}

struct Pair {
    id: String,
    deck: String,
    question: String,
}

/// Hand-parse the pairs list (no serde-derive dev-dep needed); a
/// malformed pair refuses loudly — a silently dropped question would
/// be a never-ran reported as a pass.
fn parse_pairs(path: &str) -> Result<Vec<Pair>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{path} parse: {e}"))?;
    let arr = value
        .as_array()
        .ok_or_else(|| format!("{path}: expected a JSON array"))?;
    let mut pairs = Vec::new();
    for (i, row) in arr.iter().enumerate() {
        let get = |k: &str| -> Result<String, String> {
            row.get(k)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| format!("{path}[{i}]: missing string field `{k}`"))
        };
        pairs.push(Pair {
            id: get("id")?,
            deck: get("deck")?,
            question: get("question")?,
        });
    }
    Ok(pairs)
}

/// The deck's hits as the loop-shaped evidence window (fetch.rs shape).
fn window_from_deck(deck: &Deck, run_id: &str, round: u32) -> EvidenceWindow {
    let chunks: Vec<WindowChunk> = deck
        .hits
        .iter()
        .enumerate()
        .map(|(i, h)| WindowChunk {
            id: format!("ev-{}", i + 1),
            locator: h.url.clone(),
            source_url: h.url.clone(),
            custody: sovereign_core::types::Custody::PublicWeb.as_str().to_string(),
            provenance_class: "known".to_string(),
            content: cap_content(&deck.url_bodies[&h.url]),
            ingested_into: None,
            tags: Vec::new(),
        })
        .collect();
    let derived_custody = derive_custody(&chunks);
    EvidenceWindow {
        icd: "evidence_window".to_string(),
        version: ICD_VERSION,
        run_id: run_id.to_string(),
        charter_hash: "oneshot-arm".to_string(),
        round,
        chunks,
        fetch_failures: Vec::new(),
        derived_custody,
    }
}

async fn one_shot(pair: &Pair, out: &Path) -> Result<(), String> {
    let deck = Deck::load(Path::new(&pair.deck))
        .map_err(|e| format!("{}: deck load: {e}", pair.id))?;
    if deck.hits.is_empty() {
        return Err(format!("{}: deck has no hits — nothing to draft from", pair.id));
    }
    // The loop's window cap is 20 chunks (RunConfig
    // evidence_window_max_chunks, set at deep_research_cmd.rs:600);
    // assert the invariant instead of silently truncating the
    // one-shot's evidence — the one-shot must be the SAME retrieval.
    const LOOP_WINDOW_CAP: usize = 20;
    if deck.hits.len() > LOOP_WINDOW_CAP {
        return Err(format!(
            "{}: {} hits exceed the loop's {} chunk window cap — the one-shot would not be the same retrieval",
            pair.id,
            deck.hits.len(),
            LOOP_WINDOW_CAP
        ));
    }
    let window = window_from_deck(&deck, &pair.id, 2);
    let cfg = SetupConfig::load().map_err(|e| format!("{}: read config: {e}", pair.id))?;
    let endpoint = format!("http://localhost:{}/v1", cfg.daemon.client_port);
    let draft_model = cfg
        .models
        .primary
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "config models.primary has no filename stem".to_string())?
        .to_string();
    let provider: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(&endpoint, None, &draft_model, 8192));
    let real = Arc::new(DaemonPort { provider });
    let mock = MockBackendImpl::new(deck.clone(), MockDraftSurface::Delegated(real));

    let draft = draft_round(&mock, &pair.id, "oneshot-arm", 2, &pair.question, &window, &[])
        .await
        .map_err(|e| format!("{}: {e}", pair.id))?;

    let md = out.join(format!("oneshot-{}.md", pair.id));
    let wj = out.join(format!("oneshot-{}-window.json", pair.id));
    std::fs::write(&md, draft.text).map_err(|e| format!("{}: write {md:?}: {e}", pair.id))?;
    let window_json = serde_json::to_string_pretty(&window)
        .map_err(|e| format!("{}: window serialize: {e}", pair.id))?;
    std::fs::write(&wj, window_json)
        .map_err(|e| format!("{}: write {wj:?}: {e}", pair.id))?;
    eprintln!("oneshot_rag: {} -> {md:?} ({} chunks in the window)", pair.id, window.chunks.len());
    Ok(())
}

#[test]
#[ignore = "the two-arm control's one-shot leg; run by research/deep-research/arms/run-arms.sh with DR_ARM_PAIRS + DR_ARM_OUT set"]
fn one_shot_arm() {
    let pairs_path = std::env::var("DR_ARM_PAIRS")
        .expect("DR_ARM_PAIRS must point at pairs.json (written by run-arms.sh)");
    let out_dir = PathBuf::from(
        std::env::var("DR_ARM_OUT").expect("DR_ARM_OUT must be the one-shot output dir"),
    );
    std::fs::create_dir_all(&out_dir).expect("create DR_ARM_OUT");
    let pairs = parse_pairs(&pairs_path).expect("pairs.json parses");
    assert!(!pairs.is_empty(), "pairs.json must not be empty");
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let mut failures = Vec::new();
    for pair in &pairs {
        if let Err(e) = rt.block_on(one_shot(pair, &out_dir)) {
            failures.push(e);
        }
    }
    if !failures.is_empty() {
        panic!("one-shot arm failures:\n  {}", failures.join("\n  "));
    }
    eprintln!("oneshot_rag: all {} one-shot drafts written to {:?}", pairs.len(), out_dir);
}
