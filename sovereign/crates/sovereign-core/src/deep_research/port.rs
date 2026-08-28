// SPDX-License-Identifier: AGPL-3.0-or-later
//! The LIVE `ResearchPort` — real estate, real network, real daemon.
//!
//! One concern: give the deep-research loop its production surface.
//! Estate reads come from the corpus-engine indexes under
//! `~/.svrnmesh/indexes`; the web leg goes through
//! `sovereign_tools_base`'s search registry and fetch-and-extract; all
//! inference goes through one `InferenceProvider` pointed at the local
//! daemon's `/v1`, so the loop never touches a frontier.
//!
//! This lives in `sovereign-core`, beside its sibling `MockBackendImpl`
//! (`gym.rs`), because a port is runtime, not a host. It used to live in
//! `sovereign-cli`, which forced every other surface that wanted deep
//! research to SPAWN the CLI as a subprocess — and config does not cross
//! a process boundary, so the caller's search provider and env never
//! reached the loop. One implementation, one process, one config surface.
//!
//! Custody is stamped here, by code, never by a model (R-2/R-6): estate
//! hits carry `personal` (a local corpus is the operator's own data), web
//! hits carry `public-web`. The loop's gate refuses unknown provenance.

/// The canonical index directory (the estate).
use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::index::CorpusIndex;
use corpus_engine::Corpus;
use sovereign_contracts::types::{CompletionRequest, CompletionResponse, Speed};

use super::acquisition::web_hit_relevance;
use super::estate::{
    estate_snippet, read_staged_alignment, AlignmentDecision, DraftLeg, EstateListing, PortHit,
    ResearchPort,
};
use super::gym::{CorpusSurface, Deck, MockBackendImpl, MockDraftSurface, ProviderEmbed};
use super::icd::{CorpusEntry, Plan};
use super::SearchSource;
use crate::egress::{ConsentGrant, EgressPayload};
use crate::setup_config::SetupConfig;
use crate::traits::InferenceProvider;
use crate::types::Custody;

pub fn indexes_dir() -> PathBuf {
    SetupConfig::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .join("indexes")
}

/// A corpus directory is searchable iff it carries a LanceDB table
/// (the same validity gate the engine's `installed_indexes` uses).
fn corpus_searchable(dir: &std::path::Path) -> bool {
    std::fs::metadata(dir.join("chunks.lance").join("_versions")).is_ok()
        || std::fs::metadata(Corpus::meta_in(dir)).is_ok()
}

/// The chunk count from `_corpus_meta.json` when present; 0 when the
/// file is absent or unreadable — a listed-but-uncounted corpus is
/// still searchable. The engine's meta schema v3 writes the count as
/// `chunks_expected` (null in some pipelines) with `next_chunk_id`
/// carrying the committed chunk count — measured: the demo re-ask's
/// survey listed apollo11-evidence with chunks_count 0 because the
/// read looked for the schema-v2 `chunk_count` key.
fn corpus_chunk_count(dir: &std::path::Path) -> i64 {
    std::fs::read_to_string(Corpus::meta_in(dir))
        .ok()
        .and_then(|meta| serde_json::from_str::<serde_json::Value>(&meta).ok())
        .and_then(|v| {
            v["chunks_expected"]
                .as_i64()
                .or_else(|| v["next_chunk_id"].as_i64())
        })
        .unwrap_or(0)
}

/// local daemon's `/v1` surface — the loop never touches a frontier.
pub struct LiveResearchPort {
    provider: Arc<dyn InferenceProvider>,
    client: reqwest::Client,
    orchestrator: sovereign_tools_base::web::search::SearchOrchestrator,
    /// The run's typed consent grant (order deep-research-t2a): the
    /// port carries it to the egress boundary for every web-leg
    /// dispatch. `None` is default-deny — the web leg refuses
    /// non-public-web payloads (the run's machine-formed queries).
    consent: Option<ConsentGrant>,
    /// The backend `configured_search` chose from the operator's
    /// `[search]` section — the ONE source of the loop's web-backend
    /// identity. No second read of the config or the env var exists.
    web_backend: String,
    indexes: std::path::PathBuf,
    daemon_endpoint: String,
}

impl LiveResearchPort {
    fn new(provider: Arc<dyn InferenceProvider>, consent: Option<ConsentGrant>) -> Self {
        // The boundary's search-client factory — the ONE construction
        // site for clients that carry query egress (F26 census:
        // everything else in this file is LocalDaemon).
        let client = crate::egress::search_client().expect("egress boundary search client build");
        // The operator's `[search]` section is the ONE source of the
        // web backend — the same one the desktop's chat tools read, so a
        // provider configured once serves every surface. The env read is
        // the older `SVRNMESH_TAVILY_API_KEY` path (house canonical
        // spelling via `svrnmesh_env`; the legacy `SOVEREIGN_` prefix is
        // bridged at CLI startup), kept so existing setups keep working
        // and declared in quality/env-flags.toml. Presence is logged;
        // the value never is.
        // `launch::prepare` already loaded and validated this config to
        // resolve the daemon targets, and refuses loudly when it cannot
        // — so an unreadable config never reaches here. Taking just the
        // section keeps that the launch path's error to report, not a
        // second refusal in a constructor that cannot return one.
        let search_cfg = SetupConfig::load().map(|c| c.search).unwrap_or_default();
        let env_key = sovereign_contracts::rebrand::svrnmesh_env("TAVILY_API_KEY")
            .and_then(|v| v.into_string().ok());
        let configured =
            sovereign_tools_base::web::search::configured_search(&search_cfg, env_key.as_deref());
        let web_backend = configured.preferred.clone();
        eprintln!("deep-research: web backend {web_backend} (duckduckgo always available)");
        let registry = configured.registry;
        // The web-leg consent posture is declared once here and
        // carried to the boundary at every dispatch.
        match &consent {
            Some(g) => eprintln!(
                "deep-research: consent grant for run {} — release floor {} (recorded in the manifest)",
                g.run_id, g.release_floor
            ),
            None => eprintln!(
                "deep-research: no consent grant — the web leg is default-deny for \
                 non-public-web payloads (--consent <public-web|peer|personal> to release)"
            ),
        }
        let orchestrator =
            sovereign_tools_base::web::search::SearchOrchestrator::new(Arc::new(registry));
        let daemon_endpoint = format!(
            "http://localhost:{}",
            SetupConfig::load()
                .map(|c| c.daemon.client_port)
                .unwrap_or(9741)
        );
        LiveResearchPort {
            provider,
            client,
            orchestrator,
            consent,
            web_backend,
            indexes: indexes_dir(),
            daemon_endpoint,
        }
    }

    /// The loop's web backend. Not re-derived here: `configured_search`
    /// decided it once from the operator's `[search]` section and this
    /// returns that decision verbatim (§10.6, one decider one name).
    fn default_web_backend(&self) -> &str {
        &self.web_backend
    }
}

#[async_trait::async_trait]
impl ResearchPort for LiveResearchPort {
    async fn estate_listing(&self, corpus_ids: &[String]) -> Result<EstateListing, String> {
        let mut corpora = Vec::new();
        for id in corpus_ids {
            let dir = self.indexes.join(id);
            if !dir.is_dir() {
                // Absent corpus — still listed, not searchable (the
                // survey's F16 assert sees the truth).
                corpora.push(CorpusEntry {
                    corpus_id: id.clone(),
                    kind: "knowledge".to_string(),
                    chunks_count: 0,
                    searchable: false,
                    custody: Custody::Personal.as_str().to_string(),
                });
                continue;
            }
            corpora.push(CorpusEntry {
                corpus_id: id.clone(),
                kind: "knowledge".to_string(),
                chunks_count: corpus_chunk_count(&dir),
                searchable: corpus_searchable(&dir),
                custody: Custody::Personal.as_str().to_string(),
            });
        }
        Ok(EstateListing { corpora })
    }

