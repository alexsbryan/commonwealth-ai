// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn deep-research "<question>"` — run the THIN local-only research
//! loop (order deep-research-t1a) end to end through the shipped CLI.
//!
//! The verb implements the loop's `ResearchPort` (sovereign-core
//! `deep_research::estate`) over the real estate (corpus-engine indexes),
//! the real network (web search — DuckDuckGo always as the zero-config
//! fallback, Tavily when the operator's key is present — plus
//! fetch-and-extract), and the daemon's OpenAI-compatible surface
//! (embed + draft, with the URL allowlist constraint on every draft ask).
//! The loop itself lives in sovereign-core (`deep_research::run`);
//! nothing in this file re-implements a loop step.
//!
//! Custody is stamped here, by code, never by a model (R-2/R-6): estate
//! hits carry `personal` (a local corpus is the operator's own data), web
//! hits carry `public-web`. The loop's gate refuses unknown provenance.
//!
//! `--backend mock --mock-deck DIR` (the P5 drill surface): the port's
//! search/fetch legs are served from the deck directory (`deck.toml` +
//! body files, the deep-research search gym's format) instead of the
//! network — the loop's `web_backend` is the mock's closed-set id, so a
//! run can be flown against a planted source with the real daemon still
//! doing the drafting (`MockDraftSurface::Delegated`). Additive: the
//! default path is unchanged.
//!
//! `--resume DIR` (order deep-research-t3a): an interrupted run
//! restores its state from `<DIR>/checkpoint.json` and continues at the
//! next round — ledger continuity included. The checkpoint's frozen
//! config is the identity: flags the operator did NOT pass inherit the
//! checkpoint's values (bare `--resume DIR` is the canonical shape), and
//! every explicitly-passed flag is verified against the frozen config
//! flag-by-flag (a conflicting one refuses, naming the flag); the
//! backend identity comes from the launch sidecar
//! (`resume-input.json`). The verb also closes every run by ingesting
//! its fetched evidence into `dr-estate-<run_id>` — the local cache a
//! later run's `--corpora` reads before the web leg.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use oicp_client::RemoteApiProvider;
use sovereign_contracts::types::CompletionRequest;
use sovereign_contracts::types::CompletionResponse;
use sovereign_contracts::types::Speed;
use sovereign_core::deep_research::acquisition::web_hit_relevance;
use sovereign_core::deep_research::estate::{
    estate_snippet, read_staged_alignment, AlignmentDecision, EstateListing, PortHit, ResearchPort,
};
use sovereign_core::deep_research::gym::{
    CorpusSurface, Deck, MockBackendImpl, MockDraftSurface, ProviderEmbed,
};
use sovereign_core::deep_research::icd::ICD_VERSION;
use sovereign_core::deep_research::icd::{CorpusEntry, EvidenceWindow, Plan, Survey, VerdictSet};
use sovereign_core::deep_research::render::render_race;
use sovereign_core::deep_research::{
    read_checkpoint, resume, run, RunConfig, RunOutcome, SearchSource,
};
use sovereign_core::egress::{ConsentGrant, EgressPayload};
use sovereign_core::oicp::ShardingPrivacy;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::Custody;

use corpus_engine::index::{CorpusIndex, InsertChunk};

/// The canonical index directory (the estate).
fn indexes_dir() -> PathBuf {
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
        || std::fs::metadata(dir.join("_corpus_meta.json")).is_ok()
}

/// The chunk count from `_corpus_meta.json` when present; 0 when the
/// file is absent or unreadable — a listed-but-uncounted corpus is
/// still searchable. The engine's meta schema v3 writes the count as
/// `chunks_expected` (null in some pipelines) with `next_chunk_id`
/// carrying the committed chunk count — measured: the demo re-ask's
/// survey listed apollo11-evidence with chunks_count 0 because the
/// read looked for the schema-v2 `chunk_count` key.
fn corpus_chunk_count(dir: &std::path::Path) -> i64 {
    std::fs::read_to_string(dir.join("_corpus_meta.json"))
        .ok()
        .and_then(|meta| serde_json::from_str::<serde_json::Value>(&meta).ok())
        .and_then(|v| {
            v["chunks_expected"]
                .as_i64()
                .or_else(|| v["next_chunk_id"].as_i64())
        })
        .unwrap_or(0)
}

/// The estate snippet: center on the deepest first-occurrence query
/// term instead of taking the page prefix. Long pages lead with nav
/// chrome, and the prefix-left window was measured wrong in the demo's
/// re-ask (dr-1786727099): the Smithsonian timeline chunk's 240-char
/// prefix ended at the donate blurb, the round-0 draft anchored its
/// claims on that blurb, and the gap-derived web queries drifted to
/// museum-grant pages. Prefix fallback when no term occurs (short
/// chunks, non-lexical matches). `to_ascii_lowercase` keeps byte
/// offsets valid for slicing (case folding can change length).
///
/// Two calibrations were measured by the watched test:
///   - function words (when/were/...) are filtered — the first pass
///     anchored on "were" at the sentence end and cut "July 20, 1969";
///   - the window leads 200 chars before the anchor so the answer
///     sentence's context survives the cut.
/// The port implementation for the CLI: real estate, real network, real
/// daemon. All inference goes through one `RemoteApiProvider` against the
/// local daemon's `/v1` surface — the loop never touches a frontier.
struct CliResearchPort {
    provider: Arc<dyn InferenceProvider>,
    client: reqwest::Client,
    orchestrator: sovereign_tools_base::web::search::SearchOrchestrator,
    /// The run's typed consent grant (order deep-research-t2a): the
    /// port carries it to the egress boundary for every web-leg
    /// dispatch. `None` is default-deny — the web leg refuses
    /// non-public-web payloads (the run's machine-formed queries).
    consent: Option<ConsentGrant>,
    /// True iff the operator's Tavily key was present at port
    /// construction. The ONE source of the loop's web-backend default
    /// (`default_web_backend`); no second read of the env var exists.
    tavily_keyed: bool,
    indexes: std::path::PathBuf,
    daemon_endpoint: String,
}

