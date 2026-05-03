//! Post-install atlas hooks — build the structural atlas (and, in
//! follow-up tracks, run triage + spawn Tier-2 extraction) the moment
//! a corpus install finishes. No CLI handholding required.
//!
//! Today this module ships the **structural-atlas** step: takes a
//! freshly-installed chunk corpus and runs the deterministic
//! `structure_first` strategy against it, writing
//! `<corpus>/atlas/{atoms,edges}.json`. The discovery walk in
//! [`crate::atlas_context_manager::AtlasContextManager`] picks the
//! result up automatically on the next process start; in-flight
//! processes will catch it on the next `init_from_cache` call.
//!
//! Idempotency: the hook checks for an existing
//! `<corpus>/atlas/atoms.json` and short-circuits if present. To
//! force a rebuild, delete the atlas dir.
//!
//! Triage candidate generation and Tier-2 background extraction
//! follow in Tracks A5 + B; the surface here is the chain anchor
//! they extend.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use corpus_engine::enrichment::atlas::{
    read_atlas_atoms, read_atlas_edges, vital_tier, AtlasIngestionConfig, AtlasIngestionRegistry,
    AtomEnvelope,
};
use corpus_engine::progress::IngestProgress;
use corpus_engine::{CorpusEngine, EmbedFn, ProgressCallback};

/// Outcome of a structural-atlas post-install run.
#[derive(Debug)]
pub enum StructuralAtlasOutcome {
    /// Atlas was built fresh and persisted.
    Built {
        atoms_path: PathBuf,
        edges_path: PathBuf,
        elapsed_secs: f64,
    },
    /// Atlas already existed at the target path; nothing rebuilt.
    AlreadyPresent { atoms_path: PathBuf },
    /// Build was attempted but failed (already logged with detail).
    Failed { reason: String },
}

/// Build the structural atlas for `corpus_id` and write it to
/// `<indexes_dir>/<corpus_id>/atlas/`. Source and target are the
/// same — the atlas lives alongside the chunk store, so the runtime
/// atlas-context discovery walk finds it without extra plumbing.
///
/// `recipes_dir` is the standard recipe-search root (typically
/// `<data_dir>/recipes`). It's not load-bearing for `structure_first`
/// today but the `CorpusEngine` constructor requires it.
pub async fn build_structural_atlas(
    corpus_id: &str,
    indexes_dir: PathBuf,
    recipes_dir: PathBuf,
) -> StructuralAtlasOutcome {
    let atlas_dir = indexes_dir.join(corpus_id).join("atlas");
    let atoms_path = atlas_dir.join("atoms.json");
    if atoms_path.exists() {
        return StructuralAtlasOutcome::AlreadyPresent { atoms_path };
    }

    let registry = AtlasIngestionRegistry::builtin();
    let strategy = match registry.get("structure_first") {
        Some(s) => s,
        None => {
            return StructuralAtlasOutcome::Failed {
                reason: "structure_first strategy not registered".into(),
            };
        }
    };

    // structure_first reads chunk metadata, never embeds — wire a
    // no-op EmbedFn so the engine constructor doesn't require a
    // model. Same pattern as the CLI's `enrich ingest` path.
    let noop_embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(Vec::<f32>::new()) }));
    let engine = Arc::new(CorpusEngine::new(
        recipes_dir,
        indexes_dir.clone(),
        noop_embed.clone(),
    ));

    let cfg = AtlasIngestionConfig {
        strategy_id: "structure_first".into(),
        strategy_config: serde_json::json!({
            "source_corpus_id": corpus_id,
        }),
    };

    let started = std::time::Instant::now();
    let progress: Arc<ProgressCallback> = Arc::new(Box::new(move |ev: IngestProgress| {
        tracing::debug!(?ev, "structural_atlas: progress");
    }));

    let result = strategy
        .ingest(engine, noop_embed, None, cfg, progress)
        .await;
    let elapsed_secs = started.elapsed().as_secs_f64();

    let data = match result {
        Ok(d) => d,
        Err(e) => {
            return StructuralAtlasOutcome::Failed {
                reason: format!("strategy.ingest failed: {e}"),
            };
        }
    };

    if let Err(e) = std::fs::create_dir_all(&atlas_dir) {
        return StructuralAtlasOutcome::Failed {
            reason: format!("create atlas dir {}: {e}", atlas_dir.display()),
        };
    }

    let edges_path = atlas_dir.join("edges.json");
    if let Err(e) = write_atomic_json(&atoms_path, &data.atoms) {
        return StructuralAtlasOutcome::Failed {
            reason: format!("write atoms.json: {e}"),
        };
    }
    if let Err(e) = write_atomic_json(&edges_path, &data.edges) {
        return StructuralAtlasOutcome::Failed {
            reason: format!("write edges.json: {e}"),
        };
    }
    StructuralAtlasOutcome::Built {
        atoms_path,
        edges_path,
        elapsed_secs,
    }
}