    async fn estate_search(
        &self,
        corpus_ids: &[String],
        query: &str,
        limit: usize,
    ) -> Result<Vec<PortHit>, String> {
        let embedding = self
            .provider
            .embed(query)
            .await
            .map_err(|e| format!("embed `{query}`: {e}"))?;
        let mut hits = Vec::new();
        for id in corpus_ids {
            let dir = self.indexes.join(id);
            if !corpus_searchable(&dir) {
                continue;
            }
            let index = CorpusIndex::open(&dir)
                .await
                .map_err(|e| format!("open corpus `{id}`: {e}"))?;
            let results = index
                .search(&embedding, query, limit)
                .await
                .map_err(|e| format!("search `{id}`: {e}"))?;
            for r in results {
                hits.push(PortHit {
                    id: r.chunk_id.unwrap_or_default().to_string(),
                    // The locator carries the CHUNK id (t1g rung 2 —
                    // the estate window's `estate:<corpus>:<chunk>`
                    // convention): a corpus-level-only locator made
                    // every chunk of a corpus identical to the
                    // window's dedup-by-url, collapsing multi-hit
                    // estate searches to one chunk (journaled in the
                    // t1g declaration). Synthetic chunks (chunk_id
                    // None — atlas-virtual summaries, one per corpus)
                    // keep the corpus-level locator, which is correct
                    // for them.
                    url: r.url.clone().unwrap_or_else(|| {
                        format!("estate:{id}:{}", r.chunk_id.unwrap_or_default())
                    }),
                    title: r.title.unwrap_or_default(),
                    snippet: estate_snippet(&r.content, query, 600),
                    // The BODY rides the hit (t1h — the triage
                    // boundary: the term-centered snippet cut can miss
                    // the digits; the decider reads the body). Parity
                    // with the gym's corpus surface — one shape.
                    content: Some(r.content.clone()),
                    score: r.score as f64,
                    source: format!("estate:{id}"),
                    custody: Custody::Personal,
                });
            }
        }
        Ok(hits)
    }