impl CliResearchPort {
    fn new(provider: Arc<dyn InferenceProvider>, consent: Option<ConsentGrant>) -> Self {
        // The boundary's search-client factory — the ONE construction
        // site for clients that carry query egress (F26 census:
        // everything else in this file is LocalDaemon).
        let client =
            sovereign_core::egress::search_client().expect("egress boundary search client build");
        let mut registry = sovereign_tools_base::web::search::WebSearchRegistry::new();
        // DuckDuckGo is the zero-config fallback — always registered
        // (the same fallback-first shape the desktop uses).
        registry.register(Arc::new(
            sovereign_tools_base::web::search::DuckDuckGoBackendImpl::new(),
        ));
        // Tavily rides on the operator's key when present. The read is
        // the house canonical (sovereign-contracts::rebrand::svrnmesh_env):
        // SVRNMESH_TAVILY_API_KEY preferred, the legacy
        // SOVEREIGN_TAVILY_API_KEY spelling bridged at CLI startup.
        // Presence is logged; the value never is. The read is declared
        // in quality/env-flags.toml (env-gate's registry).
        let tavily_key = sovereign_contracts::rebrand::svrnmesh_env("TAVILY_API_KEY")
            .and_then(|v| v.into_string().ok())
            .filter(|s| !s.is_empty());
        let tavily_keyed = tavily_key.is_some();
        if let Some(key) = &tavily_key {
            registry.register(Arc::new(
                sovereign_tools_base::web::search::TavilyBackendImpl::new(key.clone()),
            ));
        }
        eprintln!(
            "deep-research: web backends: tavily {}, duckduckgo (fallback)",
            if tavily_keyed { "keyed" } else { "absent" }
        );
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
        CliResearchPort {
            provider,
            client,
            orchestrator,
            consent,
            tavily_keyed,
            indexes: indexes_dir(),
            daemon_endpoint,
        }
    }

    /// The loop's web backend when the operator named none: the keyed
    /// Tavily when a key is present (the house prefer list puts tavily
    /// before duckduckgo — "best for citation-heavy synthesis"),
    /// DuckDuckGo otherwise. One decider, one name: the key presence
    /// decided in `new` is the only source of this choice.
    fn default_web_backend(&self) -> &'static str {
        if self.tavily_keyed {
            "tavily"
        } else {
            "duckduckgo"
        }
    }
}

