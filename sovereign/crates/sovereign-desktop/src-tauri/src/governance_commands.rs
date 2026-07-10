// SPDX-License-Identifier: AGPL-3.0-or-later
//! Desktop governance surface — the Tauri layer over a corpus's
//! event-sourced common law (FR-9). This is the "one UX panel" the
//! governance thesis ("one recipe + one pure fold + one UX panel") was
//! missing: the CLI `svrn govern` verbs and this module write through the
//! *same* corpus-engine library (`GovernanceView` + `GovernanceOplog`),
//! so a decision the desktop appends is seen by `govern ask`'s active-set
//! retrieval filter and vice-versa.
//!
//! Design commitments (see `docs`/plan):
//!   - The app *reconciles* law, never authors it: there is no "add rule"
//!     command. Rules come only from the enriched documents.
//!   - Friction is proportional to authority: `dismiss` is one call with
//!     an optional note (detector noise is the steward's call); `resolve`
//!     and `accept` carry a rationale (the community's decision).
//!   - Living governance: every adjudication records its endpoint rule
//!     pair, so it survives the weekly atlas rebuild that renumbers edge
//!     ids (see `governance::PairKey` / the view join). The post-build
//!     hook always runs migrate-ids *then* seed so rule ids are stable.
//!   - Glass-box: the read model surfaces integrity issues rather than
//!     silently dropping a dangling decision.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use serde::Serialize;
use tauri::State;

use corpus_engine::enrichment::atlas::migrate_ids::migrate_atlas_ids;
use corpus_engine::enrichment::atlas::{read_atlas_atoms, AtomEnvelope};
use corpus_engine::enrichment::governance_view::section_titles;
use corpus_engine::enrichment::{
    GovernanceOp, GovernanceOpKind, GovernanceOplog, GovernanceView, TensionDisposition,
};

use crate::state::AppState;

/// Serializes the desktop's own oplog appends. The append is
/// open-append-close with no advisory lock (per corpus-engine), so two
/// concurrent desktop writers could interleave a line and its newline.
/// One process-wide mutex removes that race for the desktop; cross-process
/// CLI concurrency stays out of scope for the single-steward pilot.
static APPEND_LOCK: Mutex<()> = Mutex::new(());

/// `~/.sovereign` (or the configured data dir). Mirrors the idiom in
/// `enrich_commands::recipe_enrich_init_from_corpus`.
fn data_dir() -> PathBuf {
    sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        })
}

/// The corpus index root — `<data_dir>/indexes/<corpus_id>` — where
/// `chapters.json` lives and whose `atlas/` subdir holds the graph +
/// oplog. Matches the daemon's `CorpusEngine::index_dir().join(cid)`.
fn index_root(corpus_id: &str) -> PathBuf {
    data_dir().join("indexes").join(corpus_id)
}

/// `<index_root>/atlas` — where `atoms.json`, `edges.json`, and
/// `governance_oplog.jsonl` live.
fn atlas_dir(corpus_id: &str) -> PathBuf {
    index_root(corpus_id).join("atlas")
}

/// The actor stamped on human adjudications (INV-2 requires a `human:`
/// prefix on every non-`AssertRule` op). Single-steward pilot: the OS
/// user names the hand; the *rationale* carries the community's
/// authority. Per-member identity is deferred (no desktop user model).
fn actor() -> String {
    let who = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "steward".to_string());
    format!("human:{who}")
}

/// Unix seconds now — stamped on appended ops.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Append ops to a corpus's oplog under the process-wide append lock.
/// Returns the appended op ids (for the frontend's undo affordance).
fn append_ops(dir: &Path, ops: &[GovernanceOp]) -> Result<Vec<String>, String> {
    let _guard = APPEND_LOCK.lock().map_err(|_| "append lock poisoned")?;
    let oplog = GovernanceOplog::new(dir);
    let mut ids = Vec::with_capacity(ops.len());
    for op in ops {
        oplog
            .append(op)
            .map_err(|e| format!("appending governance op: {e}"))?;
        ids.push(op.id.as_str().to_string());
    }
    Ok(ids)
}

// ── Per-corpus vocabulary (recipe labels) ────────────────────

/// The recipe's `[enrichment.ontology.vocabulary]` terms, so the panel
/// speaks the community's language ("rule" / "conflict" / …) rather than
/// "tension edge". Every field optional; the UI falls back to generic
/// defaults. Not persisted into the atlas — read from the recipe.
#[derive(Debug, Clone, Default, Serialize)]
pub struct VocabularyPayload {
    pub position_term: Option<String>,
    pub tension_term: Option<String>,
    pub concern_term: Option<String>,
    pub evidence_term: Option<String>,
}

fn read_vocabulary(corpus_id: &str) -> Option<VocabularyPayload> {
    let recipe_path = data_dir()
        .join("recipes")
        .join(corpus_id)
        .join("recipe.toml");
    let recipe = corpus_engine::Recipe::from_file(&recipe_path).ok()?;
    let vocab = recipe.enrichment?.ontology?.vocabulary?;
    Some(VocabularyPayload {
        position_term: vocab.position_term,
        tension_term: vocab.tension_term,
        concern_term: vocab.concern_term,
        evidence_term: vocab.evidence_term,
    })
}