    async fn web_search(
        &self,
        backend: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<PortHit>, String> {
        // The privacy posture's provider id must be `'static` (it is
        // the backend's stable audit id). The known backends are a
        // closed set — map the trait's `&str` onto the static ids,
        // fail-closed on anything else (a misspelled or unregistered
        // backend must not silently route).
        let provider: &'static str = match backend {
            "duckduckgo" => "duckduckgo",
            "brave" => "brave",
            "tavily" => "tavily",
            other => {
                return Err(format!(
                    "unknown web backend `{other}` (closed set: duckduckgo, brave, tavily)"
                ))
            }
        };
        // The egress boundary's release rule, before any request is
        // built (order deep-research-t2a, rung 3): the run's queries
        // are MACHINE-formed (the loop's gap templates — the user's
        // question folded with estate residue), so they carry the
        // run's consent grant; without one the leg refuses, typed,
        // naming what was withheld.
        crate::egress::verify(
            &EgressPayload {
                privacy: sovereign_tools_base::web::search::SearchPrivacy::External { provider },
                custody: Custody::Personal,
                what: "query",
                target: provider,
                detail: query,
                user_formed: false,
            },
            self.consent.as_ref(),
        )
        .map_err(|r| format!("web search refused: {r}"))?;
        let out = self
            .orchestrator
            .search(
                &self.client,
                sovereign_tools_base::web::search::SelectInputs {
                    query,
                    max_results: limit,
                    max_privacy: sovereign_tools_base::web::search::SearchPrivacy::External {
                        provider,
                    },
                    prefer: &[provider],
                },
            )
            .await;
        // Zero results is a RECORD, not a failure: the orchestrator
        // already logged the backend failure ("synthetic 0-results"),
        // and the loop journals the honest empty round (empty window →
        // never-ran abstention → done-partial with truncation
        // declared). Killing the run here would abandon the run dir
        // without a manifest — the F28 "instrument unavailable ≠
        // could-not-judge" shape, measured in demo run dr-1786720584.
        //
        // drb1-t1: every web hit is SCORED — the ONE web-admission
        // decider (acquisition::web_hit_relevance) over the hit's
        // recorded surface. The previous literal `score: 0.0` handed
        // triage a fully-tied field on every round (the t7a flight:
        // 843/843 rows at exactly 0.0), so admission fell to the
        // figure-bearing tie-break plus backend insertion order and
        // task 56's exact-topic papers all cut below-cut.
        let hits: Vec<PortHit> = out
            .results
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                let score = web_hit_relevance(query, &r.title, &r.snippet, &r.url);
                PortHit {
                    id: format!("web-{i}"),
                    url: r.url,
                    title: r.title,
                    snippet: r.snippet,
                    // Web results carry no body through this surface.
                    content: None,
                    score,
                    source: format!("web:{backend}"),
                    custody: Custody::PublicWeb,
                }
            })
            .collect();
        tracing::debug!(
            target: "deep_research",
            backend,
            query,
            hits = hits.len(),
            scores = ?hits.iter().map(|h| (h.id.as_str(), h.score)).collect::<Vec<_>>(),
            "web admission scored (drb1-t1)"
        );
        Ok(hits)
    }

    async fn web_fetch(&self, url: &str) -> Result<String, String> {
        // The estate scheme (t1g rung 2): `estate:<corpus_id>:<chunk_id>`
        // — the corpus IS the evidence store, the chunk's own content
        // is the fetch. The acquisition's corpus-source hits fetch
        // through here; a malformed or missing locator refuses loudly.
        if let Some(rest) = url.strip_prefix("estate:") {
            let (id, chunk) = rest.split_once(':').ok_or_else(|| {
                format!("malformed estate locator: {url} (expected estate:<corpus_id>:<chunk_id>)")
            })?;
            let chunk_id: u64 = chunk.parse().map_err(|_| {
                format!("malformed estate locator: {url} (chunk id `{chunk}` is not an id)")
            })?;
            let dir = self.indexes.join(id);
            if !corpus_searchable(&dir) {
                return Err(format!(
                    "estate fetch {url}: corpus `{id}` is not searchable"
                ));
            }
            let index = CorpusIndex::open(&dir)
                .await
                .map_err(|e| format!("estate fetch {url}: open corpus `{id}`: {e}"))?;
            let stored = index
                .get_chunks(&[chunk_id])
                .await
                .map_err(|e| format!("estate fetch {url}: {e}"))?;
            return stored
                .into_iter()
                .next()
                .map(|c| c.content)
                .ok_or_else(|| format!("estate fetch {url}: chunk {chunk_id} not found"));
        }
        // The fetch URL is a public-web payload (it came from web
        // search hits) — the boundary releases it unconditionally and
        // traces the egress event.
        crate::egress::verify(
            &EgressPayload {
                privacy: sovereign_tools_base::web::search::SearchPrivacy::External {
                    provider: "web-fetch",
                },
                custody: Custody::PublicWeb,
                what: "url",
                target: url,
                detail: url,
                user_formed: false,
            },
            self.consent.as_ref(),
        )
        .map_err(|r| format!("fetch refused: {r}"))?;
        // drb1-t2 (the PDF wall, order drb1-t2): scholarly gold is
        // often PDF — the logged t7a flight's task 56 admitted four
        // exact-topic papers that were ALL fetch-refused as binary
        // (8 urls flight-wide, every one `non-text payload`). A PDF
        // url now routes to the port-side extractor (pdf-extract
        // 0.7.12 — the SAME crate+version the corpus ingest path
        // uses; reuse, not a second extractor), and only non-PDF
        // binaries keep refusing. The classification is the ONE
        // accessor (sovereign-core fetch::source_type_of).
        if crate::deep_research::fetch::source_type_of(url)
            == crate::deep_research::icd::SourceType::Pdf
        {
            return fetch_pdf_text(&self.client, url)
                .await
                .map_err(|e| format!("fetch {url}: {e}"));
        }
        // The research cap, not the snippet cap (2026-08-24). The shared
        // extractor defaulted to 4,000 characters — a chat-tool budget —
        // and this leg took it silently, so `fetch::CHUNK_CONTENT_CAP`
        // (12,000), the cap deep-research DECLARES for an evidence chunk,
        // could never bind: a tighter constant three layers away had
        // already decided. Measured over the 45 pages a logged DRB-I
        // flight fetched: median page 22,293 chars, 88% over 4,000, and
        // the cap kept 156,407 of 1,409,433 available characters (11%).
        // We had already paid the fetch for all of it.
        //
        // Truncation is now VISIBLE. The trait returns a bare String, so
        // the fact rides the marker the evidence layer already knows
        // (`fetch::TRUNCATION_MARKER`, which `cap_content` appends for
        // the same reason) and the dropped count rides the log. A cap
        // that silently eats 89% of the evidence is the shape of bug
        // this subsystem keeps rediscovering; it does not get to be
        // silent again.
        let out = sovereign_tools_base::web::extract::fetch_and_extract_capped(
            &self.client,
            url,
            crate::deep_research::fetch::CHUNK_CONTENT_CAP,
        )
        .await
        .map_err(|e| format!("fetch {url}: {e}"))?;
        if out.truncated {
            tracing::debug!(
                target: "deep_research",
                url = %url,
                kept = out.text.chars().count(),
                page_chars = out.full_chars,
                dropped = out.dropped_chars(),
                "fetch truncated at the evidence chunk cap"
            );
            return Ok(format!(
                "{}{}",
                out.text,
                crate::deep_research::fetch::TRUNCATION_MARKER
            ));
        }
        Ok(out.text)
    }

    async fn terminal_poll(&self) -> Result<(), String> {
        let probe = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .map_err(|e| format!("http client build: {e}"))?;
        // The daemon's model listing is `/v1/models` (the surface every
        // other CLI consumer probes — awareness, code-index). The earlier
        // bare `/models` probe 404'd against the real daemon (measured
        // 08-14 during the demo's run 2 preflight).
        let url = format!("{}/v1/models", self.daemon_endpoint);
        match probe.get(&url).send().await {
            Ok(r) if r.status().is_success() => Ok(()),
            Ok(r) => Err(format!("daemon returned {} from {url}", r.status())),
            Err(e) => Err(format!(
                "daemon unreachable at {}: {e}",
                self.daemon_endpoint
            )),
        }
    }

    async fn draft(
        &self,
        leg: DraftLeg,
        prompt: &str,
        system_message: Option<&str>,
        allowed_urls: &[String],
    ) -> Result<String, String> {
        let speed = slot_for(leg);
        let (draft_temp, draft_top_p) = draft_sampling(0.4);
        tracing::debug!(
            target: "deep_research",
            ?leg, ?speed, prompt_chars = prompt.len(),
            temperature = ?draft_temp, pinned = sampling_pinned(),
            "deep-research: drafting leg dispatched"
        );
        let resp = complete_with_shed_retry(
            &*self.provider,
            &CompletionRequest {
                prompt: prompt.to_string(),
                system_message: system_message.map(|s| s.to_string()),
                preferred_speed: speed,
                max_tokens: None,
                temperature: draft_temp,
                top_p: draft_top_p,
                structured_output: None,
                think_budget: None,
                url_allowlist: Some(allowed_urls.to_vec()),
                ..Default::default()
            },
            "draft",
        )
        .await
        .map_err(|e| {
            let text = e.to_string();
            if !looks_prompt_overflow(&text) {
                return format!("draft ask: {text}");
            }
            // The raw message names a token count and a window and NOTHING
            // that can be acted on. Name the slot this leg runs on and the
            // key that sizes it, because that is the repair.
            tracing::error!(
                target: "deep_research",
                ?leg, ?speed, prompt_chars = prompt.len(), error = %text,
                "deep-research: the prompt does not fit this slot's context \
                 window — a CONFIGURATION failure, not load"
            );
            format!(
                "draft ask: {text}\n  {}",
                prompt_overflow_hint(speed, leg, prompt.len())
            )
        })?;
        Ok(resp.text)
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        // drb1-t5: the gate's support binder and the writer's
        // per-section retrieval both need the retrieval embedding
        // space. Same provider leg as every other inference call — one
        // `RemoteApiProvider` against the daemon.
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(
                self.provider
                    .embed(t)
                    .await
                    .map_err(|e| format!("embed: {e}"))?,
            );
        }
        Ok(out)
    }

    async fn gap_queries(&self, question: &str, gaps: &[String]) -> Result<Vec<String>, String> {
        if gaps.is_empty() {
            return Ok(Vec::new());
        }
        // ONE CALL PER GAP, not one call per round. The batched form was
        // measured first and lost its count on exactly the rounds that
        // need this most: replaying the logged t7a flight's gap lists
        // through a batched prompt, 11 of 15 rounds returned the right
        // number of lines and 4 did not — and the misses were the LONG
        // lists (13, 19 and 21 gaps), including both rounds of task 90,
        // the worst-yielding task in the flight. Asking a 4B to emit
        // exactly 21 lines in order is asking it to count; asking it to
        // rewrite one sentence is the narrow role it is good at. Per-gap
        // also makes the fallback granular — one unusable rewrite costs
        // that gap its reformulation instead of discarding twenty good
        // ones alongside it.
        //
        // `buffered`, NOT `buffer_unordered`: the caller matches these
        // back to gaps BY INDEX, so order is the contract (the same
        // reason `audit_pass` uses it). AUDIT_CONCURRENCY tracks the
        // daemon's `max_concurrent_turns`; past it the REST path sheds.
        use futures::StreamExt as _;
        // Owned, so the per-gap futures borrow nothing from the caller's
        // slice (a borrowing closure here is not general enough over the
        // lifetimes the stream needs).
        // The gap text is DRAFT prose, so it carries the draft's citation
        // apparatus — `[Source: ev-1]` spans and bare `ev-N` ids. Left in,
        // they ride into the rewrite and out the other side ("ev-2
        // liability allocation accidents"), which is the same leak
        // `template_query` strips on the deterministic path. Strip once,
        // here, before the model ever sees them — reusing the existing
        // decider (`containment::strip_citation_spans`) rather than
        // minting a second one (§19, §10.6).
        let owned: Vec<String> = gaps
            .iter()
            .map(|g| {
                let cleaned = crate::deep_research::containment::strip_citation_spans(g);
                cleaned
                    .split_whitespace()
                    .filter(|w| {
                        let core = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
                        !(core.starts_with("ev-")
                            && core[3..].chars().all(|c| c.is_ascii_digit())
                            && core.len() > 3)
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        let results: Vec<Result<String, String>> =
            futures::stream::iter(owned.into_iter().map(|gap| {
                let question = question.to_string();
                async move {
                    let prompt = format!(
                        "A research report made this statement and could not support it:\n\n\
                     {gap}\n\n\
                     Write ONE web search query that would find evidence for it.\n\n\
                     Rules:\n\
                     - Name the subject in full. Your query is read with no other \
                     context, so replace every pronoun and every phrase like \"this \
                     system\" or \"the protocol\" with the thing it refers to.\n\
                     - Ask for one thing.\n\
                     - Keep any specific figure, date or proper name the statement \
                     carries.\n\
                     - Write what you would type into a search box, not a sentence \
                     from a report.\n\
                     - Output the query and nothing else.\n\n\
                     Research question, for context only: {question}"
                    );
                    let (rewrite_temp, rewrite_top_p) = draft_sampling(0.3);
                    complete_with_shed_retry(
                        &*self.provider,
                        &CompletionRequest {
                            prompt,
                            system_message: None,
                            preferred_speed: Speed::Fast,
                            max_tokens: None,
                            temperature: rewrite_temp,
                            top_p: rewrite_top_p,
                            // Constrained to a one-field object, and thinking
                            // off. Measured: the 4B prefixed its answer with
                            // its own reasoning ("The user wants me to write a
                            // web search query...", "Thinking Process:") on 4
                            // of 46 rewrites, and a preamble blocklist is
                            // whack-a-mole — a new opener defeats it (§0).
                            // Under a schema the preamble cannot be emitted at
                            // all: valid JSON has nowhere to put it (§7.6 —
                            // never ask a model to guarantee what code can
                            // enforce).
                            structured_output: Some(serde_json::json!({
                                "type": "object",
                                "properties": { "query": { "type": "string" } },
                                "required": ["query"]
                            })),
                            think_budget: Some(0),
                            enable_thinking: Some(false),
                            url_allowlist: None,
                            ..Default::default()
                        },
                        "gap-queries",
                    )
                    .await
                    .map(|r| {
                        // The Fast slot is a 4B and it leaks its reasoning
                        // preamble ("The user wants me to write a web search
                        // query...", "Thinking Process:") ahead of the answer
                        // — the documented small-model shape this workspace
                        // has hit before. Reuse the runtime's stripper rather
                        // than pattern-matching preambles here.
                        // The schema's object first; the line form is the
                        // fallback for a provider that could not honour it
                        // (recorded by falling through to the gap's own text
                        // downstream, never silently).
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(r.text.trim()) {
                            if let Some(q) = v.get("query").and_then(|q| q.as_str()) {
                                if !q.trim().is_empty() {
                                    return q.trim().trim_matches('"').to_string();
                                }
                            }
                        }
                        let text = crate::title::strip_thinking_response(&r.text);
                        text.lines()
                            .map(|l| {
                                l.trim()
                                    .trim_start_matches(['-', '*', ' '])
                                    .trim_start_matches(|c: char| {
                                        c.is_ascii_digit() || c == '.' || c == ')'
                                    })
                                    .trim()
                                    .trim_matches('"')
                                    .to_string()
                            })
                            .find(|l| !l.is_empty())
                            .unwrap_or_default()
                    })
                    .map_err(|e| format!("gap-queries ask: {e}"))
                }
            }))
            .buffered(crate::deep_research::AUDIT_CONCURRENCY)
            .collect()
            .await;

        // A gap whose rewrite failed or came back empty keeps its OWN
        // text at that index; the caller's `query_refusal` gate then
        // decides whether that is dispatchable. Never a silent
        // substitution — `formed_by` on the fetch list carries which
        // shape produced each query.
        let mut out = Vec::with_capacity(gaps.len());
        let mut failed = 0usize;
        for (i, r) in results.into_iter().enumerate() {
            match r {
                Ok(q) if !q.trim().is_empty() => out.push(q),
                _ => {
                    failed += 1;
                    out.push(gaps[i].clone());
                }
            }
        }
        if failed == gaps.len() {
            return Err(format!("all {failed} gap-query rewrites failed"));
        }
        if failed > 0 {
            tracing::warn!(
                target: "deep_research",
                gaps = gaps.len(),
                failed,
                "gap-queries: some rewrites failed — those gaps keep their own text"
            );
        }
        Ok(out)
    }

    async fn plan_subquestions(&self, question: &str) -> Result<Vec<String>, String> {
        // t1d fix 2 (breadth): the acquisition frontier — a constrained
        // draft asking for the question's decomposition, one sub-
        // question per line. Same inference leg as the drafts
        // (Speed::Slow, temperature 0.4) with NO url allowlist: the
        // frontier is a question list, not report content, and must not
        // cite. Lines are parsed deterministically (marker-stripped,
        // deduped, capped at the shared FRONTIER_MAX) — and the prompt
        // now ASKS for that many, because the cap alone never made the
        // model produce them.
        //
        // t1e (figure-hunting): the instruction asks, generically, what
        // measures and numbers each sub-question implies — an index, a
        // ratio, a share, a rate, a count, a median, a price, a
        // percentage change — and the entities (cities, years) they
        // involve, so the search can retrieve the DATA the question
        // asks for. SHAPE, never the test: no bank vocabulary, no named
        // measures from any deck — the draft names the measures from
        // its own knowledge. The loop's deterministic fold-in
        // (acquisition::figure_hunt_frontier) then guarantees every
        // sub-question carries a specifier structurally.
        // The WIDTH is asked for, not left to the model's taste. With no
        // target named, the 27B returned EIGHT lines on task 69 against a
        // cap of twelve, and the loop's breadth was decided by that
        // silence — gap rounds never recovered it (estate::FRONTIER_MAX
        // carries the measurement). The number comes from the cap itself
        // so the ask and the parser's ceiling cannot drift apart (§10.6).
        let want = crate::deep_research::estate::FRONTIER_MAX;
        let prompt = format!(
            "Decompose the research question into {want} sub-questions that a web search could \
             answer. Cover every distinct facet the question raises, including ones it implies \
             rather than names. \
             For each sub-question, name the specific measure or statistic it implies — an index, \
             a ratio, a share, a rate, a count, a median, a price, a percentage change — and the \
             entities it involves (cities, years), so a search for the data can retrieve it. If the \
             question implies specific numbers, name them. One sub-question per line, no citations, \
             no numbering, no commentary.\n\nQuestion: {question}"
        );
        let (plan_temp, plan_top_p) = draft_sampling(0.4);
        let resp = complete_with_shed_retry(
            &*self.provider,
            &CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Slow,
                max_tokens: None,
                temperature: plan_temp,
                top_p: plan_top_p,
                structured_output: None,
                think_budget: None,
                url_allowlist: None,
                ..Default::default()
            },
            "plan-subquestions",
        )
        .await
        .map_err(|e| format!("plan-subquestions ask: {e}"))?;
        let mut out: Vec<String> = Vec::new();
        for line in resp.text.lines() {
            let line = line.trim().trim_start_matches(['-', '*', ' ']).trim();
            let line = line
                .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')')
                .trim();
            if line.is_empty() || out.contains(&line.to_string()) {
                continue;
            }
            out.push(line.to_string());
            if out.len() >= crate::deep_research::estate::FRONTIER_MAX {
                break;
            }
        }
        Ok(out)
    }

    async fn alignment_decision(
        &self,
        _plan: &Plan,
        run_dir: &Path,
    ) -> Result<AlignmentDecision, String> {
        // STEER 2: the product's alignment gate is the STAGED INPUT —
        // the launcher writes `<run_dir>/alignment-input.json`
        // (ReframeInput shape, the operator's redirect + reason) before
        // the run; the shared reader consumes the file (one decider,
        // mock and CLI alike). Absent → Proceed. The run shows the
        // plan + acceptance shapes at the gate; the operator's call is
        // on the record (alignment-1.json, manifest, report header).
        read_staged_alignment(run_dir).map(|staged| staged.unwrap_or(AlignmentDecision::Proceed))
    }
}

// ----------------------------------------------------------------------
// T6b pre-window slice — shed retry around the inference leg (order
// deep-research-t6b, pre-registered). The daemon surfaces stuck
// generations as a named shed: 503 + Retry-After. The loop client
// previously DIED on the first 503 — evidenced by the seed-05 re-flight,
// which died on its FIRST draft call (arms/runs-t6b/loop/seed-05.console
// .log) with no retry despite the daemon's shed shape. Bounded retry,
// honoring the Retry-After hint when the wire carries one.
// ----------------------------------------------------------------------

/// Shed retries granted before the error is surfaced — the loop never
/// sits in a 503 loop past MAX_SHED_RETRIES + 1 total attempts.
const MAX_SHED_RETRIES: usize = 3;

/// Default backoff when the shed body carries no retry hint — mirrors
/// the mesh's yield-refusal default (sovereign-mesh decision_log.rs
/// YIELD_REFUSAL_DEFAULT_BACKOFF_SECS).
const SHED_DEFAULT_BACKOFF_SECS: u64 = 5;

/// drb1-t2 — fetch a PDF url and extract its text (the PDF wall).
///
/// REUSE (§19, the order's inventory answer): the extraction is
/// `pdf-extract 0.7.12` — the SAME crate+version the corpus ingest
/// path runs (`sovereign-tools`' `local_corpus::extract_stage`), so
/// there is ONE PDF-to-text implementation in the workspace and this
/// is a second CALLER of it, not a second extractor. The panic guard
/// here is required (a `pdf-extract` panic — its DeviceN colour-space
/// path `unimplemented!()`s — would otherwise unwind the whole
/// research run); the corpus path's stdout-silencing is NOT
/// duplicated (that lives with its wrapper in sovereign-tools, out of
/// this change's landing paths — filed for the seat as a lift-to-
/// tools-base item: one shared panic-safe, silenced PDF accessor for
/// both paths). A PDF whose extraction fails (encrypted, malformed,
/// panic) is a typed error the fetch leg journals and classifies —
/// never a window-poisoning payload.
///
/// The extracted text is capped at the evidence chunk cap (12k chars,
/// `crate::deep_research::fetch::CHUNK_CONTENT_CAP` — the
/// ONE cap const): PDFs deliver full body text where the HTML path's
/// 4k cut (frozen in sovereign-tools-base) delivers chrome-heavy
/// prefixes. An HTML page served AT a .pdf url extracts as HTML
/// (the shared `extract_text_from_html`), and a non-PDF binary keeps
/// the port's named refusal shape so the fetch leg's health
/// classifier still sees `non-text payload`.
async fn fetch_pdf_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = client
        .get(url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        )
        .send()
        .await
        .map_err(|e| format!("Failed to fetch {url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("HTTP {} for {url}", response.status()));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let body = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read response from {url}: {e}"))?;
    let is_pdf = body.starts_with(b"%PDF") || content_type.starts_with("application/pdf");
    if !is_pdf {
        // A .pdf url that served HTML (a soft-404 landing page, an
        // HTML viewer) extracts as HTML — the shared extractor.
        let html = String::from_utf8_lossy(&body);
        let text = sovereign_tools_base::web::extract::extract_text_from_html(&html);
        let capped: String = text
            .chars()
            .take(crate::deep_research::fetch::CHUNK_CONTENT_CAP)
            .collect();
        return Ok(capped);
    }
    extract_pdf_bytes(&body).await
}