/// Default Tier-2 budget when no per-corpus override is on disk:
/// how many top-priority articles the triage step picks for the
/// deep-enrichment queue. Calibrated against the wikipedia-core eval
/// bank — at 1,000 articles the L1+L2+L3 vital sets all fit, with
/// enough headroom for top-L4 by centrality. Operators override via
/// `<corpus>/atlas/triage-config.json` or the
/// `sovereign atlas budget` CLI helper.
pub const DEFAULT_TIER2_BUDGET: usize = 1000;

/// Filename of the per-corpus Tier-2 budget override. Lives next to
/// `atoms.json` so it travels with the atlas and is readable by both
/// the post-install hook and an operator inspecting the on-disk
/// state.
pub const TRIAGE_CONFIG_FILE: &str = "triage-config.json";

/// Persisted shape of the budget override. `schema_version = 1` lets
/// future additions (token-budget caps, decay knobs) land without
/// breaking existing files.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TriageConfig {
    pub schema_version: u32,
    /// Cap on `top_in_corpus_by_centrality` after the tier prior +
    /// bumps are applied. `None` (or absent file) → use
    /// [`DEFAULT_TIER2_BUDGET`].
    pub budget_articles: Option<usize>,
}

/// Read `<atlas_dir>/triage-config.json` and return the configured
/// budget, or `None` if the file is missing / malformed / has no
/// `budget_articles`. Callers fall back to [`DEFAULT_TIER2_BUDGET`].
pub fn read_triage_budget(atlas_dir: &Path) -> Option<usize> {
    let path = atlas_dir.join(TRIAGE_CONFIG_FILE);
    let raw = std::fs::read_to_string(&path).ok()?;
    let cfg: TriageConfig = serde_json::from_str(&raw).ok()?;
    cfg.budget_articles
}