/// Entity-atom id → canonical name, for grouping rules by the topic
/// (scope entity) they govern in the "current rules" export.
fn scope_names(corpus_id: &str) -> HashMap<String, String> {
    let dir = atlas_dir(corpus_id);
    let Ok(atoms) = read_atlas_atoms(&dir) else {
        return HashMap::new();
    };
    atoms
        .atoms
        .iter()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e) => Some((e.id.as_str().to_string(), e.canonical_name.clone())),
            _ => None,
        })
        .collect()
}

/// Best-effort "documents changed since the atlas was last built" signal
/// driving the panel's staleness banner. Heuristic: the chunk/section
/// manifest (`chapters.json`, rewritten on ingest) is newer than the
/// extracted graph (`atoms.json`, written on enrich). A missing file
/// reads as "not stale" — a soft banner, never a hard gate; a false
/// negative just means the steward rebuilds from Explore instead, and a
/// rebuild is idempotent.
fn docs_changed_since_build(corpus_id: &str) -> bool {
    let root = index_root(corpus_id);
    let mtime = |p: PathBuf| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
    };
    let (Some(docs), Some(atoms)) = (
        mtime(root.join("chapters.json")),
        mtime(root.join("atlas").join("atoms.json")),
    ) else {
        return false;
    };
    docs > atoms
}

// ── get_view ─────────────────────────────────────────────────

/// Everything the Conflicts panel renders for a corpus, in one call.
#[derive(Debug, Clone, Serialize)]
pub struct GovernanceViewPayload {
    /// The joined read-model (rules + tensions + integrity issues).
    pub view: GovernanceView,
    /// section id → human title (e.g. `"Decision — 2026-03-14"`), for
    /// labelling each side of a conflict by its source document.
    pub section_titles: HashMap<String, String>,
    /// citation section id → numeric chunk id, for the "view passage"
    /// deep-link into the ReadingSurface. Best-effort.
    pub section_chunks: HashMap<String, u64>,
    /// scope entity id → canonical name, for topic grouping in exports.
    pub scope_names: HashMap<String, String>,
    /// Recipe vocabulary labels; `None` → UI uses generic defaults.
    pub vocabulary: Option<VocabularyPayload>,
    /// op id → decision metadata (timestamp, rationale, actor), so the
    /// panel can sort settled decisions most-recent-first and show *why*
    /// each was made — the living history a `TensionView` alone can't
    /// carry (the rationale lives on the oplog op, not the graph).
    pub decisions: HashMap<String, DecisionMeta>,
    /// Whether the documents changed since the last atlas build.
    pub docs_changed_since_build: bool,
}

/// Metadata for one governance decision (oplog op), keyed by op id in
/// [`GovernanceViewPayload::decisions`].
#[derive(Debug, Clone, Serialize)]
pub struct DecisionMeta {
    pub ts_unix: i64,
    /// The human rationale, if the op carried one (empty for `AssertRule`).
    pub rationale: String,
    /// `"human:<name>"` or `"seed"` — who authored the act.
    pub actor: String,
}

/// The rationale string an op kind carries (empty when it has none).
fn op_rationale(kind: &GovernanceOpKind) -> String {
    match kind {
        GovernanceOpKind::Supersede { rationale, .. }
        | GovernanceOpKind::RetractRule { rationale, .. }
        | GovernanceOpKind::ResolveTension { rationale, .. }
        | GovernanceOpKind::AcceptTension { rationale, .. }
        | GovernanceOpKind::DismissTension { rationale, .. }
        | GovernanceOpKind::Revert { rationale, .. } => rationale.clone(),
        GovernanceOpKind::AssertRule { .. } => String::new(),
    }
}

/// Load the full governance panel payload for a corpus.
#[tauri::command]
pub async fn governance_get_view(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<GovernanceViewPayload, String> {
    // Sync reads (view, titles, vocab, scopes, op times, staleness).
    let dir = atlas_dir(&corpus_id);
    let root = index_root(&corpus_id);
    let cid = corpus_id.clone();
    let (view, titles, scopes, vocab, decisions, docs_changed) =
        tokio::task::spawn_blocking(move || {
            let view = GovernanceView::from_atlas_dir(&dir)
                .map_err(|e| format!("reading governance view: {e}"))?;
            let titles = section_titles(&root);
            let scopes = scope_names(&cid);
            let vocab = read_vocabulary(&cid);
            let decisions: HashMap<String, DecisionMeta> = GovernanceOplog::new(&dir)
                .read_all()
                .unwrap_or_default()
                .into_iter()
                .map(|op| {
                    (
                        op.id.as_str().to_string(),
                        DecisionMeta {
                            ts_unix: op.ts_unix,
                            rationale: op_rationale(&op.kind),
                            actor: op.actor,
                        },
                    )
                })
                .collect();
            let docs_changed = docs_changed_since_build(&cid);
            Ok::<_, String>((view, titles, scopes, vocab, decisions, docs_changed))
        })
        .await
        .map_err(|e| format!("join: {e}"))??;

    // Resolve citation section ids → numeric chunk ids for deep-links.
    // Best-effort, mirroring `atlas_get_atom_detail`: a failure just
    // leaves passages non-clickable (the inline preview still renders).
    let mut section_chunks = HashMap::new();
    let unique_sections: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        view.rules
            .iter()
            .filter_map(|r| r.citation.as_ref().map(|c| c.chunk_id.clone()))
            .filter(|s| seen.insert(s.clone()))
            .collect()
    };
    if !unique_sections.is_empty() {
        // Clone the Arc out so the engine lock isn't held across index I/O
        // (mirrors `atlas_get_atom_detail`).
        let engine = state.corpus_engine.read().await.as_ref().map(Arc::clone);
        if let Some(engine) = engine {
            if let Ok(index) = engine.open_index_for_corpus(&corpus_id).await {
                if let Ok(map) = index.resolve_sections_to_chunks(&unique_sections).await {
                    section_chunks = map;
                }
            }
        }
    }

    Ok(GovernanceViewPayload {
        view,
        section_titles: titles,
        section_chunks,
        scope_names: scopes,
        vocabulary: vocab,
        decisions,
        docs_changed_since_build: docs_changed,
    })
}