#[async_trait::async_trait]
impl ResearchPort for CliResearchPort {
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
        sovereign_core::egress::verify(
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
        sovereign_core::egress::verify(
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
        if sovereign_core::deep_research::fetch::source_type_of(url)
            == sovereign_core::deep_research::icd::SourceType::Pdf
        {
            return fetch_pdf_text(&self.client, url)
                .await
                .map_err(|e| format!("fetch {url}: {e}"));
        }
        sovereign_tools_base::web::extract::fetch_and_extract(&self.client, url)
            .await
            .map_err(|e| format!("fetch {url}: {e}"))
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
        prompt: &str,
        system_message: Option<&str>,
        allowed_urls: &[String],
    ) -> Result<String, String> {
        let resp = complete_with_shed_retry(
            &*self.provider,
            &CompletionRequest {
                prompt: prompt.to_string(),
                system_message: system_message.map(|s| s.to_string()),
                preferred_speed: Speed::Slow,
                max_tokens: None,
                temperature: Some(0.4),
                structured_output: None,
                think_budget: None,
                url_allowlist: Some(allowed_urls.to_vec()),
                ..Default::default()
            },
            "draft",
        )
        .await
        .map_err(|e| format!("draft ask: {e}"))?;
        Ok(resp.text)
    }

    async fn plan_subquestions(&self, question: &str) -> Result<Vec<String>, String> {
        // t1d fix 2 (breadth): the acquisition frontier — a constrained
        // draft asking for the question's decomposition, one sub-
        // question per line. Same inference leg as the drafts
        // (Speed::Slow, temperature 0.4) with NO url allowlist: the
        // frontier is a question list, not report content, and must not
        // cite. Lines are parsed deterministically (marker-stripped,
        // deduped, capped at the shared FRONTIER_MAX).
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
        let prompt = format!(
            "Decompose the research question into sub-questions that a web search could answer. \
             For each sub-question, name the specific measure or statistic it implies — an index, \
             a ratio, a share, a rate, a count, a median, a price, a percentage change — and the \
             entities it involves (cities, years), so a search for the data can retrieve it. If the \
             question implies specific numbers, name them. One sub-question per line, no citations, \
             no numbering, no commentary.\n\nQuestion: {question}"
        );
        let resp = complete_with_shed_retry(
            &*self.provider,
            &CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Slow,
                max_tokens: None,
                temperature: Some(0.4),
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
            if out.len() >= sovereign_core::deep_research::estate::FRONTIER_MAX {
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
/// `sovereign_core::deep_research::fetch::CHUNK_CONTENT_CAP` — the
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
            .take(sovereign_core::deep_research::fetch::CHUNK_CONTENT_CAP)
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
        .take(sovereign_core::deep_research::fetch::CHUNK_CONTENT_CAP)
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
                if attempt >= MAX_SHED_RETRIES || !looks_shed(&text) {
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

/// The ONE port construction (order deep-research-t3a): `auto` — the
/// real network port; `mock` — the deck's search/fetch surface with
/// drafts delegated to the real port, and (when the search source is
/// corpus) the estate's REAL indexes as the acquisition surface. Used
/// by both the fresh launch and the resume — one implementation, one
/// decider, never two constructions that can drift.
async fn build_port(
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
        let real = Arc::new(CliResearchPort::new(provider.clone(), consent.clone()));
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
        let real = Arc::new(CliResearchPort::new(provider.clone(), consent));
        let web_backend = real.default_web_backend().to_string();
        Ok((real, web_backend))
    }
}

/// The launch-sidecar (order deep-research-t3a): the run's backend
/// identity, written by the verb into the run dir before launch and
/// read back on `--resume` — the operator's `--backend`/`--mock-deck`
/// flags are verified against it flag-by-flag, never silently
/// substituted.
#[derive(serde::Serialize, serde::Deserialize)]
struct ResumeInput {
    icd: String,
    version: u32,
    run_id: String,
    backend: String,
    #[serde(default)]
    mock_deck_dir: Option<String>,
}

/// `svrn deep-research "<question>" [--run-dir DIR] [--max-rounds N]
/// [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N]
/// [--fetch N] [--backend auto|mock] [--mock-deck DIR]
/// [--search-source mock|corpus|web] [--consent public-web|peer|personal]
/// [--resume DIR]`
pub async fn cmd_deep_research(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: svrn deep-research \"<question>\" [--run-dir DIR] [--max-rounds N] \
             [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N] [--fetch N] \
             [--backend auto|mock] [--mock-deck DIR] [--search-source mock|corpus|web] \
             [--consent public-web|peer|personal] [--resume DIR]"
        );
        return 0;
    }
    let mut question: Option<String> = None;
    let mut run_dir = std::env::temp_dir().join("deep-research-runs");
    let mut run_dir_explicit = false;
    let mut resume_dir: Option<PathBuf> = None;
    let mut max_rounds = 3u32;
    let mut corpora: Vec<String> = Vec::new();
    // drb1-t1: the admission thresholds default from the ONE decider
    // (acquisition::{DEFAULT_CODE_SET_K, DEFAULT_EPS_QUOTA}) — the
    // charter, the flags, and the replay harness read the same consts.
    let mut code_set_k = sovereign_core::deep_research::acquisition::DEFAULT_CODE_SET_K;
    let mut eps_quota = sovereign_core::deep_research::acquisition::DEFAULT_EPS_QUOTA;
    let mut search_allowance = 4u32;
    let mut fetch_allowance = 4u32;
    // Which flags the operator ACTUALLY passed (order deep-research-t3a):
    // a `--resume` inherits the checkpoint's frozen values for flags that
    // were NOT passed — only explicitly-passed flags are verified against
    // the frozen config, and a conflicting one refuses, naming it. The
    // fresh-launch path never reads these.
    let mut max_rounds_explicit = false;
    let mut corpora_explicit = false;
    let mut code_set_k_explicit = false;
    let mut eps_quota_explicit = false;
    let mut search_allowance_explicit = false;
    let mut fetch_allowance_explicit = false;
    let mut search_source_explicit = false;
    let mut backend_explicit = false;
    let mut mock_deck_explicit = false;
    // The P5 drill surface (additive; default `auto` = the real
    // network). `--backend mock` serves search/fetch from the deck
    // directory, drafts via the real daemon.
    let mut backend = "auto".to_string();
    let mut mock_deck_dir: Option<PathBuf> = None;
    // The acquisition search source (t1g rung 2; rung 3 = web, order
    // deep-research-t2a): a closed set, decided once here — `mock`
    // (default), `corpus`, or `web`.
    let mut search_source = SearchSource::Mock;
    // The run-scoped consent grant's release floor (order
    // deep-research-t2a): `None` = default-deny — the web leg refuses
    // non-public-web payloads without a grant. The grant itself is
    // built once the run id exists (frozen into the charter, FR-3).
    let mut consent_floor: Option<Custody> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--backend" => {
                i += 1;
                backend = args.get(i).cloned().unwrap_or_default();
                backend_explicit = true;
            }
            "--search-source" => {
                i += 1;
                match SearchSource::parse(args.get(i).map(String::as_str).unwrap_or_default()) {
                    Some(s) => search_source = s,
                    None => {
                        eprintln!(
                            "deep-research: unknown search source {:?} — the closed set is \
                             mock | corpus | web",
                            args.get(i).map(String::as_str).unwrap_or_default()
                        );
                        return 1;
                    }
                }
                search_source_explicit = true;
            }
            "--consent" => {
                i += 1;
                let s = args.get(i).map(String::as_str).unwrap_or_default();
                match Custody::parse_wire(s) {
                    Some(c) if c != Custody::Unknown => consent_floor = Some(c),
                    _ => {
                        eprintln!(
                            "deep-research: unknown consent class {:?} — the closed set is \
                             public-web | peer | personal",
                            s
                        );
                        return 1;
                    }
                }
            }
            "--mock-deck" => {
                i += 1;
                mock_deck_dir = Some(PathBuf::from(args.get(i).cloned().unwrap_or_default()));
                mock_deck_explicit = true;
            }
            "--run-dir" => {
                i += 1;
                run_dir = PathBuf::from(args.get(i).cloned().unwrap_or_default());
                run_dir_explicit = true;
            }
            // T3a: resume an interrupted run from its run dir. The
            // checkpoint's frozen config is the identity — the flags
            // below are verified against it, not applied to it.
            "--resume" => {
                i += 1;
                let p = PathBuf::from(args.get(i).cloned().unwrap_or_default());
                if p.as_os_str().is_empty() {
                    eprintln!("deep-research: --resume requires a run dir argument (--resume DIR)");
                    return 1;
                }
                resume_dir = Some(p);
            }
            "--max-rounds" => {
                i += 1;
                max_rounds = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(max_rounds);
                max_rounds_explicit = true;
            }
            "--corpora" => {
                i += 1;
                corpora = args
                    .get(i)
                    .map(|s| {
                        s.split(',')
                            .map(|c| c.trim().to_string())
                            .filter(|c| !c.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                corpora_explicit = true;
            }
            "--code-set-k" => {
                i += 1;
                code_set_k = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(code_set_k);
                code_set_k_explicit = true;
            }
            "--eps-quota" => {
                i += 1;
                eps_quota = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(eps_quota);
                eps_quota_explicit = true;
            }
            "--search" => {
                i += 1;
                search_allowance = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(search_allowance);
                search_allowance_explicit = true;
            }
            "--fetch" => {
                i += 1;
                fetch_allowance = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(fetch_allowance);
                fetch_allowance_explicit = true;
            }
            s if question.is_none() => question = Some(s.to_string()),
            _ => {
                eprintln!("Usage: svrn deep-research \"<question>\" [--run-dir DIR] [--max-rounds N] [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N] [--fetch N] [--backend auto|mock] [--mock-deck DIR] [--search-source mock|corpus|web] [--consent public-web|peer|personal] [--resume DIR]");
                return 1;
            }
        }
        i += 1;
    }
    // The backend is a closed set: a misspelled or unregistered backend
    // must refuse, never silently route (§18.3 — the mock itself
    // refuses any other backend id).
    if backend != "auto" && backend != "mock" {
        eprintln!("deep-research: unknown backend {backend:?} — the closed set is auto | mock");
        return 1;
    }
    if backend == "mock" && mock_deck_dir.is_none() {
        eprintln!("deep-research: --backend mock requires --mock-deck DIR");
        return 1;
    }
    if backend != "mock" && mock_deck_dir.is_some() {
        eprintln!("deep-research: --mock-deck requires --backend mock (no silent substitution)");
        return 1;
    }
    // The corpus source acquires from the estate's corpus-search
    // surface: a run that asks for the corpus source without naming
    // any corpus would search nothing — refused loudly, never a
    // silent empty.
    if search_source == SearchSource::Corpus && corpora.is_empty() {
        eprintln!("deep-research: --search-source corpus requires --corpora id1,id2");
        return 1;
    }
    // A fresh launch needs a question; a resume refuses one (the
    // checkpoint's question is the frozen identity). Naming both a run
    // dir and a resume dir would name two run dirs — refused.
    if question.is_none() && resume_dir.is_none() {
        eprintln!(
            "Usage: svrn deep-research \"<question>\" [--run-dir DIR] [--max-rounds N] \
             [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N] [--fetch N] \
             [--backend auto|mock] [--mock-deck DIR] [--search-source mock|corpus|web] \
             [--consent public-web|peer|personal] [--resume DIR]"
        );
        return 1;
    }
    if resume_dir.is_some() && run_dir_explicit {
        eprintln!(
            "deep-research: --run-dir cannot be combined with --resume — the resumed run dir \
             is the --resume argument"
        );
        return 1;
    }

    // Daemon + models: the loop is local-only, but it still needs the
    // local daemon's embed + draft surface.
    let cfg = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("read ~/.svrnmesh/config.toml: {e}");
            return 1;
        }
    };
    let port = cfg.daemon.client_port;
    let endpoint = format!("http://localhost:{port}/v1");
    let draft_model = match cfg.models.primary.file_stem().and_then(|s| s.to_str()) {
        Some(m) => m.to_string(),
        None => {
            eprintln!("deep-research: SetupConfig.models.primary has no filename stem (the draft model id)");
            return 1;
        }
    };
    let embed_model = match cfg.models.embed.file_stem().and_then(|s| s.to_str()) {
        Some(m) => m.to_string(),
        None => {
            eprintln!(
                "deep-research: SetupConfig.models.embed has no filename stem (the embed model id)"
            );
            return 1;
        }
    };

    // T3a: the resume gate. The checkpoint's frozen config is the
    // identity; the operator's flags are verified against it
    // flag-by-flag (each mismatch refuses, naming the flag). The
    // question, consent grant, and allowances come from the
    // checkpoint — nothing is re-decided.
    if let Some(resume_dir) = resume_dir {
        // Only EXPLICITLY-passed flags are verified against the frozen
        // config (the checkpoint's values are the default for flags the
        // operator did not pass — bare `--resume DIR` inherits the whole
        // config). A conflicting explicit flag refuses below, naming it.
        return match resume_run_inner(
            &resume_dir,
            &draft_model,
            &embed_model,
            &endpoint,
            max_rounds_explicit.then_some(max_rounds),
            corpora_explicit.then_some(corpora.as_slice()),
            code_set_k_explicit.then_some(code_set_k),
            eps_quota_explicit.then_some(eps_quota),
            search_allowance_explicit.then_some(search_allowance),
            fetch_allowance_explicit.then_some(fetch_allowance),
            search_source_explicit.then_some(search_source),
            consent_floor,
            backend_explicit.then_some(backend.as_str()),
            mock_deck_explicit.then_some(mock_deck_dir.as_deref()),
            question.as_deref(),
        )
        .await
        {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("deep-research: --resume refused: {e}");
                1
            }
        };
    }