/// Persist a Tier-2 budget override. Atomic via sibling `.tmp` +
/// rename so a crash mid-write can't corrupt the override. Used by
/// `sovereign atlas budget <corpus> <n>` and by callers wiring up
/// disk-aware autoscaling.
pub fn write_triage_budget(atlas_dir: &Path, budget_articles: usize) -> std::io::Result<()> {
    let cfg = TriageConfig {
        schema_version: 1,
        budget_articles: Some(budget_articles),
    };
    let value = serde_json::to_value(&cfg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let path = atlas_dir.join(TRIAGE_CONFIG_FILE);
    write_atomic_json(&path, &value)
}

/// Resolve the effective Tier-2 budget for `corpus_id`: the per-
/// corpus override if set, else [`DEFAULT_TIER2_BUDGET`]. Used by
/// the post-install chain so the structural-atlas → triage step
/// honours operator overrides without each call site re-implementing
/// the lookup.
pub fn effective_tier2_budget(indexes_dir: &Path, corpus_id: &str) -> usize {
    let atlas_dir = indexes_dir.join(corpus_id).join("atlas");
    read_triage_budget(&atlas_dir).unwrap_or(DEFAULT_TIER2_BUDGET)
}

/// Outcome of a triage post-install run.
#[derive(Debug)]
pub enum TriageOutcome {
    /// Triage was computed and persisted.
    Built {
        path: PathBuf,
        in_corpus_picked: usize,
        elapsed_secs: f64,
    },
    /// Atlas wasn't there yet (build_structural_atlas failed earlier).
    NoAtlas,
    /// Triage failed (already logged with detail).
    Failed { reason: String },
}

/// Read the structural atlas at `<indexes_dir>/<corpus_id>/atlas/`,
/// rank in-corpus entities by Vital Articles tier (curator prior)
/// then by inbound + outbound link degree, and persist the
/// top-`budget` canonical names to `<corpus>/triage-candidates.json`.
///
/// ## Scoring
///
/// `score = (6 - tier) * BIG + centrality`, where `BIG` is large
/// enough to guarantee strict tier ordering. `tier` comes from the
/// bundled L1-L5 lists (see [`vital_tier`]); off-list entities
/// score on centrality alone (effective tier = 6, prior = 0).
///
/// Effect on a Wikipedia-scale atlas: top-1000 deterministically
/// includes every L1+L2+L3 article that's in-corpus (1,113 articles
/// total — at budget 1000 the long tail of L3 spills into L4 by
/// centrality), then top L4 by centrality, then bare-centrality
/// L5 / off-list. Without the prior, top-1000 was dominated by
/// disambiguation pages, list-of- pages, and templated geo
/// stubs — high-degree but content-thin.
///
/// ## Schema
///
/// Output matches `sovereign enrich init --include-articles <path>`:
/// `{ "schema_version": 1, "corpus_id": …, "top_in_corpus_by_centrality": [...] }`.
/// The schema_version stays 1 because the consumer only reads the
/// `top_in_corpus_by_centrality` array; new diagnostic fields land
/// alongside without bumping the contract.
pub async fn build_triage_candidates(
    corpus_id: &str,
    indexes_dir: PathBuf,
    budget: usize,
) -> TriageOutcome {
    let atlas_dir = indexes_dir.join(corpus_id).join("atlas");
    if !atlas_dir.join("atoms.json").exists() {
        return TriageOutcome::NoAtlas;
    }

    let started = std::time::Instant::now();
    let atoms = match read_atlas_atoms(&atlas_dir) {
        Ok(a) => a,
        Err(e) => {
            return TriageOutcome::Failed {
                reason: format!("read atoms.json: {e}"),
            }
        }
    };
    let edges = match read_atlas_edges(&atlas_dir) {
        Ok(e) => e,
        Err(e) => {
            return TriageOutcome::Failed {
                reason: format!("read edges.json: {e}"),
            }
        }
    };

    // Index entities, splitting placeholders from in-corpus.
    struct Ent {
        canonical_name: String,
        is_placeholder: bool,
    }
    let mut by_id: HashMap<String, Ent> = HashMap::with_capacity(atoms.atoms.len());
    for atom in &atoms.atoms {
        if let AtomEnvelope::Entity(e) = atom {
            by_id.insert(
                e.id.as_str().to_string(),
                Ent {
                    canonical_name: e.canonical_name.clone(),
                    is_placeholder: e.description.is_empty() && e.salience == 0.0,
                },
            );
        }
    }

    // Inbound + outbound degree per entity id.
    let mut inbound: HashMap<String, u32> = HashMap::with_capacity(by_id.len());
    let mut outbound: HashMap<String, u32> = HashMap::with_capacity(by_id.len());
    for edge in &edges.edges {
        *outbound.entry(edge.source.as_str().to_string()).or_insert(0) += 1;
        *inbound.entry(edge.target.as_str().to_string()).or_insert(0) += 1;
    }

    // Load adaptive query-bump map (Phase B2) if persisted alongside
    // the atlas. Missing → empty (no adaptive prior yet). Counts are
    // additive within tier — they don't promote across tier
    // boundaries.
    let bumps = read_triage_bumps(&atlas_dir);

    // Each user-query match is worth this many "centrality points."
    // 10 is calibrated against typical Wikipedia centrality (median
    // ~10, p99 ~10K): one query is one centrality unit, ten queries
    // promote a moderate-centrality article above its peers, 100
    // queries pull a long-tail article past most of its tier.
    const BUMP_WEIGHT: u64 = 10;

    // Rank in-corpus by (Vital Articles tier, centrality + bumps).
    // Tier comes from the bundled L1-L5 prior; missing → tier 6
    // (off-list). The tier weight is large enough to guarantee
    // strict ordering — within a tier, centrality + bumps break ties.
    //
    // BIG = max(centrality) + max(bumps*BUMP_WEIGHT) + 1. Picking
    // u32::MAX + 1 leaves room for ~2^31 bump points, far past
    // realistic usage on a single corpus.
    const TIER_WEIGHT: u64 = (u32::MAX as u64) + 1;
    struct Ranked {
        canonical_name: String,
        tier: u8, // 1..=5 for vital, 6 for off-list
        centrality: u32,
        bumps: u64,
    }
    let mut ranked: Vec<Ranked> = by_id
        .iter()
        .filter(|(_, e)| !e.is_placeholder)
        .map(|(id, e)| {
            let centrality = inbound.get(id).copied().unwrap_or(0)
                + outbound.get(id).copied().unwrap_or(0);
            let tier = vital_tier(&e.canonical_name).unwrap_or(6);
            let bumps = bump_count_for(&bumps, &e.canonical_name);
            Ranked {
                canonical_name: e.canonical_name.clone(),
                tier,
                centrality,
                bumps,
            }
        })
        .collect();
    let score = |r: &Ranked| -> u64 {
        (6u64 - r.tier as u64) * TIER_WEIGHT
            + r.centrality as u64
            + r.bumps.saturating_mul(BUMP_WEIGHT)
    };
    ranked.sort_by(|a, b| {
        score(b)
            .cmp(&score(a))
            .then_with(|| a.canonical_name.cmp(&b.canonical_name))
    });
    ranked.truncate(budget);

    // Tier histogram on the kept set — useful telemetry for both
    // post-install logs and `corpus status`.
    let mut tier_counts = [0usize; 6]; // [0]=L1, [1]=L2, ..., [5]=off-list
    let mut bumped_picks = 0usize;
    for r in &ranked {
        tier_counts[(r.tier as usize) - 1] += 1;
        if r.bumps > 0 {
            bumped_picks += 1;
        }
    }

    let picked: Vec<String> = ranked.iter().map(|r| r.canonical_name.clone()).collect();
    let n = picked.len();

    let payload = serde_json::json!({
        "schema_version": 1,
        "corpus_id": corpus_id,
        "budget": budget,
        "top_in_corpus_by_centrality": picked,
        // Diagnostic: per-tier counts so an operator can sanity-
        // check the prior took effect. Consumed by `corpus status`
        // and surfaced in tracing logs from the post-install hook.
        "tier_breakdown": {
            "l1": tier_counts[0],
            "l2": tier_counts[1],
            "l3": tier_counts[2],
            "l4": tier_counts[3],
            "l5": tier_counts[4],
            "off_list": tier_counts[5],
        },
        // How many of the picks got an adaptive bump from the user's
        // own query history (Phase B2). When > 0 the triage queue
        // has incorporated lived usage; when 0 the rebuild was
        // pre-bump or no queries had landed yet.
        "bumped_picks": bumped_picks,
    });
    let out_path = indexes_dir.join(corpus_id).join("triage-candidates.json");
    if let Err(e) = write_atomic_json(&out_path, &payload) {
        return TriageOutcome::Failed {
            reason: format!("write triage-candidates.json: {e}"),
        };
    }
    TriageOutcome::Built {
        path: out_path,
        in_corpus_picked: n,
        elapsed_secs: started.elapsed().as_secs_f64(),
    }
}

/// Read the persisted query-bump map at `<atlas_dir>/triage_bumps.json`.
/// Returns an empty map if the file is missing, malformed, or has a
/// future schema we don't recognise — adaptive priors are best-effort
/// telemetry, not a load-bearing contract.
fn read_triage_bumps(atlas_dir: &Path) -> HashMap<String, u64> {
    use corpus_engine::filters::normalize_title;
    let path = atlas_dir.join("triage_bumps.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    #[derive(serde::Deserialize)]
    struct File {
        schema_version: u32,
        bumps: HashMap<String, u64>,
    }
    let parsed: File = match serde_json::from_str(&raw) {
        Ok(f) => f,
        Err(_) => return HashMap::new(),
    };
    if parsed.schema_version != 1 {
        return HashMap::new();
    }
    // Pre-normalise keys so the lookup at scoring time is a direct
    // hash hit. The atlas Entity canonical_name and the runtime-
    // recorded bump key both pass through `normalize_title`.
    parsed
        .bumps
        .into_iter()
        .map(|(k, v)| (normalize_title(&k), v))
        .collect()
}

fn bump_count_for(bumps: &HashMap<String, u64>, canonical_name: &str) -> u64 {
    if bumps.is_empty() {
        return 0;
    }
    let key = corpus_engine::filters::normalize_title(canonical_name);
    bumps.get(&key).copied().unwrap_or(0)
}

/// Suffix appended to the source corpus id to derive the Tier-2
/// workspace name. `wikipedia` → `wikipedia-tier2`. Stable so the
/// daemon's resume scan can find unfinished work on boot.
pub const TIER2_WORKSPACE_SUFFIX: &str = "-tier2";

/// Sentinel file dropped into a workspace dir the moment the
/// post-install hook creates it. The daemon's resume scan only
/// re-spawns workspaces with this marker — manual workspaces
/// (created by `sovereign enrich init`) are left to the operator.
pub const AUTO_MANAGED_MARKER: &str = ".auto_managed";

/// Outcome of a Tier-2 extraction launch.
#[derive(Debug)]
pub enum Tier2LaunchOutcome {
    /// `enrich init` ran successfully and `enrich extract --full
    /// --resume` was spawned in the background. The PID lets the
    /// caller surface "extracting" state to status endpoints.
    Spawned {
        workspace_id: String,
        log_path: PathBuf,
        pid: u32,
    },
    /// Already complete — every chapter in the workspace's checkpoint
    /// is success or skipped. No new process spawned. The
    /// `chapters_total` / `chapters_done` fields lets callers report
    /// final state.
    AlreadyComplete {
        workspace_id: String,
        chapters_done: usize,
        chapters_total: usize,
    },
    /// Phase C3 — local extraction was deliberately skipped because
    /// a mesh peer already has a deeper atlas. The operator (or a
    /// future automated pull) is expected to fetch the peer's
    /// canonical via the mesh sync surface; until that happens the
    /// local atlas stays at whatever depth the structural pass
    /// produced, and atlas grounding still works on those entries.
    DeferredToPeer {
        peer_name: String,
        peer_tier2_count: u64,
        local_tier2_count: u64,
    },
    /// Init failed (e.g. corpus not installed, triage missing).
    InitFailed { reason: String },
    /// `enrich init` ran but spawning `extract` failed.
    SpawnFailed { reason: String },
}

/// Launch — but don't await — the Tier-2 extraction pipeline against
/// `source_corpus_id`. Idempotent on workspace existence: if the
/// workspace's `chapters.json` is already present and the checkpoint
/// covers every chapter, returns `AlreadyComplete`. If the workspace
/// is partially done, the spawned `extract --resume` picks up
/// where it left off.
///
/// `peer_advice` (Phase C3) is `Some` when a mesh peer already has a
/// deeper atlas; in that case we short-circuit with `DeferredToPeer`
/// rather than burning local tokens + days of wall-clock to reproduce
/// the peer's work. Pulling the peer's atlas is a separate operator
/// step today (`sovereign mesh canonical-pull`); auto-pull is a
/// follow-up.
///
/// The subprocess inherits no stdin and writes stdout + stderr to
/// `<workspace>/extraction.log` so daemon logs stay clean. The
/// process is detached from the daemon's reaper — if the daemon
/// dies, the extract child becomes a zombie until reaped on next
/// daemon boot via the resume scan.
pub async fn launch_tier2_extraction(
    source_corpus_id: &str,
    triage_path: PathBuf,
    cli_binary: PathBuf,
    enrichment_dir: PathBuf,
    indexes_dir: PathBuf,
) -> Tier2LaunchOutcome {
    launch_tier2_extraction_with_advice(
        source_corpus_id,
        triage_path,
        cli_binary,
        enrichment_dir,
        indexes_dir,
        None,
    )
    .await
}

/// Variant that takes an optional [`PeerAtlasPullCandidate`]. Used
/// by the post-install hook (which has access to mesh state) to
/// short-circuit local extraction when a peer leads on Tier-2 by
/// at least [`crate::atlas_peer_advice::MIN_PEER_LEAD`].
pub async fn launch_tier2_extraction_with_advice(
    source_corpus_id: &str,
    triage_path: PathBuf,
    cli_binary: PathBuf,
    enrichment_dir: PathBuf,
    indexes_dir: PathBuf,
    peer_advice: Option<crate::atlas_peer_advice::PeerAtlasPullCandidate>,
) -> Tier2LaunchOutcome {
    if let Some(advice) = peer_advice {
        return Tier2LaunchOutcome::DeferredToPeer {
            peer_name: advice.peer_name,
            peer_tier2_count: advice.peer_tier2_count,
            local_tier2_count: advice.local_tier2_count,
        };
    }
    let workspace_id = format!("{source_corpus_id}{TIER2_WORKSPACE_SUFFIX}");
    let workspace_dir = enrichment_dir.join(&workspace_id);
    let chapters_manifest_path = indexes_dir.join(&workspace_id).join("chapters.json");

    // Already complete?
    if let Some((done, total)) = checkpoint_progress(&workspace_dir, &chapters_manifest_path) {
        if done == total && total > 0 {
            return Tier2LaunchOutcome::AlreadyComplete {
                workspace_id,
                chapters_done: done,
                chapters_total: total,
            };
        }
    }

    // Init the workspace if config.json is missing. Skip otherwise
    // — the extract --resume below picks up wherever the existing
    // checkpoint left off.
    let config_exists = workspace_dir.join("config.json").exists();
    if !config_exists {
        let init_status = tokio::process::Command::new(&cli_binary)
            .args([
                "enrich",
                "init",
                &workspace_id,
                "--from-corpus",
                source_corpus_id,
                "--include-articles",
                triage_path
                    .to_str()
                    .unwrap_or_else(|| "/dev/null"),
                "--pipeline",
                "referential_atlas",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await;
        match init_status {
            Ok(out) if out.status.success() => {
                // Drop the auto-managed sentinel so the daemon's
                // boot-time resume scan picks this workspace up but
                // skips manual workspaces operators created with
                // `sovereign enrich init` directly.
                let _ = std::fs::write(workspace_dir.join(AUTO_MANAGED_MARKER), "");
            }
            Ok(out) => {
                return Tier2LaunchOutcome::InitFailed {
                    reason: format!(
                        "enrich init exit {}: {}",
                        out.status,
                        String::from_utf8_lossy(&out.stderr)
                            .lines()
                            .last()
                            .unwrap_or("(no stderr)")
                    ),
                }
            }
            Err(e) => {
                return Tier2LaunchOutcome::InitFailed {
                    reason: format!("enrich init spawn failed: {e}"),
                }
            }
        }
    }

    // Spawn `extract --full --resume` and detach. Logs go to
    // <workspace>/extraction.log so they don't pollute daemon
    // stdout / stderr.
    let log_path = workspace_dir.join("extraction.log");
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log_file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(e) => {
            return Tier2LaunchOutcome::SpawnFailed {
                reason: format!("open extraction.log: {e}"),
            }
        }
    };
    let stderr_file = match log_file.try_clone() {
        Ok(f) => f,
        Err(e) => {
            return Tier2LaunchOutcome::SpawnFailed {
                reason: format!("clone log handle: {e}"),
            }
        }
    };
    let spawn = std::process::Command::new(&cli_binary)
        .args(["enrich", "extract", &workspace_id, "--full", "--resume"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(stderr_file))
        .env("RUST_LOG", "info")
        .spawn();
    match spawn {
        Ok(child) => Tier2LaunchOutcome::Spawned {
            workspace_id,
            log_path,
            pid: child.id(),
        },
        Err(e) => Tier2LaunchOutcome::SpawnFailed {
            reason: format!("enrich extract spawn: {e}"),
        },
    }
}

/// Read `<workspace>/runs/_phase1_checkpoint.jsonl` and return
/// `(distinct_chapters_processed, total_chapters)`. The chapters
/// manifest lives at `<indexes_dir>/<workspace_id>/chapters.json`
/// (per `enrich_cmd::paths::chapters_manifest_path`), so the caller
/// passes both paths. `None` when the chapters manifest is missing
/// or unparseable.
pub fn checkpoint_progress(
    workspace_dir: &Path,
    chapters_manifest_path: &Path,
) -> Option<(usize, usize)> {
    let chapters: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(chapters_manifest_path).ok()?).ok()?;
    let total = chapters.get("chapters")?.as_array()?.len();

    let ckpt_path = workspace_dir.join("runs").join("_phase1_checkpoint.jsonl");
    let ckpt_text = std::fs::read_to_string(&ckpt_path).unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    for line in ckpt_text.lines() {
        if line.is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(id) = rec.get("chapter_id").and_then(|v| v.as_str()) {
                seen.insert(id.to_string());
            }
        }
    }
    Some((seen.len(), total))
}

/// On daemon boot, find every Tier-2 workspace under
/// `enrichment_dir` whose checkpoint is incomplete and re-spawn
/// `enrich extract --full --resume` for each. Idempotent — already-
/// complete workspaces are left alone, and spawning twice is safe
/// because `extract --resume` skips chapters already in the
/// checkpoint.
pub async fn resume_inflight_tier2(
    enrichment_dir: PathBuf,
    indexes_dir: PathBuf,
    cli_binary: PathBuf,
) -> Vec<Tier2LaunchOutcome> {
    let mut outcomes = Vec::new();
    let entries = match std::fs::read_dir(&enrichment_dir) {
        Ok(rd) => rd,
        Err(_) => return outcomes,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(TIER2_WORKSPACE_SUFFIX) {
            continue;
        }
        // Auto-managed sentinel — we only resume workspaces that
        // the post-install hook created. A manual workspace
        // (`sovereign enrich init <name>-tier2`) won't have this
        // marker and is left to the operator.
        if !path.join(AUTO_MANAGED_MARKER).exists() {
            continue;
        }
        let source_corpus_id = name.trim_end_matches(TIER2_WORKSPACE_SUFFIX);
        let chapters_manifest_path = indexes_dir.join(name).join("chapters.json");
        let Some((done, total)) = checkpoint_progress(&path, &chapters_manifest_path) else {
            continue;
        };
        if total == 0 || done >= total {
            continue;
        }
        // The triage file is keyed off the source corpus, not the
        // workspace. init won't actually run (config.json exists),
        // but pass a sensible path so debug logs aren't misleading.
        let triage = indexes_dir
            .join(source_corpus_id)
            .join("triage-candidates.json");
        outcomes.push(
            launch_tier2_extraction(
                source_corpus_id,
                triage,
                cli_binary.clone(),
                enrichment_dir.clone(),
                indexes_dir.clone(),
            )
            .await,
        );
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::enrichment::atlas::{
        atoms::{AtomEnvelope, AtomId, AtomsFile, ChunkRef, Entity},
        edges::{Edge, EdgeId, EdgeProvenance, EdgeType, EdgesFile},
    };
    use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

    /// Build a synthetic structural atlas under `<dir>/<corpus>/atlas/`
    /// containing one L1 entity (low centrality) and a flotilla of
    /// off-list entities with very high centrality. The L1 should
    /// outrank the noise once the tier prior is applied.
    fn write_synthetic_atlas(dir: &Path, corpus_id: &str) -> std::io::Result<()> {
        let atlas_dir = dir.join(corpus_id).join("atlas");
        std::fs::create_dir_all(&atlas_dir)?;

        // L1: "Earth" with salience 1.0 (so not classified as
        // placeholder) and modest centrality.
        let earth = Entity {
            id: AtomId::entity(1),
            canonical_name: "Earth".into(),
            aliases: Vec::new(),
            entity_type: EntityType::Concept,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "L1 vital article".into(),
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Structural,
            affiliation: None,
            role: None,
            participants: Vec::new(),
        };
        // Off-list noise: 5 entities with massive centrality (each
        // sourcing 100 edges into earth or each other).
        let mut atoms: Vec<AtomEnvelope> = vec![AtomEnvelope::Entity(earth)];
        for i in 2..=6 {
            atoms.push(AtomEnvelope::Entity(Entity {
                id: AtomId::entity(i),
                canonical_name: format!("Random Page {i}"),
                aliases: Vec::new(),
                entity_type: EntityType::Concept,
                first_appearance: ChunkRef::new("sec_0002", None),
                description: "off-list noise".into(),
                salience: 1.0,
                enrichment_depth: EnrichmentDepth::Structural,
                affiliation: None,
                role: None,
                participants: Vec::new(),
            }));
        }

        // Edges: Earth gets 5 inbound. Random Pages each get 100
        // outbound (to each other in a ring). Without the tier prior
        // a Random Page would land at top.
        let mut edges = Vec::new();
        let mut next_edge = 0u64;
        for i in 2..=6 {
            edges.push(Edge {
                id: EdgeId::new(next_edge as usize),
                edge_type: EdgeType::Involves,
                source: AtomId::entity(i),
                target: AtomId::entity(1),
                evidence: Vec::new(),
                trigger_event: None,
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::WikilinkStructural,
            });
            next_edge += 1;
        }
        for i in 2..=6 {
            for _ in 0..100 {
                let target = ((i % 5) + 2) as usize;
                edges.push(Edge {
                    id: EdgeId::new(next_edge as usize),
                    edge_type: EdgeType::Involves,
                    source: AtomId::entity(i),
                    target: AtomId::entity(target),
                    evidence: Vec::new(),
                    trigger_event: None,
                    sub_question: None,
                    confidence: 1.0,
                    provenance: EdgeProvenance::WikilinkStructural,
                });
                next_edge += 1;
            }
        }

        let atoms_file = AtomsFile::new(atoms);
        let edges_file = EdgesFile::new(edges);
        std::fs::write(
            atlas_dir.join("atoms.json"),
            serde_json::to_vec_pretty(&atoms_file).unwrap(),
        )?;
        std::fs::write(
            atlas_dir.join("edges.json"),
            serde_json::to_vec_pretty(&edges_file).unwrap(),
        )?;
        Ok(())
    }

    #[tokio::test]
    async fn vital_tier_prior_promotes_l1_above_high_centrality_noise() {
        let tmp = tempfile::tempdir().unwrap();
        let corpus = "synthetic-vital";
        write_synthetic_atlas(tmp.path(), corpus).unwrap();

        let outcome =
            build_triage_candidates(corpus, tmp.path().to_path_buf(), 3).await;
        let path = match outcome {
            TriageOutcome::Built { path, .. } => path,
            other => panic!("triage failed: {other:?}"),
        };

        let raw = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let picks = v["top_in_corpus_by_centrality"].as_array().unwrap();
        // Earth (L1) must be #1 even though Random Page entities have
        // 100× the centrality.
        assert_eq!(picks[0].as_str(), Some("Earth"));
        // Tier breakdown sanity: l1 == 1, off_list == 2 (budget 3,
        // one L1 + two off-list noise pages).
        assert_eq!(v["tier_breakdown"]["l1"].as_u64(), Some(1));
        assert_eq!(v["tier_breakdown"]["off_list"].as_u64(), Some(2));
        // No bumps file → bumped_picks = 0.
        assert_eq!(v["bumped_picks"].as_u64(), Some(0));
    }

    #[test]
    fn budget_override_roundtrips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("corpus").join("atlas");

        // No file → fall back to default.
        assert_eq!(
            effective_tier2_budget(tmp.path(), "corpus"),
            DEFAULT_TIER2_BUDGET
        );
        assert!(read_triage_budget(&atlas_dir).is_none());

        // Write override → the resolver picks it up.
        write_triage_budget(&atlas_dir, 5_000).unwrap();
        assert_eq!(read_triage_budget(&atlas_dir), Some(5_000));
        assert_eq!(effective_tier2_budget(tmp.path(), "corpus"), 5_000);

        // Nuking the file reverts to default.
        std::fs::remove_file(atlas_dir.join(TRIAGE_CONFIG_FILE)).unwrap();
        assert_eq!(
            effective_tier2_budget(tmp.path(), "corpus"),
            DEFAULT_TIER2_BUDGET
        );
    }

    /// Phase B2 end-to-end: a `triage_bumps.json` next to the atlas
    /// should reorder same-tier entries so heavily-bumped names rank
    /// above same-centrality unbumped ones.
    #[tokio::test]
    async fn query_bumps_reorder_same_tier_picks() {
        use std::collections::HashMap;
        let tmp = tempfile::tempdir().unwrap();
        let corpus = "synthetic-bumps";
        let atlas_dir = tmp.path().join(corpus).join("atlas");
        std::fs::create_dir_all(&atlas_dir).unwrap();

        // Two off-list entities with equal centrality. Without the
        // bump file, they tie-break alphabetically (Alpha first).
        // With Beta bumped 50 times, Beta should win.
        let mut atoms: Vec<AtomEnvelope> = Vec::new();
        for (i, name) in ["Alpha thing", "Beta thing"].iter().enumerate() {
            atoms.push(AtomEnvelope::Entity(Entity {
                id: AtomId::entity(i + 1),
                canonical_name: (*name).into(),
                aliases: Vec::new(),
                entity_type: EntityType::Concept,
                first_appearance: ChunkRef::new("sec_0001", None),
                description: "off-list".into(),
                salience: 1.0,
                enrichment_depth: EnrichmentDepth::Structural,
                affiliation: None,
                role: None,
                participants: Vec::new(),
            }));
        }
        // Equal centrality: each gets one inbound from the other.
        let edges = vec![
            Edge {
                id: EdgeId::new(0),
                edge_type: EdgeType::Involves,
                source: AtomId::entity(1),
                target: AtomId::entity(2),
                evidence: Vec::new(),
                trigger_event: None,
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::WikilinkStructural,
            },
            Edge {
                id: EdgeId::new(1),
                edge_type: EdgeType::Involves,
                source: AtomId::entity(2),
                target: AtomId::entity(1),
                evidence: Vec::new(),
                trigger_event: None,
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::WikilinkStructural,
            },
        ];
        std::fs::write(
            atlas_dir.join("atoms.json"),
            serde_json::to_vec_pretty(&AtomsFile::new(atoms)).unwrap(),
        )
        .unwrap();
        std::fs::write(
            atlas_dir.join("edges.json"),
            serde_json::to_vec_pretty(&EdgesFile::new(edges)).unwrap(),
        )
        .unwrap();

        // Sanity: pre-bump rank places Alpha first (alphabetical
        // tie-break on equal score).
        let outcome =
            build_triage_candidates(corpus, tmp.path().to_path_buf(), 2).await;
        let path = match outcome {
            TriageOutcome::Built { path, .. } => path,
            other => panic!("pre-bump triage failed: {other:?}"),
        };
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let picks = v["top_in_corpus_by_centrality"].as_array().unwrap();
        assert_eq!(picks[0].as_str(), Some("Alpha thing"));
        assert_eq!(v["bumped_picks"].as_u64(), Some(0));

        // Drop a bumps file giving Beta 50 hits.
        let mut bumps: HashMap<String, u64> = HashMap::new();
        bumps.insert("Beta thing".into(), 50);
        let bumps_payload = serde_json::json!({
            "schema_version": 1,
            "bumps": bumps,
        });
        std::fs::write(
            atlas_dir.join("triage_bumps.json"),
            serde_json::to_vec_pretty(&bumps_payload).unwrap(),
        )
        .unwrap();

        // Re-rank: Beta should now win, and bumped_picks should
        // reflect one bumped entry in the kept set.
        let outcome2 =
            build_triage_candidates(corpus, tmp.path().to_path_buf(), 2).await;
        let path2 = match outcome2 {
            TriageOutcome::Built { path, .. } => path,
            other => panic!("post-bump triage failed: {other:?}"),
        };
        let v2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path2).unwrap()).unwrap();
        let picks2 = v2["top_in_corpus_by_centrality"].as_array().unwrap();
        assert_eq!(picks2[0].as_str(), Some("Beta thing"));
        assert_eq!(v2["bumped_picks"].as_u64(), Some(1));
    }
}

fn write_atomic_json(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("path has no parent: {}", path.display()),
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("atlas-postinstall")
    ));
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