// ── Adjudication commands ────────────────────────────────────

/// Look a tension up in a freshly-loaded view, returning its endpoint
/// rule ids so every appended adjudication is pair-durable by
/// construction.
fn tension_endpoints(
    view: &GovernanceView,
    tension_id: &str,
) -> Result<(corpus_engine::enrichment::atlas::AtomId, corpus_engine::enrichment::atlas::AtomId), String>
{
    view.tensions
        .iter()
        .find(|t| t.id.as_str() == tension_id)
        .map(|t| (t.rule_a.clone(), t.rule_b.clone()))
        .ok_or_else(|| {
            format!("no conflict `{tension_id}` in this corpus — the list may be out of date")
        })
}

fn resolve_at(
    dir: &Path,
    tension_id: &str,
    keep_rule_id: &str,
    rationale: &str,
) -> Result<Vec<String>, String> {
    let view =
        GovernanceView::from_atlas_dir(dir).map_err(|e| format!("reading governance view: {e}"))?;
    let (rule_a, rule_b) = tension_endpoints(&view, tension_id)?;
    let (keep, old) = if keep_rule_id == rule_a.as_str() {
        (rule_a.clone(), rule_b.clone())
    } else if keep_rule_id == rule_b.as_str() {
        (rule_b.clone(), rule_a.clone())
    } else {
        return Err(format!(
            "`{keep_rule_id}` is not one of this conflict's two rules"
        ));
    };
    let ts = now_unix();
    let who = actor();
    // The Supersede is the substance; ResolveTension records that this
    // tension was adjudicated via it, so a later undo reverts the bundle
    // atomically. Both carry the endpoint pair for rebuild-durability.
    let supersede = GovernanceOp::new(
        GovernanceOpKind::Supersede {
            new_rule: keep.clone(),
            old_rules: vec![old.clone()],
            rationale: rationale.to_string(),
        },
        ts,
        who.clone(),
    );
    let resolve = GovernanceOp::new(
        GovernanceOpKind::ResolveTension {
            tension: view
                .tensions
                .iter()
                .find(|t| t.id.as_str() == tension_id)
                .map(|t| t.id.clone())
                .expect("tension existed above"),
            via: supersede.id.clone(),
            endpoints: Some((rule_a, rule_b)),
            rationale: rationale.to_string(),
        },
        ts,
        who,
    );
    append_ops(dir, &[supersede, resolve])
}

/// Resolve a conflict by keeping one rule (the other is superseded).
/// Returns the two appended op ids.
#[tauri::command]
pub async fn governance_resolve(
    corpus_id: String,
    tension_id: String,
    keep_rule_id: String,
    rationale: String,
) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || {
        resolve_at(&atlas_dir(&corpus_id), &tension_id, &keep_rule_id, &rationale)
    })
    .await
    .map_err(|e| format!("join: {e}"))?
}

fn accept_at(dir: &Path, tension_id: &str, rationale: &str) -> Result<Vec<String>, String> {
    if rationale.trim().is_empty() {
        return Err("an accepted conflict must record why both rules stand".into());
    }
    let view =
        GovernanceView::from_atlas_dir(dir).map_err(|e| format!("reading governance view: {e}"))?;
    let (rule_a, rule_b) = tension_endpoints(&view, tension_id)?;
    let edge = view
        .tensions
        .iter()
        .find(|t| t.id.as_str() == tension_id)
        .map(|t| t.id.clone())
        .expect("tension existed above");
    let op = GovernanceOp::new(
        GovernanceOpKind::AcceptTension {
            tension: edge,
            rationale: rationale.to_string(),
            endpoints: Some((rule_a, rule_b)),
        },
        now_unix(),
        actor(),
    );
    append_ops(dir, &[op])
}

/// Accept a conflict as known-and-tolerated (both rules remain in force).
#[tauri::command]
pub async fn governance_accept(
    corpus_id: String,
    tension_id: String,
    rationale: String,
) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || accept_at(&atlas_dir(&corpus_id), &tension_id, &rationale))
        .await
        .map_err(|e| format!("join: {e}"))?
}

fn dismiss_at(
    dir: &Path,
    tension_id: &str,
    rationale: Option<&str>,
) -> Result<Vec<String>, String> {
    let view =
        GovernanceView::from_atlas_dir(dir).map_err(|e| format!("reading governance view: {e}"))?;
    let (rule_a, rule_b) = tension_endpoints(&view, tension_id)?;
    let edge = view
        .tensions
        .iter()
        .find(|t| t.id.as_str() == tension_id)
        .map(|t| t.id.clone())
        .expect("tension existed above");
    let op = GovernanceOp::new(
        GovernanceOpKind::DismissTension {
            tension: edge,
            endpoints: Some((rule_a, rule_b)),
            rationale: rationale.unwrap_or("").to_string(),
        },
        now_unix(),
        actor(),
    );
    append_ops(dir, &[op])
}