/// The PDF bytes → text half of the fetch path (drb1-t2): bytes →
/// staging file → the extractor under `catch_unwind` on the blocking
/// pool → text capped at the evidence chunk cap. `pdf_extract::
/// extract_text` is CPU-bound and panics on some malformed inputs —
/// both are the blocking task's problem, and the panic becomes a
/// typed error (the unit-tested half of `fetch_pdf_text`).
async fn extract_pdf_bytes(body: &[u8]) -> Result<String, String> {
    let tmp = tempfile::Builder::new()
        .prefix("drb1-t2-fetch-")
        .suffix(".pdf")
        .tempfile()
        .map_err(|e| format!("pdf staging file: {e}"))?;
    std::fs::write(tmp.path(), body).map_err(|e| format!("pdf staging write: {e}"))?;
    let path = tmp.path().to_path_buf();
    let extracted = tokio::task::spawn_blocking(move || {
        use std::panic::{catch_unwind, AssertUnwindSafe};
        catch_unwind(AssertUnwindSafe(|| pdf_extract::extract_text(&path)))
    })
    .await
    .map_err(|e| format!("pdf extraction task failed: {e}"))?
    .map_err(|payload| {
        // Same downcast discipline as the corpus path's wrapper.
        let msg = payload
            .downcast_ref::<&'static str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown pdf-extract panic".to_string());
        format!("pdf-extract panicked during extraction: {msg}")
    })?
    .map_err(|e| format!("pdf extraction failed: {e}"))?;
    let capped: String = extracted
        .chars()
        .take(crate::deep_research::fetch::CHUNK_CONTENT_CAP)
        .collect();
    Ok(capped)
}

