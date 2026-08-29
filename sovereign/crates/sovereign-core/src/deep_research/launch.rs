// SPDX-License-Identifier: AGPL-3.0-or-later
//! Launching and closing a deep-research run — the sequence every host
//! shares, in ONE place.
//!
//! A host knows two things: what the operator asked for, and how to
//! report progress. Everything between those — which models the daemon
//! is serving, what the run is called, where its directory lives, how
//! the consent grant is minted, which port to build, how the `RunConfig`
//! is assembled, and what has to happen after the loop lands — is
//! runtime, and lives here.
//!
//! This exists because it was previously assembled inline in
//! `sovereign-cli`'s verb. The desktop could not call it (nothing may
//! depend on a host, `quality/ARCH_LAYERS.toml`), so it spawned the CLI
//! as a subprocess instead — and a second host assembling its own
//! `RunConfig` would have been two implementations of one launch, the
//! §10.6 shape this codebase has been burned by. `prepare` and `close`
//! are the two halves, and a host that calls `prepare` cannot forget
//! what `close` does: the estate ingest and the RACE page are not
//! optional courtesies, they are what makes a finished run readable and
//! re-citable.
//!
//! The order is fixed and it is the reason these are two functions and
//! not three: `prepare` → `run`/`resume` (the caller's, so it owns the
//! abort flag and progress reporting) → `close`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::acquisition::{DEFAULT_CONTENT_COVERAGE_FLOOR, DEFAULT_PROSE_LINE_FLOOR};
use super::estate::ResearchPort;
use super::icd::{EvidenceWindow, Survey, VerdictSet, ICD_VERSION};
use super::port::{build_port, indexes_dir};
use super::render::render_race;
use super::{RunConfig, RunOutcome, SearchSource};
use crate::egress::ConsentGrant;
use crate::oicp::ShardingPrivacy;
use crate::setup_config::SetupConfig;
use crate::traits::InferenceProvider;
use crate::types::Custody;
use corpus_engine::index::{CorpusIndex, InsertChunk};

/// The launch sidecar (order deep-research-t3a): the run's backend
/// identity, written into the run dir BEFORE launch and read back on
/// resume — the operator's `--backend`/`--mock-deck` flags are verified
/// against it flag-by-flag, never silently substituted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeInput {
    pub icd: String,
    pub version: u32,
    pub run_id: String,
    pub backend: String,
    #[serde(default)]
    pub mock_deck_dir: Option<String>,
}

/// What a HOST knows about a fresh run: the operator's question and the
/// budget they set. Nothing here names a model, a port, a run id or a
/// directory — those are the runtime's to decide, which is the whole
/// point of the type.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub question: String,
    /// The directory runs are minted UNDER. `prepare` appends the run id.
    pub runs_base: PathBuf,
    pub max_rounds: u32,
    pub code_set_k: usize,
    pub eps_quota: f64,
    pub search_allowance: u32,
    pub fetch_allowance: u32,
    pub estate_corpus_ids: Vec<String>,
    pub search_source: SearchSource,
    /// `auto` (the live port) or `mock` (the deck drill surface).
    pub backend: String,
    pub mock_deck_dir: Option<PathBuf>,
    /// The operator's consent class. The closed set is the `Custody`
    /// enum itself — a host parses the operator's wire string once with
    /// `Custody::parse_wire` (the ONE parser) and hands the type in, so
    /// an unknown class cannot reach a run dir. `None` is default-deny:
    /// non-public-web egress refuses.
    pub consent_floor: Option<Custody>,
}

/// Everything `prepare` decided, handed to the caller so it can drive
/// the loop and report progress. The `run_dir` is real on disk by the
/// time this returns — a host can announce it before the loop turns,
/// with no stderr to scrape.
pub struct Launch {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub config: RunConfig,
    pub port: Arc<dyn ResearchPort>,
    pub provider: Arc<dyn InferenceProvider>,
    /// The daemon's `/v1` endpoint and the two model ids resolved from
    /// `SetupConfig` — named so a host can show the operator what it is
    /// actually about to run against.
    pub endpoint: String,
    pub draft_model: String,
    pub embed_model: String,
    pub web_backend: String,
}

/// Resolve the daemon endpoint and the draft/embed model ids from the
/// operator's `SetupConfig`. The ONE read — a host that wants to show
/// them reads them off `Launch`, never a second `SetupConfig::load`.
/// The `/v1` root of the daemon this run will talk to — [`client_daemon_base`]
/// plus the suffix, and nothing else.
///
/// Split out of [`daemon_targets`] because the endpoint and the two MODEL ids
/// have different dependencies: the ids genuinely need a `SetupConfig` on
/// disk, the endpoint does not. `daemon_targets` used to build this from
/// `cfg.daemon.client_port`, which made a deep-research run — the single most
/// expensive thing in this tree to misdirect — ignore `SOVEREIGN_DAEMON_URL`
/// and silently drive the operator's local daemon instead of the one the
/// operator had just pointed it at (§10.6, §18.3).
pub fn daemon_endpoint() -> String {
    format!("{}/v1", crate::setup_config::client_daemon_base())
}