/// Dismiss a conflict as detector noise (not a real contradiction).
/// One-click, optional note — distinct from `accept`.
#[tauri::command]
pub async fn governance_dismiss(
    corpus_id: String,
    tension_id: String,
    rationale: Option<String>,
) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || {
        dismiss_at(&atlas_dir(&corpus_id), &tension_id, rationale.as_deref())
    })
    .await
    .map_err(|e| format!("join: {e}"))?
}

fn undo_at(dir: &Path, tension_id: &str) -> Result<String, String> {
    let view =
        GovernanceView::from_atlas_dir(dir).map_err(|e| format!("reading governance view: {e}"))?;
    let tv = view
        .tensions
        .iter()
        .find(|t| t.id.as_str() == tension_id)
        .ok_or_else(|| format!("no conflict `{tension_id}` in this corpus"))?;
    // The view already resolved edge-id-then-pair matching; its
    // disposition names the winning adjudication op.
    let by = match &tv.disposition {
        TensionDisposition::Resolved { by }
        | TensionDisposition::Accepted { by }
        | TensionDisposition::Dismissed { by } => by.clone(),
        TensionDisposition::Open | TensionDisposition::Moot { .. } => {
            return Err("this conflict has no decision to undo".into());
        }
    };
    // Reconstruct the bundle to revert: the winning op, plus (for a
    // resolve) the Supersede it was authored via.
    let ops = GovernanceOplog::new(dir)
        .read_all()
        .map_err(|e| format!("reading governance oplog: {e}"))?;
    let winner = ops
        .iter()
        .find(|op| op.id == by)
        .ok_or_else(|| "the decision to undo is no longer in the log".to_string())?;
    let mut targets = vec![by.clone()];
    if let GovernanceOpKind::ResolveTension { via, .. } = &winner.kind {
        targets.push(via.clone());
    }
    let revert = GovernanceOp::new(
        GovernanceOpKind::Revert {
            targets,
            rationale: "undo from desktop".into(),
        },
        now_unix(),
        actor(),
    );
    let ids = append_ops(dir, std::slice::from_ref(&revert))?;
    Ok(ids.into_iter().next().unwrap_or_default())
}

/// Undo the current decision on a conflict (revert the adjudication
/// bundle atomically). Returns the appended revert op id.
#[tauri::command]
pub async fn governance_undo_tension(
    corpus_id: String,
    tension_id: String,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || undo_at(&atlas_dir(&corpus_id), &tension_id))
        .await
        .map_err(|e| format!("join: {e}"))?
}

// ── Seed + post-build (living-governance durability) ─────────

/// Establish/refresh the governed rule baseline: one idempotent
/// `AssertRule` per Claim atom (actor `"seed"`, INV-2-exempt). Skips
/// rules the oplog already governs, so it is safe to re-run after every
/// rebuild. Returns the count of newly-asserted rules.
fn seed_at(dir: &Path) -> Result<u32, String> {
    let atoms = read_atlas_atoms(dir)
        .map_err(|e| format!("reading atoms.json at {}: {e} — enrich the corpus first", dir.display()))?;
    let oplog = GovernanceOplog::new(dir);
    let already: std::collections::HashSet<_> = oplog
        .read_all()
        .map_err(|e| format!("reading governance oplog: {e}"))?
        .into_iter()
        .filter_map(|op| match op.kind {
            GovernanceOpKind::AssertRule { rule, .. } => Some(rule),
            _ => None,
        })
        .collect();
    let ts = now_unix();
    let mut new_ops = Vec::new();
    for env in &atoms.atoms {
        if let AtomEnvelope::Claim(c) = env {
            if already.contains(&c.id) {
                continue;
            }
            new_ops.push(GovernanceOp::new(
                GovernanceOpKind::AssertRule {
                    rule: c.id.clone(),
                    source_doc: None,
                },
                ts,
                "seed",
            ));
        }
    }
    let seeded = new_ops.len() as u32;
    if !new_ops.is_empty() {
        append_ops(dir, &new_ops)?;
    }
    Ok(seeded)
}

/// Manual re-seed (also exercised by tests). The automatic path is
/// [`governance_post_build`], called from the enrich-build completion
/// hook.
#[tauri::command]
pub async fn governance_seed(corpus_id: String) -> Result<u32, String> {
    tokio::task::spawn_blocking(move || seed_at(&atlas_dir(&corpus_id)))
        .await
        .map_err(|e| format!("join: {e}"))?
}

/// Whether a corpus's recipe declares it governance-managed
/// (`[enrichment] domain = "governance"`). Gates the enrich-build
/// post-hook so ordinary corpora are never migrated/seeded.
pub fn is_governance_corpus(corpus_id: &str) -> bool {
    let recipe_path = data_dir()
        .join("recipes")
        .join(corpus_id)
        .join("recipe.toml");
    corpus_engine::Recipe::from_file(&recipe_path)
        .ok()
        .and_then(|r| r.enrichment)
        .and_then(|e| e.domain)
        .is_some_and(|d| d.eq_ignore_ascii_case("governance"))
}