/// Is this inference error the daemon's shed shape (503 busy /
/// overloaded / shutting down)? Mirrors the mesh's `looks_shed`
/// classification token-for-token — same lowercase contains.
fn looks_shed(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "503",
        "service unavailable",
        "too many requests",
        "429",
        "retry-after",
    ]
    .iter()
    .any(|tok| t.contains(tok))
}

/// Is this inference error a prompt that cannot fit the slot's context
/// window? Such a failure is DETERMINISTIC and must never be retried: the
/// prompt does not shrink and the window does not grow, so every attempt
/// fails identically while the "shed" label points diagnosis at load.
///
/// Classified by CONTENT, not by status code, deliberately. The local daemon
/// reports it as 503, but a peer, a proxy, or a future version may report it
/// as 400 or 413 — and the retry decision must be right in all of those
/// cases. `looks_shed` stays untouched: it answers "did this arrive in shed
/// shape?", which is still true here; this answers the different question the
/// retry loop actually needs.
fn looks_prompt_overflow(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("prompt too long") || t.contains("exceeds the context window")
}

/// What to actually DO about a prompt that does not fit its slot.
///
/// The daemon's message names a token count and a window and nothing that can
/// be acted on; a reader who has not been bitten before will look at load,
/// because the failure arrives wearing a 503. This names the slot the leg runs
/// on and the key that sizes it, which is the repair.
fn prompt_overflow_hint(speed: Speed, leg: DraftLeg, prompt_chars: usize) -> String {
    let key = match speed {
        Speed::Fast => "[models].fast_context_size (unset falls back to context_size)",
        _ => "[models].context_size",
    };
    format!(
        "The {speed:?} slot serves the {leg:?} leg, and this prompt was \
         {prompt_chars} chars. A window smaller than the leg's evidence budget \
         fails every time — check {key} in ~/.sovereign/config.toml."
    )
}

/// The Retry-After hint from a shed body, in seconds. Tries the
/// admission shape's `retry_after_secs` key first (a bare `retry_after`
/// search would match inside `retry_after_secs`), then the busy
/// shape's `retry_after` key. Mesh-style digit parse, clamped to
/// [1, 300] so a shed can never command a multi-minute stall.
fn shed_retry_hint_secs(text: &str) -> Option<u64> {
    for key in ["retry_after_secs", "retry_after"] {
        if let Some(idx) = text.find(key) {
            let tail = &text[idx + key.len()..];
            let digits: String = tail
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !digits.is_empty() {
                if let Ok(secs) = digits.parse::<u64>() {
                    return Some(secs.clamp(1, 300));
                }
            }
        }
    }
    None
}

/// Bounded retry around `provider.complete` for the loop's inference
/// legs (draft, plan-subquestions): a shed (503) is retried up to
/// MAX_SHED_RETRIES times with the Retry-After hint (or the default
/// backoff); anything else — and the last shed — surfaces IMMEDIATELY
/// as the raw error text, so callers keep their own `draft ask: {e}`
/// / `plan-subquestions ask: {e}` framing and the evidenced seed-05
/// error shape survives exhausted retries unchanged.
async fn complete_with_shed_retry(
    provider: &dyn InferenceProvider,
    request: &CompletionRequest,
    what: &str,
) -> Result<CompletionResponse, String> {
    let mut attempt: usize = 0;
    loop {
        match provider.complete(request).await {
            Ok(resp) => {
                if attempt > 0 {
                    tracing::info!(
                        what,
                        attempt,
                        "deep-research: inference recovered after a shed"
                    );
                }
                return Ok(resp);
            }
            Err(e) => {
                let text = e.to_string();
                // A prompt that does not fit the slot's context window is
                // DETERMINISTIC — the same prompt against the same window
                // fails identically forever — so it is never a shed, whatever
                // status code it arrives under. The daemon returns it as a
                // 503, which `looks_shed` matches on, and the retries then
                // burn the backoff AND file a CONFIGURATION failure under
                // LOAD. Measured 2026-08-27: `fast_context_size = 16384` left
                // the round leg's ~96,000-char budget unservable and every
                // flight died after three pointless retries whose log line
                // said "shed", sending the reader to look at contention.
                if attempt >= MAX_SHED_RETRIES || !looks_shed(&text) || looks_prompt_overflow(&text)
                {
                    // Honest error, surfaced raw — never a substitution.
                    return Err(text);
                }
                attempt += 1;
                let backoff = shed_retry_hint_secs(&text).unwrap_or(SHED_DEFAULT_BACKOFF_SECS);
                tracing::warn!(
                    what,
                    attempt,
                    max = MAX_SHED_RETRIES,
                    backoff_secs = backoff,
                    "deep-research: inference shed (503) — honoring Retry-After, will retry"
                );
                tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
            }
        }
    }
}