    // A fresh launch was validated to carry a question above; the
    // resume branch already returned for the resume shape.
    let question = question.expect("a fresh launch has a question (validated above)");

    let run_id = format!("dr-{}", now_unix());
    let run_dir = run_dir.join(&run_id);

    // The run-scoped consent grant (order deep-research-t2a): minted
    // once, here, from the operator's `--consent` class — then frozen
    // (FR-3) into both the port (the egress boundary's check) and the
    // RunConfig (the manifest record). Default-deny: no flag, no
    // grant, non-public-web egress refuses.
    let consent: Option<ConsentGrant> = consent_floor.map(|release_floor| ConsentGrant {
        run_id: run_id.clone(),
        granted_at_unix: now_unix(),
        release_floor,
    });

    let provider: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(&endpoint, None, &draft_model, 8192));
    // The ONE port construction (shared with the resume path — see
    // build_port): `--backend mock` serves search/fetch from the deck
    // directory with drafting delegated to the real port (the daemon);
    // the corpus source attaches the estate's real indexes.
    let (port, web_backend) = match build_port(
        &backend,
        mock_deck_dir.as_deref(),
        search_source,
        &corpora,
        provider.clone(),
        consent.clone(),
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("deep-research: {e}");
            return 1;
        }
    };

    eprintln!("deep-research: run {run_id} — {question}");
    eprintln!("deep-research: run dir {}", run_dir.display());
    eprintln!("deep-research: web backend {web_backend}");
    eprintln!("deep-research: search source {}", search_source.as_str());
    if backend == "mock" {
        eprintln!(
            "deep-research: mock deck {} (search/fetch served from the deck; drafts delegated)",
            mock_deck_dir.as_deref().expect("validated above").display()
        );
    }
    if search_source == SearchSource::Corpus {
        eprintln!("deep-research: corpus source over: {}", corpora.join(", "));
    }
    eprintln!("deep-research: daemon {endpoint} (draft {draft_model}, embed {embed_model})");

    let config = RunConfig {
        run_id: run_id.clone(),
        question: question.clone(),
        seed_id: None,
        run_dir: run_dir.clone(),
        max_rounds,
        code_set_k,
        eps_quota,
        // drb1-t2: the content admission floors — one decider, the
        // acquisition consts (the charter records them; no CLI flag
        // until the seat asks for one).
        content_coverage_floor:
            sovereign_core::deep_research::acquisition::DEFAULT_CONTENT_COVERAGE_FLOOR,
        prose_line_floor: sovereign_core::deep_research::acquisition::DEFAULT_PROSE_LINE_FLOOR,
        evidence_window_max_chunks: 20,
        estate_corpus_ids: corpora,
        web_backend,
        search_source,
        web_search_allowance: search_allowance,
        web_fetch_allowance: fetch_allowance,
        posture: ShardingPrivacy::LocalOnly,
        consent,
        max_rounds_override: None,
        max_search_override: None,
        max_fetch_override: None,
    };

    // The launch sidecar (order deep-research-t3a): the run's backend
    // identity, recorded BEFORE launch so a later `--resume` verifies
    // the operator's flags against it flag-by-flag (never a silent
    // substitution). Written by the verb, read by the verb.
    if let Err(e) = std::fs::create_dir_all(&run_dir) {
        eprintln!("deep-research: create run dir {}: {e}", run_dir.display());
        return 1;
    }
    let sidecar = ResumeInput {
        icd: "resume_input".to_string(),
        version: ICD_VERSION,
        run_id: run_id.clone(),
        backend: backend.clone(),
        mock_deck_dir: mock_deck_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
    };
    let sidecar_json = match serde_json::to_string_pretty(&sidecar) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("deep-research: resume sidecar serialize: {e}");
            return 1;
        }
    };
    if let Err(e) = std::fs::write(run_dir.join("resume-input.json"), sidecar_json) {
        eprintln!("deep-research: resume sidecar write: {e}");
        return 1;
    }

    let mut outcome = match run(
        config,
        port,
        provider.clone(),
        Arc::new(AtomicBool::new(false)),
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("deep-research: run failed: {e}");
            return 1;
        }
    };
    // The run-close estate ingest (scene 6 of the dr-journey bar): the
    // run's fetched evidence lands in `dr-estate-<run_id>`, stamped
    // indexes-built and ingestion-complete — retrieval-visible without
    // a manual ritual. A failed ingest fails the verb loudly; it never
    // passes in silence.
    if let Err(e) = ingest_run_estate(&mut outcome, &provider, &embed_model).await {
        eprintln!("deep-research: estate ingest failed: {e}");
        return 1;
    }
    print_summary(&outcome);
    // T6b pre-window slice (pre-registered): the clean RACE article
    // page, written post-flight beside report.md. A write failure
    // fails the verb loudly — the deliverable is missing.
    if let Err(e) = write_race_render(&outcome.report_path) {
        eprintln!("deep-research: {e}");
        return 1;
    }
    0
}