/// Run after an atlas build of a governance corpus. The order is
/// load-bearing for living governance: **migrate-ids THEN seed**, always.
/// migrate rewrites sequential atom ids to content-hash ids (stable
/// across rebuilds when rule text is unchanged), so the seeded
/// `AssertRule`s — and the rule refs in past Supersede/Resolve ops —
/// keep resolving week over week. Best-effort and idempotent: a failure
/// logs and returns, never blocking the build.
pub fn governance_post_build(corpus_id: &str) -> Result<u32, String> {
    post_build_at(&atlas_dir(corpus_id), corpus_id)
}

/// [`governance_post_build`] against an explicit atlas dir — the testable
/// core. `corpus_id` is still needed: it is hashed into every content-hash
/// atom id, so migrate must use the real one.
fn post_build_at(dir: &Path, corpus_id: &str) -> Result<u32, String> {
    match migrate_atlas_ids(dir, corpus_id, false) {
        Ok(summary) => tracing::info!(
            corpus_id,
            ?summary,
            "governance_post_build: migrated atom ids to content-hash"
        ),
        Err(e) => {
            // Non-fatal: a non-governance or already-content-hash atlas
            // still seeds fine; sequential ids just won't survive a
            // future rebuild (surfaced as a needs-attention issue then).
            tracing::warn!(corpus_id, error = %e, "governance_post_build: migrate-ids failed");
        }
    }
    seed_at(dir)
}

// ── Template recipe + export ─────────────────────────────────

/// Write a governance recipe (`[enrichment] type=atlas domain=governance`
/// + the generalized ontology guidance) for a freshly-added folder
/// corpus, so `recipe_enrich_init_from_corpus` selects the `custom_atlas`
/// pipeline and the post-build hook recognizes the corpus as
/// governance-managed. Validated by round-tripping through
/// `Recipe::from_file`. Returns the recipe path.
#[tauri::command]
pub async fn governance_write_recipe(
    corpus_id: String,
    display_name: String,
    source_path: String,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || write_recipe_sync(&corpus_id, &display_name, &source_path))
        .await
        .map_err(|e| format!("join: {e}"))?
}

fn write_recipe_sync(
    corpus_id: &str,
    display_name: &str,
    source_path: &str,
) -> Result<String, String> {
    let dir = data_dir().join("recipes").join(corpus_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating recipe dir: {e}"))?;
    let path = dir.join("recipe.toml");
    let toml = render_governance_recipe(corpus_id, display_name, source_path);
    std::fs::write(&path, &toml).map_err(|e| format!("writing recipe.toml: {e}"))?;
    // Validate: it must parse AND resolve to the custom-ontology path, or
    // enrichment would silently fall back to the literary pipeline.
    match corpus_engine::Recipe::from_file(&path) {
        Ok(r) if r.custom_ontology().is_some() => Ok(path.display().to_string()),
        Ok(_) => Err("governance recipe wrote but has no custom ontology — template bug".into()),
        Err(e) => Err(format!("governance recipe failed to parse: {e}")),
    }
}

/// Write arbitrary text to a path the user picked via the save dialog.
/// Pairs with the frontend's "Export current rules" / agenda save flow.
#[tauri::command]
pub async fn governance_export_write(dest_path: String, content: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        std::fs::write(&dest_path, content.as_bytes())
            .map_err(|e| format!("writing export to {dest_path}: {e}"))
    })
    .await
    .map_err(|e| format!("join: {e}"))?
}

/// Generalized governance ontology guidance — a domain-neutral
/// generalization of the maple-house probe recipe. Covers any community
/// or shared organization governed by founding documents plus dated
/// decisions (houses, co-ops, clubs, small orgs). The four
/// NOT-a-conflict discriminators and the "name one concrete moment" test
/// are kept verbatim — they are the detector's decoy-rejection logic and
/// are already domain-general.
pub const GOVERNANCE_ONTOLOGY_GUIDANCE: &str = r#"This corpus is the governing rules of a community or shared organization: founding documents (a charter, bylaws, or agreement) plus a series of dated meeting decisions that amend, extend, or override those documents over time. Treat it as living common law — later decisions can change earlier rules.

Extract, as claims, every NORMATIVE STATEMENT — each thing that is required, forbidden, or permitted. For each such rule, capture:
- its deontic force: whether it requires, forbids, or permits something;
- the single topic it governs — for example: guests, quiet hours, shared spaces, chores, money, membership, meetings, pets. Attribute each rule to that topic, so that all rules about the same topic are grouped together;
- any conditions or exceptions it carries (times, days, who it applies to, where it applies).

Identify the governed topics themselves as entities.

Surface TENSIONS between rules: a later decision that contradicts, narrows, or overrides an earlier rule, or any two rules that give incompatible guidance for the same situation. A tension is a genuine conflict in what is required, forbidden, or permitted for the same topic and situation — NOT merely two rules that happen to mention the same word. Two rules about different aspects of the same topic (for example, where a guest may park versus how many nights a guest may stay) are NOT in tension.

In particular, these pairs are NOT conflicts even when they share a topic — do not flag them:
- Two SEPARATE exemptions or exceptions to the same rule (one member excused for one reason, another excused for a different reason): each stands alone, and honoring one never forces breaking the other.
- A rule about one group of people versus a rule about a DIFFERENT group (visitors versus members): they do not bind the same person at the same moment.
- Rules that govern DIFFERENT places or resources (one room versus another; one shared resource versus another).
- An ADDITIVE rule that layers a step, label, or record on top of another: both can be followed at once.
Flag a conflict only when you can name one concrete moment in which a single member, in one place and at one time, would have to break one rule to follow the other.