/// Whether this run forces EVERY drafting leg onto the primary (Slow) slot.
///
/// On-form: `SOVEREIGN_DR_ALL_LEGS_SLOW=1` (also `true`, case-insensitive);
/// every other value, including unset, keeps the measured per-leg mapping in
/// [`slot_for`]. Shares [`pin_requested`] with the sampling pin so there is
/// ONE answer to "what counts as on" across the measurement switches.
///
/// Why it exists: the strategy question "is the gap to AIQ our architecture or
/// our model?" is only answerable if the big model does the WRITING, and the
/// writing leg (`Section`) is a Fast-slot leg.
fn all_legs_slow() -> bool {
    std::env::var("SOVEREIGN_DR_ALL_LEGS_SLOW")
        .map(|v| pin_requested(&v))
        .unwrap_or(false)
}

/// Whether this run pins the writer's sampling for MEASUREMENT.
///
/// Why it exists: the judge is pinned greedy (temperature 0.0, top_p 1.0 —
/// "one judge instrument, one sampling pin") while the writer drafts at
/// temperature 0.3-0.4. Measured 2026-08-25 on the task-69 replay bed, THREE
/// runs at identical configuration scored 42.21 / 41.99 / 48.91 — a 6.92-point
/// spread with evidence held constant, matching the 7.56-point pipeline spread
/// measured earlier. Every A/B run against that bed tested 2-3 point levers
/// with n=2, so none of them could resolve: we pinned the instrument and left
/// the subject free, then read the subject's variance as our levers.
///
/// On-form: `SOVEREIGN_DR_PIN_SAMPLING=1` (also `true`, case-insensitive).
/// EVERY other value, including unset, keeps production sampling — this is a
/// measurement harness, not a product change, and it must never alter what a
/// user gets by accident.
///
/// What it does NOT fix: a pinned writer is a slightly DIFFERENT writer than
/// the sampled one we ship, so a lever measured under the pin still needs
/// confirming unpinned before it lands. That trade is deliberate — an
/// underpowered measurement of the real writer resolves nothing at all.
fn sampling_pinned() -> bool {
    std::env::var("SOVEREIGN_DR_PIN_SAMPLING")
        .map(|v| pin_requested(&v))
        .unwrap_or(false)
}

/// Pure policy so the on-form is testable without touching process env
/// (same split as `memory_watch::hard_limit_policy`).
fn pin_requested(raw: &str) -> bool {
    let v = raw.trim();
    v == "1" || v.eq_ignore_ascii_case("true")
}

/// The sampling a drafting call should carry: `(temperature, top_p)`.
///
/// ONE decider for all three drafting call sites (§10.6) — before this,
/// three literals meant a pin could be applied to two of them and silently
/// missed on the third, which is exactly the shape of bug that makes a
/// measurement look controlled when it is not.
fn draft_sampling(production_temperature: f32) -> (Option<f32>, Option<f32>) {
    draft_sampling_for(sampling_pinned(), production_temperature)
}

/// Pure policy — the decision without the env read, so a test can cover
/// both arms in a parallel suite.
fn draft_sampling_for(pinned: bool, production_temperature: f32) -> (Option<f32>, Option<f32>) {
    if pinned {
        // Greedy, and top_p closed — identical to the judge's pin, so the
        // writer and the ruler are deterministic under the same switch.
        (Some(0.0), Some(1.0))
    } else {
        (Some(production_temperature), None)
    }
}

/// Which slot serves each drafting leg — the ONE mapping, so the answer
/// to "what drafts a section?" lives in one place instead of at each call
/// site (§10.6).
///
/// Measured on this host 2026-08-24, same prompt (3,130 chars of evidence,
/// 300-380 words asked), temperature 0, 3 reps each: the 4B fast slot
/// median **13.46s at 34.62 tok/s**; the 27B primary **68.95s at 7.47
/// tok/s** on its first rep. Roughly 5x, and the 4B held the asked word
/// count (369 words) byte-identically across all three reps.
///
/// Why the split is not "everything Fast": `Plan` shapes every later leg
/// — a worse decomposition costs more than it saves, and it is ONE call
/// per run rather than one per section. `Synthesis` reads the whole
/// report and is the Insight-dimension lever (the highest-weighted RACE
/// dimension), also one call. The two high-volume legs move; the two
/// single-call legs that set the ceiling stay.
///
/// This is a mapping, not a policy — if a measurement says `Synthesis`
/// survives the 4B, this is the one line that changes.
fn slot_for(leg: DraftLeg) -> Speed {
    // MEASUREMENT ESCAPE HATCH, default off. `Round` and `Section` route to
    // Fast below — and `Section` is the leg that WRITES THE DELIVERABLE. So a
    // model-ceiling diagnostic that only repoints the primary slot would leave
    // the actual writing on the 4B and answer a question nobody asked. This
    // forces every leg onto the primary slot so "what does our architecture
    // score when a big model writes it?" is the question actually measured.
    // It is not a product mode: it costs the 5x Fast-slot speedup on the two
    // volume legs (measured 13.46s vs 68.95s per call, 2026-08-24).
    slot_for_with(all_legs_slow(), leg)
}

/// Pure policy — the routing decision without the env read, so both arms are
/// testable in a parallel suite.
fn slot_for_with(forced_slow: bool, leg: DraftLeg) -> Speed {
    if forced_slow {
        return Speed::Slow;
    }
    match leg {
        // One call per run, and it decides the shape of everything after.
        DraftLeg::Plan => Speed::Slow,
        // One call per run, over the drafted report rather than evidence.
        DraftLeg::Synthesis => Speed::Slow,
        // One call per run, and it decides the deliverable's structure —
        // the dimension the criteria weigh most heavily.
        DraftLeg::Outline => Speed::Slow,
        // One call per sub-question — the volume legs.
        DraftLeg::Round | DraftLeg::Section => Speed::Fast,
        // One call per sub-question, so a volume leg by count — but it is
        // the leg the WRITER then reads instead of the evidence, and a
        // fabricated finding here becomes a cited sentence downstream that
        // the audit cannot locate. Bought on the slow slot deliberately;
        // the cost is named in the DEFAULTS_LEDGER row.
        DraftLeg::Research => Speed::Slow,
    }
}