pub fn daemon_targets() -> Result<(String, String, String), String> {
    let cfg = SetupConfig::load().map_err(|e| format!("SetupConfig load: {e}"))?;
    let endpoint = daemon_endpoint();
    let draft_model = cfg
        .models
        .primary
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("SetupConfig.models.primary has no filename stem (the draft model id)")?
        .to_string();
    let embed_model = cfg
        .models
        .embed
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("SetupConfig.models.embed has no filename stem (the embed model id)")?
        .to_string();
    Ok((endpoint, draft_model, embed_model))
}

/// Everything decided before the loop turns: the daemon targets, the run
/// id and directory, the consent grant, the provider, the port, the
/// `RunConfig`, and the resume sidecar on disk.
///
/// One decider, one name (§10.6): every host launches through here, so
/// there is exactly one assembly of a `RunConfig` for a fresh run and
/// exactly one place a launch default can be read.
pub async fn prepare(opts: LaunchOptions) -> Result<Launch, String> {
    if opts.question.trim().is_empty() {
        return Err("a question is required".to_string());
    }
    if opts.backend == "mock" && opts.mock_deck_dir.is_none() {
        return Err("backend `mock` requires a mock deck directory".to_string());
    }
    let (endpoint, draft_model, embed_model) = daemon_targets()?;

    let run_id = format!("dr-{}", now_unix());
    let run_dir = opts.runs_base.join(&run_id);

    // The run-scoped consent grant (order deep-research-t2a): minted
    // once, here, from the operator's class — then frozen (FR-3) into
    // both the port (the egress boundary's check) and the RunConfig
    // (the manifest record). Default-deny: no class, no grant.
    if opts.consent_floor == Some(Custody::Unknown) {
        return Err(
            "a consent grant never releases `unknown` provenance — the closed set is \
             public-web | peer | personal"
                .to_string(),
        );
    }
    let consent: Option<ConsentGrant> = opts.consent_floor.map(|release_floor| ConsentGrant {
        run_id: run_id.clone(),
        granted_at_unix: now_unix(),
        release_floor,
    });

    let provider: Arc<dyn InferenceProvider> = Arc::new(oicp_client::RemoteApiProvider::new(
        &endpoint,
        None,
        &draft_model,
        8192,
    ));
    let (port, web_backend) = build_port(
        &opts.backend,
        opts.mock_deck_dir.as_deref(),
        opts.search_source,
        &opts.estate_corpus_ids,
        provider.clone(),
        consent.clone(),
    )
    .await?;

    let config = RunConfig {
        run_id: run_id.clone(),
        question: opts.question.trim().to_string(),
        seed_id: None,
        run_dir: run_dir.clone(),
        max_rounds: opts.max_rounds,
        code_set_k: opts.code_set_k,
        eps_quota: opts.eps_quota,
        // drb1-t2: the content admission floors — one decider, the
        // acquisition consts (the charter records them).
        content_coverage_floor: DEFAULT_CONTENT_COVERAGE_FLOOR,
        prose_line_floor: DEFAULT_PROSE_LINE_FLOOR,
        // Greedy (2026-08-24): the window holds SOURCES, and the
        // composer retrieves from it rather than reading it whole, so a
        // bigger window is a bigger pool and not a bigger prompt. 20 was
        // sized when the loop could only deliver 4-10 sources anyway.
        evidence_window_max_chunks: 100,
        estate_corpus_ids: opts.estate_corpus_ids,
        web_backend: web_backend.clone(),
        search_source: opts.search_source,
        web_search_allowance: opts.search_allowance,
        web_fetch_allowance: opts.fetch_allowance,
        posture: ShardingPrivacy::LocalOnly,
        consent,
        max_rounds_override: None,
        max_search_override: None,
        max_fetch_override: None,
    };

    // The launch sidecar, recorded BEFORE launch so a later resume can
    // verify the operator's flags against it (never a silent
    // substitution).
    std::fs::create_dir_all(&run_dir)
        .map_err(|e| format!("create run dir {}: {e}", run_dir.display()))?;
    let sidecar = ResumeInput {
        icd: "resume_input".to_string(),
        version: ICD_VERSION,
        run_id: run_id.clone(),
        backend: opts.backend.clone(),
        mock_deck_dir: opts
            .mock_deck_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
    };
    let sidecar_json = serde_json::to_string_pretty(&sidecar)
        .map_err(|e| format!("resume sidecar serialize: {e}"))?;
    std::fs::write(run_dir.join("resume-input.json"), sidecar_json)
        .map_err(|e| format!("resume sidecar write: {e}"))?;

    Ok(Launch {
        run_id,
        run_dir,
        config,
        port,
        provider,
        endpoint,
        draft_model,
        embed_model,
        web_backend,
    })
}