/// `--resume DIR` (order deep-research-t3a): restore an interrupted
/// run's state from its checkpoint and continue at the next round.
/// Every refusal is typed and names what was withheld:
///   - the checkpoint envelope (read_checkpoint — malformed /
///     inconsistent / foreign),
///   - a passed question (the checkpoint's question is the frozen
///     identity),
///   - the launch sidecar (resume-input.json — unreadable, malformed,
///     or belonging to another run),
///   - each EXPLICITLY-passed flag that conflicts with the checkpoint's
///     frozen config (--max-rounds, --search, --fetch, --code-set-k,
///     --eps-quota, --corpora, --search-source, --consent) or the
///     sidecar (--backend, --mock-deck). A flag the operator did NOT
///     pass inherits the checkpoint's frozen value — bare `--resume
///     DIR` is the canonical resume shape.
/// The core's resume_start adds the charter-hash, config-identity,
/// ledger-continuity, and live-lock gates behind this surface.
async fn resume_run_inner(
    resume_dir: &Path,
    draft_model: &str,
    embed_model: &str,
    endpoint: &str,
    max_rounds: Option<u32>,
    corpora: Option<&[String]>,
    code_set_k: Option<usize>,
    eps_quota: Option<f64>,
    search_allowance: Option<u32>,
    fetch_allowance: Option<u32>,
    search_source: Option<SearchSource>,
    consent_floor: Option<Custody>,
    backend: Option<&str>,
    mock_deck_dir: Option<Option<&Path>>,
    question: Option<&str>,
) -> Result<(), String> {
    let cp = read_checkpoint(resume_dir)?;
    let mut c = cp.config.clone();
    // The operator's named dir IS the state home (order
    // deep-research-t3a, measured red: the core anchored on
    // cp.config.run_dir — the LAUNCH dir — so a `--resume` of a COPY
    // resumed and closed the ORIGINAL run, and a tampered copy's
    // deadbeef checkpoint was never even read). `run_dir` is a
    // location, not an identity field (the charter — the identity —
    // never included it; config_mismatch does not compare it): the
    // checkpoint records where the run LAUNCHED, `--resume <dir>`
    // anchors where it CONTINUES. All state reads/writes below go to
    // the named dir.
    c.run_dir = resume_dir.to_path_buf();

    if let Some(q) = question {
        return Err(format!(
            "a question argument ({q:?}) substitutes for the checkpoint's frozen question — \
             resume without one"
        ));
    }

    // The launch sidecar: the run's backend identity. A run launched
    // before the sidecar existed has no verifiable identity — refused,
    // never assumed.
    let sidecar_path = resume_dir.join("resume-input.json");
    let raw = std::fs::read_to_string(&sidecar_path).map_err(|e| {
        format!(
            "{sidecar_path:?} is unreadable ({e}) — the run's backend identity cannot be \
             verified (a run launched before the sidecar existed cannot be resumed)"
        )
    })?;
    let sidecar: ResumeInput =
        serde_json::from_str(&raw).map_err(|e| format!("{sidecar_path:?} is malformed: {e}"))?;
    if sidecar.icd != "resume_input" || sidecar.version != ICD_VERSION {
        return Err(format!(
            "{sidecar_path:?} is not a resume sidecar (icd {:?}, version {}) — foreign or \
             tampered",
            sidecar.icd, sidecar.version
        ));
    }
    if sidecar.run_id != c.run_id {
        return Err(format!(
            "{sidecar_path:?} belongs to run {} but the checkpoint is run {} — mismatched run \
             dir",
            sidecar.run_id, c.run_id
        ));
    }

    // Flag-by-flag identity — an EXPLICITLY-passed flag is verified
    // against the frozen config; a flag the operator did NOT pass
    // inherits the checkpoint's value (bare `--resume DIR` resumes with
    // the exact state the run was interrupted with). Each refusal names
    // the flag AND the checkpoint's value, so the operator sees exactly
    // what to drop.
    if let Some(max_rounds) = max_rounds {
        if max_rounds != c.max_rounds {
            return Err(format!(
                "--max-rounds {max_rounds} differs from the checkpoint's {} — a resume keeps \
                 the frozen config",
                c.max_rounds
            ));
        }
    }
    if let Some(search_allowance) = search_allowance {
        if search_allowance != c.web_search_allowance {
            return Err(format!(
                "--search {search_allowance} differs from the checkpoint's {} — a resume keeps \
                 the frozen budget",
                c.web_search_allowance
            ));
        }
    }
    if let Some(fetch_allowance) = fetch_allowance {
        if fetch_allowance != c.web_fetch_allowance {
            return Err(format!(
                "--fetch {fetch_allowance} differs from the checkpoint's {} — a resume keeps \
                 the frozen budget",
                c.web_fetch_allowance
            ));
        }
    }
    if let Some(code_set_k) = code_set_k {
        if code_set_k != c.code_set_k {
            return Err(format!(
                "--code-set-k {code_set_k} differs from the checkpoint's {} — a resume keeps \
                 the frozen config",
                c.code_set_k
            ));
        }
    }
    if let Some(eps_quota) = eps_quota {
        if eps_quota != c.eps_quota {
            return Err(format!(
                "--eps-quota {eps_quota} differs from the checkpoint's {} — a resume keeps the \
                 frozen config",
                c.eps_quota
            ));
        }
    }
    if let Some(corpora) = corpora {
        if corpora != c.estate_corpus_ids.as_slice() {
            return Err(format!(
                "--corpora {} differs from the checkpoint's {} — a resume keeps the frozen \
                 corpus set",
                corpora.join(","),
                c.estate_corpus_ids.join(",")
            ));
        }
    }
    if let Some(search_source) = search_source {
        if search_source != c.search_source {
            return Err(format!(
                "--search-source {} differs from the checkpoint's {} — a resume keeps the \
                 frozen source",
                search_source.as_str(),
                c.search_source.as_str()
            ));
        }
    }
    if let Some(backend) = backend {
        if backend != sidecar.backend {
            return Err(format!(
                "--backend {backend} differs from the run's recorded {} — the backend is part \
                 of the run's identity",
                sidecar.backend
            ));
        }
    }
    match mock_deck_dir {
        Some(Some(given)) => match sidecar.mock_deck_dir.as_deref() {
            Some(recorded) if given.to_string_lossy() != recorded => {
                return Err(format!(
                    "--mock-deck {} differs from the run's recorded {recorded} — the deck is \
                     part of the run's identity",
                    given.display()
                ));
            }
            None => {
                return Err(
                    "--mock-deck was given but the run's sidecar records no deck — the run did \
                     not launch from a mock deck"
                        .to_string(),
                );
            }
            _ => {}
        },
        // Omitted: the sidecar's recorded deck IS the identity — the
        // port is rebuilt from it below, never from the operator's flags.
        _ => {}
    }
    // The consent grant is frozen in the checkpoint (FR-3): a
    // contradicting flag refuses; an omitted flag keeps the grant.
    match (consent_floor, &c.consent) {
        (Some(f), Some(g)) if f != g.release_floor => {
            return Err(format!(
                "--consent {} differs from the checkpoint's frozen {} — the grant is part of \
                 the run's identity",
                f.as_str(),
                g.release_floor.as_str()
            ));
        }
        (Some(_), None) => {
            return Err(
                "--consent was given but the checkpoint's run has no consent grant — resume \
                 without it"
                    .to_string(),
            );
        }
        _ => {}
    }

    let run_id = c.run_id.clone();
    eprintln!(
        "deep-research: resume {run_id} — continuing at round {}",
        cp.written_after_round + 1
    );
    eprintln!("deep-research: run dir {}", resume_dir.display());
    eprintln!("deep-research: question {}", c.question);
    eprintln!("deep-research: web backend {}", c.web_backend);
    if sidecar.backend == "mock" {
        eprintln!(
            "deep-research: mock deck {} (search/fetch served from the deck; drafts delegated)",
            sidecar.mock_deck_dir.as_deref().unwrap_or("?")
        );
    }
    if c.search_source == SearchSource::Corpus {
        eprintln!(
            "deep-research: corpus source over: {}",
            c.estate_corpus_ids.join(", ")
        );
    }
    eprintln!("deep-research: daemon {endpoint} (draft {draft_model}, embed {embed_model})");

    let provider: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(endpoint, None, draft_model, 8192));
    // The port is rebuilt from the SIDECAR's identity + the
    // checkpoint's config — never from the operator's flags (those
    // were verified equal above).
    let (port, _web_backend) = build_port(
        &sidecar.backend,
        sidecar.mock_deck_dir.as_deref().map(Path::new),
        c.search_source,
        &c.estate_corpus_ids,
        provider.clone(),
        c.consent.clone(),
    )
    .await?;
    let mut outcome = resume(c, port, provider.clone(), Arc::new(AtomicBool::new(false)))
        .await
        .map_err(|e| format!("resume failed: {e}"))?;
    if let Err(e) = ingest_run_estate(&mut outcome, &provider, embed_model).await {
        return Err(format!("estate ingest failed: {e}"));
    }
    print_summary(&outcome);
    write_race_render(&outcome.report_path)?;
    Ok(())
}