Reader questions worth surfacing: What is the current rule about a given topic? Which founding provisions have been amended by a later decision?"#;

/// Render a minimal governance recipe for a folder corpus. The
/// acquire/extract/chunk blocks satisfy the parser and record
/// provenance; `enrich init --from-corpus` builds the atlas from the
/// installed index, not from these. The ontology block is what makes the
/// corpus governance-managed.
fn render_governance_recipe(corpus_id: &str, display_name: &str, source_path: &str) -> String {
    // TOML-escape the two interpolated free-text fields (backslash + quote).
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let name = esc(display_name);
    let path = esc(source_path);
    let guidance = GOVERNANCE_ONTOLOGY_GUIDANCE; // triple-quoted below; contains no """.
    format!(
        r#"# Generated by Sovereign Desktop — community governance template.
# Reconciles rules from documents; it never authors rules.

[corpus]
id = "{corpus_id}"
name = "{name}"
description = "Governing rules — founding documents plus dated decisions."
license = "none"
schema_version = 1

[acquire]
type = "local_file"
path = "{path}"

[extract]
type = "markdown"

[chunk]
type = "paragraph"
max_chars = 2000
overlap_chars = 200

[index]
fts = true
vector = true

[enrichment]
enabled = true
type = "atlas"
domain = "governance"

[enrichment.ontology]
guidance = """
{guidance}
"""

[enrichment.ontology.vocabulary]
position_term = "rule"
tension_term = "conflict"
concern_term = "community question"
evidence_term = "passage"
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::enrichment::atlas::{
        AtomEnvelope, AtomId, AtomsFile, ChunkRef, Claim, Edge, EdgeId, EdgeProvenance, EdgeType,
        EdgesFile,
    };
    use corpus_engine::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EpistemicStatus,
    };

    /// A rule to plant: sequential claim index, its normative text, and
    /// the section id its citation points at.
    struct Rule {
        idx: usize,
        text: &'static str,
        section: &'static str,
    }
    /// A conflict to plant between two rules (by claim index).
    struct Conflict {
        edge: usize,
        a: usize,
        b: usize,
        why: &'static str,
    }

    /// Write a governance atlas (atoms.json + edges.json) to `dir` exactly
    /// as the enrichment pipeline would — sequential ids, one Claim per
    /// rule, one Tension edge per conflict. This is what a *fresh* atlas
    /// build produces; `post_build_at` then migrates the ids to
    /// content-hash and seeds. Re-calling with the same rule TEXT (and
    /// new edge numbers) simulates the weekly rebuild.
    fn write_atlas(dir: &Path, rules: &[Rule], conflicts: &[Conflict]) {
        let claim = |r: &Rule| {
            AtomEnvelope::Claim(Claim {
                id: AtomId::claim(r.idx),
                content: r.text.into(),
                discourse_act: DiscourseAct::Enact,
                epistemic_status: EpistemicStatus::Confident,
                scope: ClaimScope::Contextual,
                evidence: vec![ChunkRef::new(r.section.to_string(), None)],
                quotable_excerpt: None,
                attributed_to: Some(AtomId::entity(1)),
                confidence: None,
                anchor: None,
                claim_kind: Some("requires".into()),
                concession_outcome: None,
                evidence_kind: None,
                enrichment_depth: EnrichmentDepth::Extracted,
            })
        };
        let atoms = AtomsFile::new(rules.iter().map(claim).collect());
        std::fs::write(dir.join("atoms.json"), serde_json::to_vec(&atoms).unwrap()).unwrap();

        let edges = EdgesFile::new(
            conflicts
                .iter()
                .map(|c| Edge {
                    id: EdgeId::new(c.edge),
                    edge_type: EdgeType::Tension,
                    source: AtomId::claim(c.a),
                    target: AtomId::claim(c.b),
                    evidence: Vec::new(),
                    trigger_event: None,
                    sub_question: Some(c.why.into()),
                    confidence: 0.85,
                    provenance: EdgeProvenance::Derived,
                })
                .collect(),
        );
        std::fs::write(dir.join("edges.json"), serde_json::to_vec(&edges).unwrap()).unwrap();
    }

    fn view(dir: &Path) -> GovernanceView {
        GovernanceView::from_atlas_dir(dir).unwrap()
    }
    fn open_count(dir: &Path) -> usize {
        view(dir).open_tensions().count()
    }
    /// The current disposition tag for a tension found by its crux text.
    fn disposition_by_why(dir: &Path, why: &str) -> String {
        let v = view(dir);
        let t = v.tensions.iter().find(|t| t.why.as_deref() == Some(why)).unwrap();
        serde_json::to_value(&t.disposition).unwrap()["disposition"]
            .as_str()
            .unwrap()
            .to_string()
    }
    fn tension_id_by_why(dir: &Path, why: &str) -> String {
        let v = view(dir);
        v.tensions
            .iter()
            .find(|t| t.why.as_deref() == Some(why))
            .unwrap()
            .id
            .as_str()
            .to_string()
    }

    // The Maple-House planted ground truth (generalized): three real
    // cross-section conflicts plus one lexical decoy.
    fn cycle1_rules() -> Vec<Rule> {
        vec![
            Rule { idx: 1, text: "Quiet hours begin at 11 PM every night.", section: "sec_charter_ii" },
            Rule { idx: 2, text: "Quiet hours begin at 10 PM on weeknights.", section: "sec_2026_02_10" },
            Rule { idx: 3, text: "Guests may stay up to two nights.", section: "sec_charter_i" },
            Rule { idx: 4, text: "No overnight guests are permitted.", section: "sec_2026_03_14" },
            Rule { idx: 5, text: "Whoever cooks cleans the kitchen.", section: "sec_charter_iii" },
            Rule { idx: 6, text: "The cook is excused from kitchen cleanup.", section: "sec_2026_04_02" },
            Rule { idx: 7, text: "Guests may park on the street.", section: "sec_2026_03_28" },
        ]
    }
    fn cycle1_conflicts() -> Vec<Conflict> {
        vec![
            Conflict { edge: 1, a: 1, b: 2, why: "When do quiet hours begin now?" },
            Conflict { edge: 2, a: 3, b: 4, why: "May a guest stay overnight?" },
            Conflict { edge: 3, a: 5, b: 6, why: "Who cleans the kitchen?" },
            // Decoy: lexical overlap on "guests", not a real conflict.
            Conflict { edge: 4, a: 3, b: 7, why: "Where do guests park?" },
        ]
    }

    #[test]
    fn full_governance_flow_survives_a_weekly_rebuild() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let corpus = "maple-house-e2e";

        // ── Cycle 1: fresh build → post-build (migrate + seed) ──
        write_atlas(dir, &cycle1_rules(), &cycle1_conflicts());
        let seeded = post_build_at(dir, corpus).unwrap();
        assert_eq!(seeded, 7, "one AssertRule per rule claim");

        // Migrate ran before seed: every governed rule id is content-hash,
        // not sequential — the property that lets rules survive a rebuild.
        let ops = GovernanceOplog::new(dir).read_all().unwrap();
        let asserted: Vec<_> = ops
            .iter()
            .filter_map(|o| match &o.kind {
                GovernanceOpKind::AssertRule { rule, .. } => Some(rule.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(asserted.len(), 7);
        assert!(
            asserted.iter().all(|id| id.is_content_hash()),
            "post-build must seed content-hash rule ids (got {asserted:?})"
        );

        assert_eq!(view(dir).active_rules().count(), 7);
        assert_eq!(open_count(dir), 4, "four surfaced conflicts, all open");

        // ── Triage + meeting: dismiss the decoy, resolve one, accept one ──
        let decoy = tension_id_by_why(dir, "Where do guests park?");
        dismiss_at(dir, &decoy, Some("parking vs stays — not a conflict")).unwrap();
        assert_eq!(disposition_by_why(dir, "Where do guests park?"), "dismissed");
        assert_eq!(open_count(dir), 3);

        // Resolve quiet-hours by keeping the 10 PM weeknight decision.
        let quiet = tension_id_by_why(dir, "When do quiet hours begin now?");
        let keep = {
            let v = view(dir);
            let t = v.tensions.iter().find(|t| t.id.as_str() == quiet).unwrap();
            // Keep whichever side is the 10 PM rule.
            if t.text_a.contains("10 PM") { t.rule_a.clone() } else { t.rule_b.clone() }
        };
        resolve_at(dir, &quiet, keep.as_str(), "House meeting — kept 10 PM").unwrap();
        assert_eq!(disposition_by_why(dir, "When do quiet hours begin now?"), "resolved");
        assert_eq!(open_count(dir), 2);

        // Accept the kitchen conflict as a tolerated contradiction.
        let kitchen = tension_id_by_why(dir, "Who cleans the kitchen?");
        accept_at(dir, &kitchen, "Both stand — cook's choice by custom").unwrap();
        assert_eq!(disposition_by_why(dir, "Who cleans the kitchen?"), "accepted");
        assert_eq!(open_count(dir), 1, "only the guest-overnight conflict remains open");

        // Empty rationale on accept is refused (friction = authority).
        assert!(accept_at(dir, &kitchen, "   ").is_err());

        // ── Undo round-trips: reopen the resolved conflict, then redo ──
        undo_at(dir, &quiet).unwrap();
        assert_eq!(disposition_by_why(dir, "When do quiet hours begin now?"), "open");
        assert_eq!(open_count(dir), 2, "undo reopened quiet-hours");
        resolve_at(dir, &quiet, keep.as_str(), "House meeting — kept 10 PM (redo)").unwrap();
        assert_eq!(open_count(dir), 1);

        // ── Cycle 2: WEEKLY REBUILD. Same rule texts (→ same content-hash
        //    ids after migrate) but every edge id is re-minted, plus a new
        //    decision that creates a genuinely new conflict. ──
        let mut rules2 = cycle1_rules();
        rules2.push(Rule {
            idx: 8,
            text: "Quiet hours begin at 9:30 PM on weeknights.",
            section: "sec_2026_07_19",
        });
        // Re-detect the same four conflicts under NEW edge numbers (11..14),
        // and surface the new 9:30-vs-10 conflict (edge 15).
        let conflicts2 = vec![
            Conflict { edge: 11, a: 1, b: 2, why: "When do quiet hours begin now?" },
            Conflict { edge: 12, a: 3, b: 4, why: "May a guest stay overnight?" },
            Conflict { edge: 13, a: 5, b: 6, why: "Who cleans the kitchen?" },
            Conflict { edge: 14, a: 3, b: 7, why: "Where do guests park?" },
            Conflict { edge: 15, a: 2, b: 8, why: "Is it 10 PM or 9:30 PM on weeknights?" },
        ];
        write_atlas(dir, &rules2, &conflicts2);
        let seeded2 = post_build_at(dir, corpus).unwrap();
        assert_eq!(seeded2, 1, "only the one new rule is freshly asserted");

        // The whole point: last week's decisions survived the rebuild.
        assert_eq!(
            disposition_by_why(dir, "When do quiet hours begin now?"),
            "resolved",
            "the resolved conflict stays resolved across the edge-id renumber"
        );
        assert_eq!(disposition_by_why(dir, "Who cleans the kitchen?"), "accepted");
        assert_eq!(disposition_by_why(dir, "Where do guests park?"), "dismissed");

        // The genuinely new conflict is open — and it is the ONLY open one.
        assert_eq!(
            disposition_by_why(dir, "Is it 10 PM or 9:30 PM on weeknights?"),
            "open"
        );
        assert_eq!(open_count(dir), 2, "new 9:30 conflict + the never-adjudicated overnight one");

        // No false "needs attention": a valid pair re-detected under a new
        // edge id is normal weekly variance, not drift.
        assert!(
            view(dir).issues.is_empty(),
            "no dangling-adjudication issues after a clean rebuild: {:?}",
            view(dir).issues
        );

        // And current law reflects the resolution: the 11 PM rule is dead.
        let v = view(dir);
        let eleven = v.rules.iter().find(|r| r.text.contains("11 PM")).unwrap();
        assert!(
            !matches!(eleven.status, corpus_engine::enrichment::RuleStatus::Active),
            "the superseded 11 PM rule is out of current law"
        );
    }

    /// Emits (with `REGEN_GOV_FIXTURE=1`) — or otherwise validates — the
    /// deterministic governance atlas the real-mode browser spec overlays
    /// onto an ingested corpus (`tests/e2e/real/global-setup.ts` →
    /// `governance.real.spec.ts`). Generating it from the SAME
    /// `write_atlas` + `post_build_at` the durability test drives means the
    /// checked-in fixture is a byte-for-byte real post-build atlas
    /// (content-hash rule ids, seeded oplog), not hand-authored JSON that
    /// could drift from the serde format.
    #[test]
    fn governance_real_fixture_is_valid_or_regenerated() {
        let out = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/e2e/real/fixtures/governance-atlas");

        if std::env::var("REGEN_GOV_FIXTURE").is_ok() {
            let tmp = tempfile::tempdir().unwrap();
            write_atlas(tmp.path(), &cycle1_rules(), &cycle1_conflicts());
            post_build_at(tmp.path(), "maple-house-real").unwrap();
            std::fs::create_dir_all(&out).unwrap();
            for f in ["atoms.json", "edges.json", "governance_oplog.jsonl"] {
                std::fs::copy(tmp.path().join(f), out.join(f))
                    .unwrap_or_else(|e| panic!("copying {f}: {e}"));
            }
            eprintln!("[regen] wrote governance real-mode fixture → {}", out.display());
            return;
        }

        // Not regenerating: if the fixture is checked in, it must parse as
        // the exact governance atlas the spec expects. (A checkout without
        // it — the fixture is committed, so this is rare — skips rather
        // than fails, since regen is a manual, offline step.)
        if !out.join("governance_oplog.jsonl").exists() {
            eprintln!(
                "[skip] governance real-mode fixture absent at {} — run with \
                 REGEN_GOV_FIXTURE=1 to generate",
                out.display()
            );
            return;
        }
        let view = GovernanceView::from_atlas_dir(&out).unwrap();
        assert_eq!(view.active_rules().count(), 7, "7 seeded rules");
        assert_eq!(view.open_tensions().count(), 4, "3 conflicts + 1 decoy, all open");
        // The four planted cruxes the spec drives against, by text.
        let cruxes: Vec<&str> = view.tensions.iter().filter_map(|t| t.why.as_deref()).collect();
        for want in [
            "When do quiet hours begin now?",
            "May a guest stay overnight?",
            "Who cleans the kitchen?",
            "Where do guests park?",
        ] {
            assert!(cruxes.contains(&want), "fixture missing crux {want:?}");
        }
    }

    #[test]
    fn rendered_recipe_parses_and_is_custom_ontology() {
        let toml = render_governance_recipe("my-house", "My House \"Coop\"", "/docs/rules");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recipe.toml");
        std::fs::write(&path, &toml).unwrap();
        let recipe = corpus_engine::Recipe::from_file(&path)
            .expect("generated governance recipe must parse");
        assert!(
            recipe.custom_ontology().is_some(),
            "recipe must resolve to the custom-ontology (governance) atlas path"
        );
        let enrichment = recipe.enrichment.expect("enrichment block");
        assert_eq!(enrichment.domain.as_deref(), Some("governance"));
        let vocab = enrichment
            .ontology
            .and_then(|o| o.vocabulary)
            .expect("vocabulary block");
        assert_eq!(vocab.tension_term.as_deref(), Some("conflict"));
    }
}