/// Read + verify a run's launch sidecar. Every refusal is typed and
/// names what was withheld: unreadable (a run launched before the
/// sidecar existed cannot be resumed), malformed, foreign/tampered
/// (wrong icd or version), or belonging to a different run.
pub fn read_resume_sidecar(resume_dir: &Path, run_id: &str) -> Result<ResumeInput, String> {
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
    if sidecar.run_id != run_id {
        return Err(format!(
            "{sidecar_path:?} belongs to run {} but the checkpoint is run {} — mismatched run \
             dir",
            sidecar.run_id, run_id
        ));
    }
    Ok(sidecar)
}

/// The resume counterpart of `prepare`: restore a run's identity from
/// its checkpoint and launch sidecar, rebuild the port from THAT
/// identity, and hand back a `Launch` the caller drives with `resume`.
///
/// Nothing is re-decided here — the checkpoint's frozen config is the
/// run (FR-3). A host that wants to verify operator-passed flags against
/// it does so BEFORE calling this (the CLI's `--resume` gate); a host
/// with no flags to verify (the desktop's resume affordance) calls it
/// directly. The named directory is the state home, not the launch dir
/// the checkpoint recorded: resuming a COPY must continue the copy.
pub async fn prepare_resume(resume_dir: &Path) -> Result<Launch, String> {
    let cp = super::read_checkpoint(resume_dir)?;
    let mut config = cp.config.clone();
    config.run_dir = resume_dir.to_path_buf();
    let sidecar = read_resume_sidecar(resume_dir, &config.run_id)?;
    let (endpoint, draft_model, embed_model) = daemon_targets()?;
    let provider: Arc<dyn InferenceProvider> = Arc::new(oicp_client::RemoteApiProvider::new(
        &endpoint,
        None,
        &draft_model,
        8192,
    ));
    let (port, web_backend) = build_port(
        &sidecar.backend,
        sidecar.mock_deck_dir.as_deref().map(Path::new),
        config.search_source,
        &config.estate_corpus_ids,
        provider.clone(),
        config.consent.clone(),
    )
    .await?;
    Ok(Launch {
        run_id: config.run_id.clone(),
        run_dir: resume_dir.to_path_buf(),
        config,
        port,
        provider,
        endpoint,
        draft_model,
        embed_model,
        web_backend,
    })
}

/// Everything that has to happen after the loop lands, in order: the
/// run's fetched evidence is ingested into `dr-estate-<run_id>` so a
/// later run can cite it without going back to the web, and the clean
/// RACE article page is written beside `report.md`.
///
/// Both failures are LOUD. A run whose evidence never reached the estate
/// looks finished and is not, and a missing RACE page is a missing
/// deliverable — neither is allowed to pass in silence (§18.3).
/// Takes the provider and embed model rather than a `&Launch` so the
/// RESUME path — which restores its identity from the checkpoint and the
/// sidecar, not from a fresh launch — closes through this same function.
/// One close sequence, three callers.
pub async fn close(
    outcome: &mut RunOutcome,
    provider: &Arc<dyn InferenceProvider>,
    embed_model: &str,
) -> Result<(), String> {
    ingest_run_estate(outcome, provider, embed_model).await?;
    write_race_render(&outcome.report_path)
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
    use super::{daemon_endpoint, write_race_render};

    // ------------------------------------------------------------------
    // The daemon endpoint is the ONE decider's, not a second reading of
    // `[daemon] client_port` (order vast-dev-daemon, part B).
    // ------------------------------------------------------------------

    static DAEMON_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct DaemonEnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prior: Vec<(&'static str, Option<String>)>,
    }

    impl DaemonEnvGuard {
        fn set(pairs: &[(&'static str, &str)]) -> Self {
            let lock = DAEMON_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            const KEYS: [&str; 2] = ["SOVEREIGN_DAEMON_URL", "SVRNMESH_DAEMON_URL"];
            let prior = KEYS.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in KEYS {
                std::env::remove_var(k);
            }
            for (k, v) in pairs {
                std::env::set_var(k, v);
            }
            Self { _lock: lock, prior }
        }
    }

    impl Drop for DaemonEnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.prior {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// RED on the tree before this sweep: `daemon_targets` built the endpoint
    /// from `cfg.daemon.client_port` and ignored `SOVEREIGN_DAEMON_URL`, so a
    /// deep-research run pointed at a rented daemon silently drove the
    /// operator's local one instead — and a run is the most expensive thing
    /// in the tree to misdirect.
    ///
    /// This resolves WITHOUT a `SetupConfig` on disk, deliberately: the
    /// endpoint is not a config-dependent fact once it goes through
    /// `client_daemon_base`, which carries its own fallback. Only the two
    /// MODEL ids still need the config, which is why `daemon_targets` keeps
    /// returning a `Result` and this accessor does not.
    #[test]
    fn the_daemon_endpoint_honours_the_knob() {
        let _g = DaemonEnvGuard::set(&[("SOVEREIGN_DAEMON_URL", "http://a-rented-pod:9841")]);
        assert_eq!(daemon_endpoint(), "http://a-rented-pod:9841/v1");
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
}