/// The ONE port construction (order deep-research-t3a): `auto` — the
/// real network port; `mock` — the deck's search/fetch surface with
/// drafts delegated to the real port, and (when the search source is
/// corpus) the estate's REAL indexes as the acquisition surface. Used
/// by both the fresh launch and the resume — one implementation, one
/// decider, never two constructions that can drift.
pub async fn build_port(
    backend: &str,
    mock_deck_dir: Option<&Path>,
    search_source: SearchSource,
    corpora: &[String],
    provider: Arc<dyn InferenceProvider>,
    consent: Option<ConsentGrant>,
) -> Result<(Arc<dyn ResearchPort>, String), String> {
    if backend == "mock" {
        let deck_dir = mock_deck_dir.ok_or("--backend mock requires --mock-deck DIR")?;
        let deck = Deck::load(deck_dir).map_err(|e| format!("mock deck load failed: {e}"))?;
        let real = Arc::new(LiveResearchPort::new(provider.clone(), consent.clone()));
        let mock = if search_source == SearchSource::Corpus {
            let mut indexes = Vec::new();
            let mut missing = Vec::new();
            for id in corpora {
                let dir = indexes_dir().join(id);
                if !corpus_searchable(&dir) {
                    missing.push(id.clone());
                    continue;
                }
                match CorpusIndex::open(&dir).await {
                    Ok(i) => indexes.push(i),
                    Err(e) => return Err(format!("open corpus `{id}`: {e}")),
                }
            }
            if !missing.is_empty() {
                return Err(format!(
                    "--search-source corpus: corpus not searchable at the estate: {} — a \
                     named corpus is never silently skipped",
                    missing.join(", ")
                ));
            }
            MockBackendImpl::with_corpus(
                deck,
                MockDraftSurface::Delegated(real),
                CorpusSurface {
                    indexes,
                    embed: Box::new(ProviderEmbed(provider.clone())),
                },
            )
        } else {
            MockBackendImpl::new(deck, MockDraftSurface::Delegated(real))
        };
        Ok((Arc::new(mock), MockBackendImpl::BACKEND_ID.to_string()))
    } else {
        let real = Arc::new(LiveResearchPort::new(provider.clone(), consent));
        let web_backend = real.default_web_backend().to_string();
        Ok((real, web_backend))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        complete_with_shed_retry, draft_sampling_for, extract_pdf_bytes, looks_prompt_overflow,
        looks_shed, pin_requested, prompt_overflow_hint, shed_retry_hint_secs, slot_for_with,
        DraftLeg, MAX_SHED_RETRIES,
    };
    use crate::traits::InferenceProvider;
    use futures::Stream;
    use sovereign_contracts::error::Error as ContractError;
    use sovereign_contracts::types::{CompletionRequest, CompletionResponse, Speed};
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    // ------------------------------------------------------------------
    // T6b pre-window slice — the shed (503 + Retry-After) retry
    // (RED-FIRST: complete_with_shed_retry did not exist at HEAD; the
    // seed-05 re-flight died on its FIRST draft call with no retry —
    // arms/runs-t6b/loop/seed-05.console.log — order deep-research-t6b,
    // pre-registered).
    // ------------------------------------------------------------------

    /// A provider that sheds (503) the first `fails` completes, then
    /// answers. Attempts are counted so tests can assert the exact
    /// retry bound.
    struct ShedStub {
        fails: usize,
        error_text: String,
        attempts: Arc<Mutex<usize>>,
    }

    #[async_trait::async_trait]
    impl InferenceProvider for ShedStub {
        async fn complete(
            &self,
            _request: &CompletionRequest,
        ) -> std::result::Result<CompletionResponse, ContractError> {
            let mut attempts = self.attempts.lock().unwrap();
            *attempts += 1;
            if *attempts <= self.fails {
                Err(ContractError::Inference(self.error_text.clone()))
            } else {
                Ok(CompletionResponse {
                    text: "the answer".to_string(),
                    tokens_used: 0,
                    prompt_tokens: 0,
                    model_id: "shed-stub".to_string(),
                    latency_ms: 0,
                    oicp_meta: None,
                    finish_reason: None,
                    completion_tokens: None,
                })
            }
        }

        async fn complete_stream(
            &self,
            _request: &CompletionRequest,
        ) -> std::result::Result<
            Pin<Box<dyn Stream<Item = std::result::Result<String, ContractError>> + Send>>,
            ContractError,
        > {
            Err(ContractError::NotImplemented("ShedStub".into()))
        }

        async fn embed(&self, _text: &str) -> std::result::Result<Vec<f32>, ContractError> {
            Ok(vec![])
        }

        fn capabilities(&self) -> sovereign_contracts::types::ProviderCapabilities {
            sovereign_contracts::types::ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: true,
                relative_speed: Speed::Fast,
                relative_reasoning: sovereign_contracts::types::Depth::Moderate,
            }
        }
    }

    /// The classifier and hint parser read the REAL wire shapes — the
    /// admission shape (retry_after_secs key), the busy shape (bare
    /// retry_after key), the evidenced seed-05 death (no retry key at
    /// all — default backoff), and a transport failure (never a shed).
    #[test]
    fn forcing_every_leg_slow_overrides_the_per_leg_mapping() {
        // The mapping is the DEFAULT, and it must stay the default: the two
        // volume legs live on Fast for a measured 5x, and losing that
        // silently would make every run slower for no stated reason.
        assert_eq!(slot_for_with(false, DraftLeg::Section), Speed::Fast);
        assert_eq!(slot_for_with(false, DraftLeg::Round), Speed::Fast);
        assert_eq!(slot_for_with(false, DraftLeg::Plan), Speed::Slow);
        assert_eq!(slot_for_with(false, DraftLeg::Synthesis), Speed::Slow);
        // Forced: EVERY leg, especially Section — the leg that writes the
        // deliverable, and the whole point of a model-ceiling diagnostic.
        for leg in [
            DraftLeg::Section,
            DraftLeg::Round,
            DraftLeg::Plan,
            DraftLeg::Synthesis,
            DraftLeg::Outline,
        ] {
            assert_eq!(
                slot_for_with(true, leg),
                Speed::Slow,
                "{leg:?} must be forced Slow"
            );
        }
    }

    #[test]
    fn the_sampling_pin_is_opt_in_and_covers_every_drafting_leg() {
        // On-form, and nothing else. A measurement switch that turned itself
        // on from a stray value would silently change what users get.
        assert!(pin_requested("1"));
        assert!(pin_requested("true"));
        assert!(pin_requested("TRUE"));
        assert!(pin_requested(" 1 "));
        for off in ["0", "", "  ", "yes", "on", "banana", "2"] {
            assert!(!pin_requested(off), "{off:?} must NOT arm the pin");
        }

        // Production sampling is untouched when the pin is off, and the
        // per-leg default is preserved rather than flattened.
        assert_eq!(draft_sampling_for(false, 0.4), (Some(0.4), None));
        assert_eq!(draft_sampling_for(false, 0.3), (Some(0.3), None));

        // Pinned: greedy AND top_p closed, identical to the judge's pin, so
        // writer and ruler are deterministic under one switch.
        assert_eq!(draft_sampling_for(true, 0.4), (Some(0.0), Some(1.0)));
        assert_eq!(
            draft_sampling_for(true, 0.3),
            (Some(0.0), Some(1.0)),
            "the pin must not vary by leg — a per-leg pin is not a pin"
        );
    }

    #[test]
    fn shed_classifier_and_hint_parse_the_real_wire_shapes() {
        let seed_05 = "Inference error: Remote API returned 503 Service Unavailable: \
            {\"error\":{\"message\":\"local inference failed: Inference error: MTP inference \
            deadline exceeded after 300s (3560 tokens)\",\"type\":\"backend_error\",\"code\":null}}";
        assert!(looks_shed(seed_05), "the evidenced death is a shed");
        assert_eq!(
            shed_retry_hint_secs(seed_05),
            None,
            "no retry key — default backoff"
        );
        let admission = "Remote API returned 503 Service Unavailable: \
            {\"error\":\"local queue full\",\"reason\":\"local_queue_full\",\"retry_after_secs\":30}";
        assert!(looks_shed(admission));
        assert_eq!(shed_retry_hint_secs(admission), Some(30));
        let busy = "Remote API returned 503 Service Unavailable: \
            {\"error\":\"host busy\",\"retry_after\":14,\"queue_position\":3}";
        assert!(looks_shed(busy));
        assert_eq!(shed_retry_hint_secs(busy), Some(14));
        assert_eq!(
            shed_retry_hint_secs("retry_after_secs\":3600"),
            Some(300),
            "clamped"
        );
        assert!(!looks_shed("connection refused"));
        assert_eq!(shed_retry_hint_secs("connection refused"), None);
    }

    /// The evidenced seed-05 death — through the helper, with the
    /// DEFAULT backoff (the body carries no retry key) — recovers on
    /// the retry and answers. This is the exact text that killed the
    /// seed-05 flight.
    #[tokio::test]
    async fn the_evidenced_seed_05_death_is_a_shed_the_client_now_survives() {
        let seed_05 = "Inference error: Remote API returned 503 Service Unavailable: \
            {\"error\":{\"message\":\"local inference failed: Inference error: MTP inference \
            deadline exceeded after 300s (3560 tokens)\",\"type\":\"backend_error\",\"code\":null}}";
        let stub = Arc::new(ShedStub {
            fails: 1,
            error_text: seed_05.to_string(),
            attempts: Arc::new(Mutex::new(0)),
        });
        let request = CompletionRequest {
            prompt: "q".to_string(),
            ..Default::default()
        };
        let resp = complete_with_shed_retry(&*stub, &request, "draft")
            .await
            .unwrap();
        assert_eq!(resp.text, "the answer");
        assert_eq!(
            *stub.attempts.lock().unwrap(),
            2,
            "one shed + the answering call"
        );
    }

    /// THE 2026-08-27 DEATH, verbatim. `fast_context_size = 16384` left the
    /// round leg's ~96,000-char evidence budget unservable. The daemon
    /// reported it as a 503, `looks_shed` matched, and the client spent three
    /// backoffs on a prompt that could never fit — while the log line said
    /// "shed", which points diagnosis at LOAD instead of at config.toml.
    #[tokio::test]
    async fn a_prompt_that_cannot_fit_the_window_is_terminal_not_a_shed() {
        let measured = "Inference error: Remote API request returned 503 Service \
            Unavailable: {\"error\":{\"message\":\"local inference failed: Inference \
            error: Prompt too long: 21014 tokens already meets or exceeds the context \
            window of 16380. Shorten the conversation.\",\"type\":\"backend_error\"}}";

        // It genuinely DOES arrive in shed shape. That is precisely why the
        // second check has to exist — if this assertion ever flips, the
        // classifier changed underneath and this test is no longer the guard
        // it claims to be.
        assert!(
            looks_shed(measured),
            "the daemon really does report it as a 503"
        );
        assert!(looks_prompt_overflow(measured));

        let stub = Arc::new(ShedStub {
            fails: 99, // it would never stop failing, because it cannot
            error_text: measured.to_string(),
            attempts: Arc::new(Mutex::new(0)),
        });
        let request = CompletionRequest {
            prompt: "q".to_string(),
            ..Default::default()
        };
        let err = complete_with_shed_retry(&*stub, &request, "draft")
            .await
            .unwrap_err();
        assert!(
            err.contains("Prompt too long"),
            "surfaced raw, not substituted: {err}"
        );
        assert_eq!(
            *stub.attempts.lock().unwrap(),
            1,
            "ONE attempt — a deterministic failure must never be retried"
        );
    }

    /// The correction must not swallow real sheds. A deadline overrun says
    /// "exceeded" too, and it IS transient — retrying is the whole point.
    #[test]
    fn a_deadline_overrun_is_still_a_shed_and_not_a_prompt_overflow() {
        let deadline = "Inference error: Remote API returned 503 Service Unavailable: \
            {\"error\":{\"message\":\"local inference failed: Inference error: MTP \
            inference deadline exceeded after 300s (3560 tokens)\"}}";
        assert!(looks_shed(deadline));
        assert!(
            !looks_prompt_overflow(deadline),
            "'deadline exceeded' must not be read as a context-window overflow"
        );
    }

    /// The hint names the SLOT and the KEY, because the daemon's own message
    /// names neither and that is what cost a flight.
    #[test]
    fn the_overflow_hint_names_the_slot_and_the_config_key() {
        let fast = prompt_overflow_hint(Speed::Fast, DraftLeg::Round, 98_936);
        assert!(fast.contains("fast_context_size"), "{fast}");
        assert!(fast.contains("98936"), "{fast}");
        // The Slow legs are sized by a DIFFERENT key; naming the wrong one
        // sends the reader to edit a line that cannot help.
        let slow = prompt_overflow_hint(Speed::Slow, DraftLeg::Synthesis, 12);
        assert!(slow.contains("[models].context_size"), "{slow}");
        assert!(!slow.contains("fast_context_size"), "{slow}");
    }

    /// Sheds twice with a 1s hint, then succeeds: three attempts, the
    /// Retry-After hint honored, the success surfaced.
    #[tokio::test]
    async fn shed_twice_then_succeed_retries_with_the_hint_backoff() {
        let stub = Arc::new(ShedStub {
            fails: 2,
            error_text: "Remote API returned 503 Service Unavailable: \
                {\"error\":\"host busy\",\"retry_after\":1}"
                .to_string(),
            attempts: Arc::new(Mutex::new(0)),
        });
        let request = CompletionRequest {
            prompt: "q".to_string(),
            ..Default::default()
        };
        let resp = complete_with_shed_retry(&*stub, &request, "draft")
            .await
            .unwrap();
        assert_eq!(resp.text, "the answer");
        assert_eq!(
            *stub.attempts.lock().unwrap(),
            3,
            "two sheds + the answering call"
        );
    }

    /// Always-shed: attempts bounded at MAX_SHED_RETRIES + 1, the last
    /// honest error surfaced — never a silent substitution, never an
    /// unbounded stall.
    #[tokio::test]
    async fn always_shed_is_bounded_and_surfaces_the_last_error() {
        let stub = Arc::new(ShedStub {
            fails: usize::MAX,
            error_text: "Remote API returned 503 Service Unavailable: \
                {\"error\":\"host busy\",\"retry_after\":1}"
                .to_string(),
            attempts: Arc::new(Mutex::new(0)),
        });
        let request = CompletionRequest {
            prompt: "q".to_string(),
            ..Default::default()
        };
        let err = complete_with_shed_retry(&*stub, &request, "plan-subquestions")
            .await
            .unwrap_err();
        assert_eq!(*stub.attempts.lock().unwrap(), MAX_SHED_RETRIES + 1);
        assert!(
            err.contains("503"),
            "the honest error, not a substitution: {err}"
        );
    }

    /// A non-shed failure (connection refused) surfaces immediately —
    /// no retry, no stall; the seed-05 sibling errors keep their shape.
    #[tokio::test]
    async fn non_shed_error_surfaces_immediately_without_retry() {
        let stub = Arc::new(ShedStub {
            fails: usize::MAX,
            error_text: "connection refused".to_string(),
            attempts: Arc::new(Mutex::new(0)),
        });
        let request = CompletionRequest {
            prompt: "q".to_string(),
            ..Default::default()
        };
        let err = complete_with_shed_retry(&*stub, &request, "draft")
            .await
            .unwrap_err();
        assert_eq!(*stub.attempts.lock().unwrap(), 1, "no retry on a non-shed");
        // The honest wire shape, prefix included — exactly what the
        // evidence shows the loop client sees (seed-05: "Inference
        // error: Remote API returned 503 ...").
        assert_eq!(err, "Inference error: connection refused");
    }

    /// A minimal single-page PDF with a known text object — built
    /// with computed xref offsets so the fixture is deterministic,
    /// self-contained, and needs no vendored binary.
    fn minimal_pdf(text: &str) -> Vec<u8> {
        let content = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        let objs: Vec<String> = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), content),
        ];
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, body) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{}\nendobj\n", i + 1, body));
        }
        let xref_pos = out.len();
        out.push_str(&format!("xref\n0 {}\n", objs.len() + 1));
        out.push_str("0000000000 65535 f \n");
        for off in offsets {
            out.push_str(&format!("{off:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
            objs.len() + 1
        ));
        out.into_bytes()
    }

    /// RED `pdf_bytes_extract_to_text` (order drb1-t2, the PDF wall):
    /// PDF bytes extract to text through the port's PDF path — the
    /// SAME extractor (pdf-extract 0.7.12) the corpus ingest uses, so
    /// the logged flight's fetch-refused-as-binary scholarly PDFs
    /// (task 56: four exact-topic papers, `non-text payload`) become
    /// window-admissible content. A malformed PDF is a typed error,
    /// never a panic and never a window-poisoning payload.
    #[tokio::test]
    async fn pdf_bytes_extract_to_text() {
        let bytes = minimal_pdf("A Simple Approach to Analyzing Asymmetric First Price Auctions");
        let text = extract_pdf_bytes(&bytes)
            .await
            .expect("extraction succeeds");
        assert!(
            text.contains("Asymmetric"),
            "the paper's title text survives extraction: {text:?}"
        );
        assert!(
            text.contains("Auctions"),
            "the extraction is text, not glyph soup: {text:?}"
        );
        // The task-56 gold shape: the brocku title's words are present.
        assert!(text.to_lowercase().contains("first price auctions"));

        // A malformed PDF is a typed error (the panic guard holds —
        // the run never dies inside the fetch leg).
        let err = extract_pdf_bytes(b"%PDF-1.4 not actually a pdf")
            .await
            .expect_err("malformed bytes must error");
        assert!(!err.is_empty());
    }
}
