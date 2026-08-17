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

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use oicp_client::RemoteApiProvider;
use sovereign_contracts::types::CompletionRequest;
use sovereign_contracts::types::Speed;
use sovereign_core::deep_research::estate::{
    estate_snippet, read_staged_alignment, AlignmentDecision, EstateListing, PortHit, ResearchPort,
};
use sovereign_core::deep_research::gym::{
    CorpusSurface, Deck, MockBackendImpl, MockDraftSurface, ProviderEmbed,
};
use sovereign_core::deep_research::icd::{CorpusEntry, Plan};
use sovereign_core::deep_research::{run, RunConfig, RunOutcome, SearchSource};
use sovereign_core::oicp::ShardingPrivacy;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::Custody;

use corpus_engine::index::CorpusIndex;

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
    budget: sovereign_tools_base::web::search::BudgetView,
    /// True iff the operator's Tavily key was present at port
    /// construction. The ONE source of the loop's web-backend default
    /// (`default_web_backend`); no second read of the env var exists.
    tavily_keyed: bool,
    indexes: std::path::PathBuf,
    daemon_endpoint: String,
}

impl CliResearchPort {
    fn new(provider: Arc<dyn InferenceProvider>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client build");
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
        // Budget from the house defaults (budget.tavily daily_calls =
        // 100 et al.) — the orchestrator's untracked=unlimited default
        // would otherwise apply on this CLI path.
        let mut budget = sovereign_tools_base::web::search::BudgetView::new();
        for (id, entry) in
            &sovereign_tools_base::web::search::assets::BackendsConfig::from_default_toml()
                .budget
                .per_backend
        {
            budget.set(id, entry.daily_calls);
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
            budget,
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
                    budget: &self.budget,
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
        Ok(out
            .results
            .into_iter()
            .enumerate()
            .map(|(i, r)| PortHit {
                id: format!("web-{i}"),
                url: r.url,
                title: r.title,
                snippet: r.snippet,
                // Web results carry no body through this surface.
                content: None,
                score: 0.0,
                source: format!("web:{backend}"),
                custody: Custody::PublicWeb,
            })
            .collect())
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
        let resp = self
            .provider
            .complete(&CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Slow,
                max_tokens: None,
                temperature: Some(0.4),
                structured_output: None,
                think_budget: None,
                url_allowlist: None,
                ..Default::default()
            })
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

/// `svrn deep-research "<question>" [--run-dir DIR] [--max-rounds N]
/// [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N]
/// [--fetch N] [--backend auto|mock] [--mock-deck DIR]
/// [--search-source mock|corpus]`
pub async fn cmd_deep_research(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: svrn deep-research \"<question>\" [--run-dir DIR] [--max-rounds N] \
             [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N] [--fetch N] \
             [--backend auto|mock] [--mock-deck DIR] [--search-source mock|corpus]"
        );
        return 0;
    }
    let mut question: Option<String> = None;
    let mut run_dir = std::env::temp_dir().join("deep-research-runs");
    let mut max_rounds = 3u32;
    let mut corpora: Vec<String> = Vec::new();
    let mut code_set_k = 3usize;
    let mut eps_quota = 0.1f64;
    let mut search_allowance = 4u32;
    let mut fetch_allowance = 4u32;
    // The P5 drill surface (additive; default `auto` = the real
    // network). `--backend mock` serves search/fetch from the deck
    // directory, drafts via the real daemon.
    let mut backend = "auto".to_string();
    let mut mock_deck_dir: Option<PathBuf> = None;
    // The acquisition search source (t1g rung 2): a closed set,
    // decided once here — `mock` (default) or `corpus`.
    let mut search_source = SearchSource::Mock;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--backend" => {
                i += 1;
                backend = args.get(i).cloned().unwrap_or_default();
            }
            "--search-source" => {
                i += 1;
                match SearchSource::parse(args.get(i).map(String::as_str).unwrap_or_default()) {
                    Some(s) => search_source = s,
                    None => {
                        eprintln!(
                            "deep-research: unknown search source {:?} — the closed set is \
                             mock | corpus",
                            args.get(i).map(String::as_str).unwrap_or_default()
                        );
                        return 1;
                    }
                }
            }
            "--mock-deck" => {
                i += 1;
                mock_deck_dir = Some(PathBuf::from(args.get(i).cloned().unwrap_or_default()));
            }
            "--run-dir" => {
                i += 1;
                run_dir = PathBuf::from(args.get(i).cloned().unwrap_or_default());
            }
            "--max-rounds" => {
                i += 1;
                max_rounds = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(max_rounds);
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
            }
            "--code-set-k" => {
                i += 1;
                code_set_k = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(code_set_k);
            }
            "--eps-quota" => {
                i += 1;
                eps_quota = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(eps_quota);
            }
            "--search" => {
                i += 1;
                search_allowance = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(search_allowance);
            }
            "--fetch" => {
                i += 1;
                fetch_allowance = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(fetch_allowance);
            }
            s if question.is_none() => question = Some(s.to_string()),
            _ => {
                eprintln!("Usage: svrn deep-research \"<question>\" [--run-dir DIR] [--max-rounds N] [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N] [--fetch N] [--backend auto|mock] [--mock-deck DIR] [--search-source mock|corpus]");
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
    let Some(question) = question else {
        eprintln!(
            "Usage: svrn deep-research \"<question>\" [--run-dir DIR] [--max-rounds N] \
             [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N] [--fetch N] \
             [--backend auto|mock] [--mock-deck DIR] [--search-source mock|corpus]"
        );
        return 1;
    };

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

    let run_id = format!("dr-{}", now_unix());
    let run_dir = run_dir.join(&run_id);

    let provider: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(&endpoint, None, &draft_model, 8192));
    // `--backend mock`: search/fetch are served from the deck
    // directory; drafting is DELEGATED to the real port (the daemon).
    // The web backend is the mock's closed-set id — the loop's search
    // leg then routes to the deck, and the deck refuses any other id.
    let (port, web_backend): (Arc<dyn ResearchPort>, String) = if backend == "mock" {
        let deck_dir = mock_deck_dir.as_deref().expect("validated above");
        let deck = match Deck::load(deck_dir) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("deep-research: mock deck load failed: {e}");
                return 1;
            }
        };
        let real = Arc::new(CliResearchPort::new(provider.clone()));
        // The corpus source (t1g rung 2): the mock serves its estate
        // leg from the estate's REAL corpus indexes — the compounding
        // corpus is a search source, opened read-only, queried with
        // the daemon's embed slot.
        let mock = if search_source == SearchSource::Corpus {
            let mut indexes = Vec::new();
            let mut missing = Vec::new();
            for id in &corpora {
                let dir = indexes_dir().join(id);
                if !corpus_searchable(&dir) {
                    missing.push(id.clone());
                    continue;
                }
                match CorpusIndex::open(&dir).await {
                    Ok(i) => indexes.push(i),
                    Err(e) => {
                        eprintln!("deep-research: open corpus `{id}`: {e}");
                        return 1;
                    }
                }
            }
            if !missing.is_empty() {
                eprintln!(
                    "deep-research: --search-source corpus: corpus not searchable at the \
                     estate: {} — a named corpus is never silently skipped",
                    missing.join(", ")
                );
                return 1;
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
        (Arc::new(mock), MockBackendImpl::BACKEND_ID.to_string())
    } else {
        let real = Arc::new(CliResearchPort::new(provider.clone()));
        // The web backend default is decided once, by the port, from
        // the operator's key presence (tavily when keyed, duckduckgo
        // otherwise).
        let web_backend = real.default_web_backend().to_string();
        (real, web_backend)
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
        evidence_window_max_chunks: 20,
        estate_corpus_ids: corpora,
        web_backend,
        search_source,
        web_search_allowance: search_allowance,
        web_fetch_allowance: fetch_allowance,
        posture: ShardingPrivacy::LocalOnly,
    };

    let outcome = match run(config, port, provider, Arc::new(AtomicBool::new(false))).await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("deep-research: run failed: {e}");
            return 1;
        }
    };
    print_summary(&outcome);
    0
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

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use sovereign_core::deep_research::estate::estate_snippet;

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
}