/// The run-close estate ingest (order deep-research-t3a — scene 6 of
/// the dr-journey bar, the local cache): every source the run
/// actually fetched (the round-1 survey's estate hits + every
/// evidence-window's chunks, deduped by source url) is ingested into
/// the run's estate corpus `dr-estate-<run_id>`, stamped
/// indexes-built and ingestion-complete (the two stamps listing AND
/// retrieval check — no manual ritual), and stamped `ingested_into`
/// on the manifest's fetched sources. A later run's `--corpora
/// dr-estate-<run_id>` reads the corpus BEFORE the web leg and cites
/// `estate:dr-estate-<run_id>:` locators — the cache that means we do
/// not always rebuild from web search.
async fn ingest_run_estate(
    outcome: &mut RunOutcome,
    provider: &Arc<dyn InferenceProvider>,
    embed_model: &str,
) -> Result<(), String> {
    let corpus_id = format!("dr-estate-{}", outcome.manifest.run_id);
    let corpus_dir = indexes_dir().join(&corpus_id);
    if corpus_dir.exists() {
        eprintln!("deep-research: estate corpus {corpus_id} already exists — skip (idempotent)");
        return Ok(());
    }
    let run_dir = outcome
        .report_path
        .parent()
        .ok_or_else(|| "the run's report path has no parent (no run dir)".to_string())?
        .to_path_buf();

    // Collect the run's evidence: window chunks + survey hits,
    // deduped by source url (survey first — its chunks carry the
    // estate locators the windows repeat).
    let mut collected: Vec<(String, Option<String>, String)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let survey_path = run_dir.join("survey-1.json");
    if let Ok(raw) = std::fs::read_to_string(&survey_path) {
        if let Ok(survey) = serde_json::from_str::<Survey>(&raw) {
            for q in &survey.searched {
                for hit in &q.hits {
                    if let Some(content) = hit.content.as_deref().filter(|c| !c.trim().is_empty()) {
                        let url = hit.url.clone().unwrap_or_else(|| {
                            format!("estate:{}:{}", hit.corpus_id, hit.chunk_id)
                        });
                        if seen.insert(url.clone()) {
                            collected.push((url, Some(hit.chunk_id.clone()), content.to_string()));
                        }
                    }
                }
            }
        }
    }
    let mut window_paths: Vec<PathBuf> = std::fs::read_dir(&run_dir)
        .map_err(|e| format!("read run dir {}: {e}", run_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("evidence-window-") && n.ends_with(".json"))
                .unwrap_or(false)
        })
        .collect();
    window_paths.sort();
    for path in window_paths {
        let raw =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let window: EvidenceWindow =
            serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
        for chunk in &window.chunks {
            if chunk.content.trim().is_empty() {
                continue;
            }
            let url = if chunk.source_url.is_empty() {
                chunk.locator.clone()
            } else {
                chunk.source_url.clone()
            };
            if seen.insert(url.clone()) {
                collected.push((url, Some(chunk.locator.clone()), chunk.content.clone()));
            }
        }
    }
    if collected.is_empty() {
        eprintln!("deep-research: run fetched no content — estate corpus {corpus_id} not created");
        return Ok(());
    }

    // Embed + insert through the ONE embed path (ProviderEmbed — the
    // same surface the estate leg uses).
    let mut pairs: Vec<(InsertChunk, Vec<f32>)> = Vec::with_capacity(collected.len());
    let mut dim = 0usize;
    for (url, title, content) in &collected {
        let embedding = provider
            .embed(content)
            .await
            .map_err(|e| format!("embed estate chunk `{url}`: {e}"))?;
        if dim == 0 {
            dim = embedding.len();
        }
        pairs.push((
            InsertChunk {
                content: content.clone(),
                title: title.clone(),
                url: Some(url.clone()),
                metadata: None,
                content_hash: None,
                source_doc_id: Some(url.clone()),
                source_file: None,
                code: Default::default(),
                unit_id: None,
            },
            embedding,
        ));
    }
    let index = CorpusIndex::create_with_sharing(
        &corpus_dir,
        &corpus_id,
        &corpus_id,
        embed_model,
        dim,
        false,
        Some(false),
        "dr-estate",
    )
    .await
    .map_err(|e| format!("create estate corpus {corpus_id}: {e}"))?;
    index
        .insert_batch(&pairs)
        .await
        .map_err(|e| format!("insert into estate corpus {corpus_id}: {e}"))?;
    // Index build is best-effort (a warn; a small corpus's IVF/FTS
    // matters less than the stamps below).
    if let Err(e) = index.build_indexes(true, true, None).await {
        eprintln!("deep-research: estate corpus {corpus_id}: index build warned: {e}");
    }
    // The two stamps retrieval and listing check — mark_indexes_built
    // MUST stamp; a failure propagates (an invisible corpus would be
    // a silent failure).
    index
        .mark_indexes_built()
        .map_err(|e| format!("stamp indexes-built on {corpus_id}: {e}"))?;
    index
        .mark_ingestion_complete()
        .map_err(|e| format!("stamp ingestion-complete on {corpus_id}: {e}"))?;

    // Stamp the manifest's fetched sources and re-write the record.
    let ingested: std::collections::BTreeSet<&String> =
        collected.iter().map(|(url, _, _)| url).collect();
    for f in &mut outcome.manifest.sources.fetched {
        if ingested.contains(&f.url) {
            f.ingested_into = Some(corpus_id.clone());
        }
    }
    let manifest_json = serde_json::to_string_pretty(&outcome.manifest)
        .map_err(|e| format!("manifest serialize: {e}"))?;
    std::fs::write(run_dir.join("manifest.json"), manifest_json)
        .map_err(|e| format!("manifest re-write: {e}"))?;
    eprintln!(
        "deep-research: estate corpus {corpus_id} built — {} chunks (retrieval-visible)",
        pairs.len()
    );
    Ok(())
}

