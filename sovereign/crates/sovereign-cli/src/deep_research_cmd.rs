// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn deep-research "<question>"` — run the THIN local-only research
//! loop (order deep-research-t1a) end to end through the shipped CLI.
//!
//! The verb implements the loop's `ResearchPort` (sovereign-core
//! `deep_research::estate`) over the real estate (corpus-engine indexes),
//! the real network (DuckDuckGo + fetch-and-extract), and the daemon's
//! OpenAI-compatible surface (embed + draft, with the URL allowlist
//! constraint on every draft ask). The loop itself lives in sovereign-core
//! (`deep_research::run`); nothing in this file re-implements a loop step.
//!
//! Custody is stamped here, by code, never by a model (R-2/R-6): estate
//! hits carry `personal` (a local corpus is the operator's own data), web
//! hits carry `public-web`. The loop's gate refuses unknown provenance.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use oicp_client::RemoteApiProvider;
use sovereign_contracts::types::CompletionRequest;
use sovereign_contracts::types::Speed;
use sovereign_core::deep_research::estate::{EstateListing, PortHit, ResearchPort};
use sovereign_core::deep_research::icd::CorpusEntry;
use sovereign_core::deep_research::{run, RunConfig, RunOutcome};
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

/// The chunk count from `_corpus_meta.json` when present (the engine
/// writes `"chunk_count"` there at commit); 0 when the file is absent
/// or unreadable — a listed-but-uncounted corpus is still searchable.
fn corpus_chunk_count(dir: &std::path::Path) -> i64 {
    std::fs::read_to_string(dir.join("_corpus_meta.json"))
        .ok()
        .and_then(|meta| serde_json::from_str::<serde_json::Value>(&meta).ok())
        .and_then(|v| v["chunk_count"].as_i64())
        .unwrap_or(0)
}

/// The port implementation for the CLI: real estate, real network, real
/// daemon. All inference goes through one `RemoteApiProvider` against the
/// local daemon's `/v1` surface — the loop never touches a frontier.
struct CliResearchPort {
    provider: Arc<dyn InferenceProvider>,
    client: reqwest::Client,
    orchestrator: sovereign_tools_base::web::search::SearchOrchestrator,
    indexes: std::path::PathBuf,
    daemon_endpoint: String,
}

impl CliResearchPort {
    fn new(provider: Arc<dyn InferenceProvider>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client build");
        let backend = sovereign_tools_base::web::search::DuckDuckGoBackendImpl::new();
        let mut registry = sovereign_tools_base::web::search::WebSearchRegistry::new();
        registry.register(Arc::new(backend));
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
            indexes: indexes_dir(),
            daemon_endpoint,
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
                    url: r.url.clone().unwrap_or_else(|| format!("estate:{id}")),
                    title: r.title.unwrap_or_default(),
                    snippet: r.content.chars().take(240).collect(),
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
                    budget: &sovereign_tools_base::web::search::BudgetView::new(),
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
                score: 0.0,
                source: format!("web:{backend}"),
                custody: Custody::PublicWeb,
            })
            .collect())
    }

    async fn web_fetch(&self, url: &str) -> Result<String, String> {
        sovereign_tools_base::web::extract::fetch_and_extract(&self.client, url)
            .await
            .map_err(|e| format!("fetch {url}: {e}"))
    }

    async fn terminal_poll(&self) -> Result<(), String> {
        let probe = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .map_err(|e| format!("http client build: {e}"))?;
        let url = format!("{}/models", self.daemon_endpoint);
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
}

/// `svrn deep-research "<question>" [--run-dir DIR] [--max-rounds N]
/// [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N]
/// [--fetch N]`
pub async fn cmd_deep_research(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: svrn deep-research \"<question>\" [--run-dir DIR] [--max-rounds N] \
             [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N] [--fetch N]"
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

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
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
                eprintln!("Usage: svrn deep-research \"<question>\" [--run-dir DIR] [--max-rounds N] [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N] [--fetch N]");
                return 1;
            }
        }
        i += 1;
    }
    let Some(question) = question else {
        eprintln!(
            "Usage: svrn deep-research \"<question>\" [--run-dir DIR] [--max-rounds N] \
             [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N] [--fetch N]"
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
    let port = Arc::new(CliResearchPort::new(provider.clone()));

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
        web_backend: "duckduckgo".to_string(),
        web_search_allowance: search_allowance,
        web_fetch_allowance: fetch_allowance,
        posture: ShardingPrivacy::LocalOnly,
    };

    eprintln!("deep-research: run {run_id} — {question}");
    eprintln!("deep-research: run dir {}", run_dir.display());
    eprintln!("deep-research: daemon {endpoint} (draft {draft_model}, embed {embed_model})");

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