fn print_summary(outcome: &RunOutcome) {
    let m = &outcome.manifest;
    println!();
    println!("deep-research: {outcome:?}");
    println!("terminal state: {}", outcome.terminal_state.as_str());
    println!("report: {}", outcome.report_path.display());
    println!(
        "rounds: {} | gaps after last round: {} | searches: {} | fetched sources: {}",
        m.rounds.len(),
        m.rounds.last().map(|r| r.gaps_after).unwrap_or(0),
        m.rounds.iter().map(|r| r.search_calls).sum::<u32>(),
        m.sources.fetched.len()
    );
    if !m.not_covered.is_empty() {
        println!("open questions (could-not-judge):");
        for g in &m.not_covered {
            println!("  - {g}");
        }
    }
    println!("artifacts (flight recorder):");
    for a in &outcome.artifacts {
        println!("  {a}");
    }
}

/// T6b pre-window slice (pre-registered 2026-08-19): the post-flight
/// RACE article page. Reads the run's verdict-set.json (the structured
/// channel — typed citations and verdicts) and writes `render-race.md`
/// beside report.md: passed findings with typed citations, downgraded
/// claims stamped, zero model-written tails. The page's question is
/// report.md's own H1 — the question the transcript actually answers
/// (a reframed/redirected run's title comes from the record, never a
/// silent substitute). Skipped with a named note when the verdict set
/// is absent (an aborted run); a write failure fails the verb loudly —
/// the deliverable is missing.
fn write_race_render(report_path: &std::path::Path) -> Result<(), String> {
    let dir = report_path
        .parent()
        .ok_or_else(|| "the run's report path has no parent (no run dir)".to_string())?;
    let question = match std::fs::read_to_string(report_path) {
        Ok(text) => text
            .lines()
            .find_map(|l| l.strip_prefix("# ").map(str::to_string))
            .ok_or_else(|| {
                format!(
                    "render-race.md skipped — report.md carries no `# ` heading: {}",
                    report_path.display()
                )
            })?,
        Err(_) => {
            eprintln!(
                "deep-research: render-race.md skipped — report.md unreadable at {}",
                report_path.display()
            );
            return Ok(());
        }
    };
    let verdict_path = dir.join("verdict-set.json");
    let verdict_set: VerdictSet = match std::fs::read(&verdict_path)
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
    {
        Some(vs) => vs,
        None => {
            eprintln!(
                "deep-research: render-race.md skipped — {} absent or unreadable (aborted run?)",
                verdict_path.display()
            );
            return Ok(());
        }
    };
    let page = render_race(&question, &verdict_set.claims, &verdict_set.run_id);
    let race_path = dir.join("render-race.md");
    std::fs::write(&race_path, page).map_err(|e| {
        format!(
            "render-race.md write failed at {}: {e}",
            race_path.display()
        )
    })
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        complete_with_shed_retry, extract_pdf_bytes, looks_shed, shed_retry_hint_secs,
        write_race_render, MAX_SHED_RETRIES,
    };
    use futures::Stream;
    use sovereign_contracts::error::Error as ContractError;
    use sovereign_contracts::types::{CompletionRequest, CompletionResponse, Speed};
    use sovereign_core::deep_research::estate::estate_snippet;
    use sovereign_core::traits::InferenceProvider;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    /// Measured fixture (demo re-ask dr-1786727099): the Smithsonian
    /// timeline chunk's 240-char prefix is nav + donate blurb; the
    /// answer content starts ~1.6k chars in. The snippet must center
    /// on the query terms, not the prefix.
    #[test]
    fn estate_snippet_centers_on_query_terms_not_nav_chrome() {
        let content = "Apollo 11 Timeline | National Air and Space Museum Skip to main content \
            Visit tips around Freedom 250 Grand Prix in Washington, DC. \
            Give Show additional content Give Donate Become a Member Wall of Honor Ways to Give \
            Host an Event Be the spark Your support will help fund exhibitions, educational \
            programming, and preservation efforts. Apollo 11 Timeline \
            Breadcrumb Home Explore Stories The Apollo Missions Apollo 11 Timeline \
            On July 20, 1969, a human walked on the Moon for the first time. \
            From launch to landing, Armstrong, Aldrin, and Collins were on a three day journey \
            to the Moon.";
        let query =
            "When did the Apollo 11 mission land on the Moon and who were its crew members?";
        let snippet = estate_snippet(content, query, 600);
        assert!(
            snippet.contains("July 20, 1969"),
            "snippet must carry the answer content, not the donate blurb: {snippet}"
        );
        assert!(
            snippet.contains("Armstrong, Aldrin, and Collins"),
            "snippet must carry the crew content: {snippet}"
        );
    }

    /// No query term in the chunk — fall back to the prefix (short
    /// chunks, non-lexical matches).
    #[test]
    fn estate_snippet_falls_back_to_prefix_without_query_terms() {
        let content = "short chunk with no matching terms here";
        let snippet = estate_snippet(content, "zzzqqq wwww", 50);
        assert_eq!(snippet, content);
    }

    // ------------------------------------------------------------------
    // T6b pre-window slice — the post-flight RACE page (RED-FIRST: the
    // write path did not exist at HEAD; the render test in sovereign-core
    // watched the red first — order deep-research-t6b, pre-registered).
    // ------------------------------------------------------------------

    /// write_race_render reads a run dir's verdict-set.json (the
    /// structured channel, real wire shape) + report.md's H1 (the
    /// question the transcript actually answers) and writes the clean
    /// article page beside the transcript — typed citations in [passed]
    /// position, no model-written tails, downgraded claims stamped.
    #[test]
    fn write_race_render_produces_the_clean_page_from_a_run_dir() {
        let tmp = std::env::temp_dir().join(format!("dr-race-render-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let report = "# Meridian Bridge history\n\n- run: `dr-test`\n\n\
            ## Findings\n\n- **[passed]** The bridge was completed in 1873. — `ev-1` \
            [https://example.com/a](https://example.com/a)\n";
        std::fs::write(tmp.join("report.md"), report).unwrap();
        let verdict_set = serde_json::json!({
            "icd": "verdict_set",
            "version": 1,
            "run_id": "dr-test",
            "charter_hash": "h",
            "claims": [
                {"id": "c1",
                 "text": "The bridge was completed in 1873 [Source: https://example.com/draft]. ",
                 "verdict": "passed", "status": "passed",
                 "evidence_ids": ["ev-1"],
                 "citations": [{"evidence_id": "ev-1", "url": "https://example.com/a",
                                "chunk_id": "ev-1"}],
                 "flag": null},
                {"id": "c2",
                 "text": "The engineer was Helena Voss.",
                 "verdict": "failed", "status": "failed",
                 "evidence_ids": [], "citations": [],
                 "flag": "refuted by the evidence"}
            ]
        });
        std::fs::write(
            tmp.join("verdict-set.json"),
            serde_json::to_vec_pretty(&verdict_set).unwrap(),
        )
        .unwrap();
        write_race_render(&tmp.join("report.md")).unwrap();
        let page = std::fs::read_to_string(tmp.join("render-race.md")).unwrap();
        assert!(page.starts_with("# Meridian Bridge history"), "{page}");
        assert!(page.contains("## Findings"), "{page}");
        let findings = page.split("## Findings").nth(1).expect("findings present");
        assert!(findings.contains("ev-1"), "{findings}");
        assert!(findings.contains("https://example.com/a"), "{findings}");
        assert!(!findings.contains("[Source:"), "{findings}");
        assert!(page.contains("[refuted]"), "{page}");
        assert!(page.contains("Helena Voss"), "{page}");
        // The transcript file is untouched, byte-for-byte.
        assert_eq!(
            std::fs::read_to_string(tmp.join("report.md")).unwrap(),
            report
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// No verdict set (an aborted run) skips with a note — never an
    /// error and never a page pretending to be complete.
    #[test]
    fn write_race_render_skips_without_a_verdict_set() {
        let tmp = std::env::temp_dir().join(format!("dr-race-render-skip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("report.md"), "# Q\n\n## Findings\n\n").unwrap();
        write_race_render(&tmp.join("report.md")).unwrap();
        assert!(
            !tmp.join("render-race.md").exists(),
            "no verdict set — no race page"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

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
