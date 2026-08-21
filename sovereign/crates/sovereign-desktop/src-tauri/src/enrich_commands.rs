// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri command surface for atlas enrichment (read + install side).
//!
//! Enrichment BUILDS now run IN-PROCESS in the daemon (tiered
//! GliNER/RAPTOR during ingest + the post-install structural-atlas
//! hook), observed from the UI by polling `lc_enrichment_status`. The
//! old CLI-shell commands (`enrich_build_async`, `enrich_cancel_build`,
//! `enrich_init_for_local_corpus`, `recipe_enrich_init_from_corpus`,
//! `enrich_estimate`, `enrich_get_active_job`) were removed once every
//! desktop surface migrated to that path — `sovereign-cli` is not
//! bundled with the desktop, so those shell-outs exited 127 in shipped
//! builds.
//!
//! What remains here needs no subprocess:
//!   - `enrich_list_corpora` — inventory of the shared enrichment store.
//!   - `install_starter_corpus` — restore the Federalist starter snapshot.
//!   - `enrich_get_starter_questions` — mine starter chips from atoms.json.
//!   - `is_first_run` / `mark_first_run_complete` — onboarding marker.

use std::collections::HashSet;
use std::path::PathBuf;

use corpus_engine::enrichment::atlas::{read_atlas_atoms, AtomEnvelope};
use serde::Serialize;
// The enrichment store, shared with the CLI and the daemon's watched-folder
// driver (rung nc-16-shared-capability). This file used to carry its own
// inventory loop, its own `config.json` field names and its own path
// derivation; all three are gone.
use sovereign_enrichment_catalog::{catalog, paths, EnrichedCorpusSummary};
use tauri::AppHandle;

// ─── Command: enrich_list_corpora ────────────────────────────────────

/// Inventory of enrichment corpora on disk.
///
/// The listing, the config schema and the path layout all live in
/// `sovereign-enrichment-catalog`, below this host and below the CLI and the
/// daemon that write the same tree. Until 2026-08-20 this command walked
/// `read_dir` itself and pulled `pipeline_id` / `source_path` / `created_at`
/// out of an untyped JSON value by name — a fourth reader of a file it did not
/// own, rooted on an accessor the CLI disagreed with.
///
/// Errors flatten to a String for the Tauri boundary; an absent store is
/// `Ok(vec![])`, not an error, because the UI branches on length.
#[tauri::command]
pub async fn enrich_list_corpora() -> Result<Vec<EnrichedCorpusSummary>, String> {
    catalog::list_enriched_corpora().map_err(|e| e.to_string())
}

/// Result of [`install_starter_corpus`].
#[derive(Serialize)]
pub struct StarterInstallResult {
    pub corpus_id: String,
    /// True when the corpus was already present (no work done) — the
    /// caller can skip straight to chat.
    pub already_installed: bool,
}

/// Install the "Federalist Papers" starter corpus by downloading and restoring
/// its pre-enriched snapshot — no inference, ~162 KB, a few seconds.
///
/// The snapshot is distributed like every other corpus: a `.tar.zst` on
/// HuggingFace (`svrnmesh/federalist-starter`), embedded with
/// `qwen-embedding-0.6b` (the app's embed model), fetched via the shared
/// `BulkDownloader` and restored into `~/.svrnmesh/indexes` via the shared
/// snapshot-restore primitive — the SAME root the daemon + desktop read corpora
/// from. NO hardcoded paths: the data root resolves via the shared
/// `sovereign_enrichment_catalog::paths` accessors. The restore gates on the
/// snapshot's sha256 and refuses on an embedding-dimension mismatch.
/// Idempotent — returns early if the corpus is already present.
///
/// HF-only as of 2026-06-19: the snapshot is no longer bundled into the app. It
/// used to ship as a Tauri resource, but `tauri.release.conf.json`'s `resources`
/// ARRAY overrode the base config's resource MAP, silently dropping it from every
/// release build. Rather than re-bundle (and re-fight that clobber), it now rides
/// the same registry/HF rails as the rest of the catalog. First run needs network
/// — already true for the multi-GB model download that precedes it.
///
/// Dev override: `SOVEREIGN_STARTER_SNAPSHOT=<path>` points at a local archive
/// (e.g. the in-repo `resources/starter/federalist-starter.tar.zst`) for an
/// offline / no-network dev loop.
#[tauri::command]
pub async fn install_starter_corpus(app: AppHandle) -> Result<StarterInstallResult, String> {
    let _ = &app; // AppHandle no longer needed to resolve the bundled resource (HF download); kept for command signature stability.
    const STARTER_ID: &str = "federalist-starter";
    // The snapshot lives at
    // https://huggingface.co/datasets/svrnmesh/federalist-starter/resolve/main/federalist-starter.tar.zst
    const STARTER_HF_REPO: &str = "svrnmesh/federalist-starter";
    const STARTER_HF_FILENAME: &str = "federalist-starter.tar.zst";
    // sha256 of federalist-starter.tar.zst (gates restore against a corrupt or
    // tampered download — same value verified on the HF artifact).
    const STARTER_SHA256: &str = "dc189da612b9b01d412e7e0aca93cd0d550184cbb339fc85bbc76d3a1d57031f";

    // Idempotent: a resolved atoms.json ⇒ already restored + enriched.
    if paths::index_root(STARTER_ID)
        .join("atlas")
        .join("atoms.json")
        .exists()
    {
        return Ok(StarterInstallResult {
            corpus_id: STARTER_ID.to_string(),
            already_installed: true,
        });
    }

    // Resolve the snapshot archive. Dev escape hatch first (a local path for an
    // offline loop); otherwise download it from HuggingFace exactly like every
    // other corpus. The sha256 gate lives in restore_snapshot_archive below.
    let dev_override = std::env::var("SOVEREIGN_STARTER_SNAPSHOT")
        .ok()
        .filter(|p| !p.is_empty());
    let archive: PathBuf = match &dev_override {
        Some(p) => {
            let p = PathBuf::from(p);
            if !p.exists() {
                return Err(format!(
                    "SOVEREIGN_STARTER_SNAPSHOT points at {} which does not exist",
                    p.display()
                ));
            }
            tracing::info!(path = %p.display(), "starter corpus: using local snapshot (dev override)");
            p
        }
        None => {
            let url = format!(
                "https://huggingface.co/datasets/{STARTER_HF_REPO}/resolve/main/{STARTER_HF_FILENAME}"
            );
            // Conventional download cache, mirroring CorpusEngine::try_restore_prebuilt.
            let download_dir = paths::indexes_dir().join("_downloads");
            std::fs::create_dir_all(&download_dir).map_err(|e| {
                format!(
                    "create starter download dir {}: {e}",
                    download_dir.display()
                )
            })?;
            tracing::info!(url = %url, "starter corpus: downloading snapshot from HuggingFace");
            corpus_engine::acquirers::bulk_download::BulkDownloader::new(&url, true)
                .download(&download_dir, STARTER_ID, &None)
                .await
                .map_err(|e| format!("download starter snapshot from {url}: {e}"))?
        }
    };

    // restore_snapshot_archive is blocking (tar extract + streaming sha) — run it
    // off the async runtime. ~162 KB ⇒ milliseconds, but keep the hot path clean.
    let data_dir = paths::data_root();
    let archive_for_task = archive.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        corpus_engine::restore_snapshot_archive(
            &archive_for_task,
            &data_dir,
            STARTER_ID,
            Some(STARTER_SHA256),
            "qwen-embedding-0.6b",
            corpus_engine::DEFAULT_EMBED_DIM,
        )
    })
    .await
    .map_err(|e| format!("starter restore task join: {e}"))?
    .map_err(|e| format!("restore starter snapshot from {}: {e}", archive.display()))?;

    // Tidy the download cache (skip the dev override — we don't own that path).
    // Removing it forces a fresh, sha-gated re-download on any future reinstall,
    // so a stale cached archive can never mask an updated snapshot.
    if dev_override.is_none() {
        let _ = std::fs::remove_file(&archive);
    }

    tracing::info!(
        corpus_id = %STARTER_ID,
        index_dir = %outcome.index_dir.display(),
        "starter corpus restored from HuggingFace snapshot (no inference)"
    );
    Ok(StarterInstallResult {
        corpus_id: STARTER_ID.to_string(),
        already_installed: false,
    })
}

// ─── Command: enrich_get_starter_questions ───────────────────────────

#[derive(Debug, Serialize, Clone)]
pub struct StarterQuestion {
    pub text: String,
    pub atom_id: String,
    pub source_section: Option<String>,
    pub question_type: String,
}

/// Return up to `limit` starter questions mined from the atlas.
///
/// Heuristic (shipped question atoms lack a salience or `addressed_by`
/// field — verified against three live corpora). Ranking:
///
///   1. Length window 25..=220 chars (drops too-terse fragments and
///      run-on multi-clause questions).
///   2. Question-type preference, in order: Thematic, Interpretive,
///      Open, Factual, Rhetorical, Other.
///   3. Diversify by first `raised_at.chunk_id`: at most one question
///      per section in the returned set, as far as `limit` and corpus
///      size permit.
///
/// Returns an empty vec (NOT an error) when atoms.json is absent — the
/// UI branches on vec length to decide whether to fall back to
/// excerpt-based starters.
#[tauri::command]
pub async fn enrich_get_starter_questions(
    corpus_id: String,
    limit: usize,
) -> Result<Vec<StarterQuestion>, String> {
    let atlas_dir = paths::index_root(&corpus_id).join("atlas");
    if !atlas_dir.exists() {
        return Ok(Vec::new());
    }
    let atoms_file = read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("reading atoms.json under {}: {e}", atlas_dir.display()))?;

    let starters = rank_starter_questions(&atoms_file.atoms, limit);
    tracing::debug!(
        corpus_id = %corpus_id,
        total_atoms = atoms_file.atoms.len(),
        question_atoms = atoms_file.atoms.iter().filter(|a| matches!(a, AtomEnvelope::Question(_))).count(),
        returned = starters.len(),
        "enrich_get_starter_questions"
    );
    Ok(starters)
}

/// Core ranker. Separated from the Tauri command so unit tests can
/// feed it synthetic atom slices without touching the filesystem.
fn rank_starter_questions(atoms: &[AtomEnvelope], limit: usize) -> Vec<StarterQuestion> {
    if limit == 0 {
        return Vec::new();
    }
    // Tier score — lower is better.
    fn tier(q_type: &str) -> u8 {
        match q_type {
            "thematic" => 0,
            "interpretive" => 1,
            "open" => 2,
            "factual" => 3,
            "rhetorical" => 4,
            _ => 5,
        }
    }
    // Collect candidates that pass the length + shape filters.
    let mut candidates: Vec<StarterQuestion> = atoms
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Question(q) => {
                let text = q.content.trim();
                let char_count = text.chars().count();
                if !(25..=220).contains(&char_count) {
                    return None;
                }
                // Normalise trailing punctuation to a question mark.
                let cleaned = if text.ends_with('?') {
                    text.to_string()
                } else {
                    let stripped = text.trim_end_matches(['.', '!', ',', ';', ':']);
                    format!("{stripped}?")
                };
                let source_section = q
                    .raised_at
                    .first()
                    .map(|r| r.chunk_id.clone())
                    .filter(|s| !s.is_empty());
                Some(StarterQuestion {
                    text: cleaned,
                    atom_id: q.id.as_str().to_string(),
                    source_section,
                    question_type: q.question_type.as_str_repr().to_string(),
                })
            }
            _ => None,
        })
        .collect();
    // Stable sort by (tier, then atom_id) so ties resolve deterministically.
    candidates.sort_by(|a, b| {
        tier(&a.question_type)
            .cmp(&tier(&b.question_type))
            .then_with(|| a.atom_id.cmp(&b.atom_id))
    });
    // Round-robin diversify by source_section. First pass: pick one
    // per section in tier order. Second pass: fill remaining slots
    // from the leftover pool.
    let mut picked: Vec<StarterQuestion> = Vec::with_capacity(limit);
    let mut used_sections: HashSet<String> = HashSet::new();
    let mut leftovers: Vec<StarterQuestion> = Vec::new();
    for q in candidates {
        if picked.len() >= limit {
            leftovers.push(q);
            continue;
        }
        match &q.source_section {
            Some(section) if !used_sections.contains(section) => {
                used_sections.insert(section.clone());
                picked.push(q);
            }
            _ => leftovers.push(q),
        }
    }
    for q in leftovers {
        if picked.len() >= limit {
            break;
        }
        picked.push(q);
    }
    picked
}

// ─── Command: mark_first_run_complete / is_first_run ─────────────────

/// Marker file under `~/.svrnmesh/first_run_complete`. Absence
/// signals "user has not finished the onboarding corpus flow yet".
/// Content is an ISO-8601 timestamp so a future version can reason
/// about when onboarding completed (e.g. re-onboarding after a major
/// schema change).
fn first_run_marker_path() -> PathBuf {
    paths::data_root().join("first_run_complete")
}

#[tauri::command]
pub async fn is_first_run() -> Result<bool, String> {
    // Dev: SOVEREIGN_DEV_FORCE_FIRST_RUN replays the corpus onboarding
    // as a first launch (in-memory; the marker on disk is untouched).
    if crate::dev_flags::force_first_run() {
        return Ok(true);
    }
    Ok(!first_run_marker_path().exists())
}

#[tauri::command]
pub async fn mark_first_run_complete() -> Result<(), String> {
    let path = first_run_marker_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let ts = chrono::Utc::now().to_rfc3339();
    std::fs::write(&path, &ts).map_err(|e| format!("writing {}: {e}", path.display()))?;
    tracing::info!(path = %path.display(), "first_run_complete marker written");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_question_ranker_prefers_thematic_then_interpretive() {
        use corpus_engine::enrichment::atlas::{
            AtomEnvelope, AtomId, ChunkRef, Question, ResolutionStatus,
        };
        use corpus_engine::enrichment::pipeline::{EnrichmentDepth, QuestionType};
        let mk = |id: usize, text: &str, qtype: QuestionType, section: &str| {
            AtomEnvelope::Question(Question {
                id: AtomId::question(id),
                content: text.into(),
                question_type: qtype,
                addressed_by: Vec::new(),
                raised_at: vec![ChunkRef::new(section.to_string(), None)],
                resolution_status: ResolutionStatus::Open,
                enrichment_depth: EnrichmentDepth::Extracted,
            })
        };
        let atoms = vec![
            mk(
                1,
                "What is the factual date of the encounter between the brothers?",
                QuestionType::Factual,
                "sec_0001",
            ),
            mk(
                2,
                "How does faith change when grief meets doubt across chapters?",
                QuestionType::Thematic,
                "sec_0002",
            ),
            mk(
                3,
                "Does the ending dissolve or resolve the central question posed here?",
                QuestionType::Interpretive,
                "sec_0003",
            ),
        ];
        let picks = rank_starter_questions(&atoms, 3);
        assert_eq!(picks.len(), 3, "all three should pass length gate");
        assert_eq!(picks[0].question_type, "thematic", "thematic wins tier 0");
        assert_eq!(
            picks[1].question_type, "interpretive",
            "interpretive wins tier 1"
        );
        assert_eq!(picks[2].question_type, "factual", "factual in tier 3");
    }

    #[test]
    fn starter_question_ranker_diversifies_by_section() {
        use corpus_engine::enrichment::atlas::{
            AtomEnvelope, AtomId, ChunkRef, Question, ResolutionStatus,
        };
        use corpus_engine::enrichment::pipeline::{EnrichmentDepth, QuestionType};
        let mk = |id: usize, text: &str, section: &str| {
            AtomEnvelope::Question(Question {
                id: AtomId::question(id),
                content: text.into(),
                question_type: QuestionType::Thematic,
                addressed_by: Vec::new(),
                raised_at: vec![ChunkRef::new(section.to_string(), None)],
                resolution_status: ResolutionStatus::Open,
                enrichment_depth: EnrichmentDepth::Extracted,
            })
        };
        // Three questions from the same section and two from different
        // sections. Limit=3 should pull at most one from sec_0001
        // before falling back to leftovers.
        let atoms = vec![
            mk(
                1,
                "A first long enough thematic question from section one opening?",
                "sec_0001",
            ),
            mk(
                2,
                "A second long enough thematic question from section one opening?",
                "sec_0001",
            ),
            mk(
                3,
                "A third long enough thematic question from section one opening?",
                "sec_0001",
            ),
            mk(
                4,
                "A long enough thematic question from section two probing meaning?",
                "sec_0002",
            ),
            mk(
                5,
                "A long enough thematic question from section three probing nuance?",
                "sec_0003",
            ),
        ];
        let picks = rank_starter_questions(&atoms, 3);
        let sections: Vec<Option<String>> =
            picks.iter().map(|p| p.source_section.clone()).collect();
        let distinct_sections: HashSet<_> = picks
            .iter()
            .filter_map(|p| p.source_section.clone())
            .collect();
        assert_eq!(picks.len(), 3);
        assert_eq!(
            distinct_sections.len(),
            3,
            "should cover three distinct sections before revisiting one; got {:?}",
            sections
        );
    }

    #[test]
    fn starter_question_ranker_rejects_too_short_and_too_long() {
        use corpus_engine::enrichment::atlas::{
            AtomEnvelope, AtomId, ChunkRef, Question, ResolutionStatus,
        };
        use corpus_engine::enrichment::pipeline::{EnrichmentDepth, QuestionType};
        let mk = |id: usize, text: String| {
            AtomEnvelope::Question(Question {
                id: AtomId::question(id),
                content: text,
                question_type: QuestionType::Thematic,
                addressed_by: Vec::new(),
                raised_at: vec![ChunkRef::new("sec_0001".to_string(), None)],
                resolution_status: ResolutionStatus::Open,
                enrichment_depth: EnrichmentDepth::Extracted,
            })
        };
        let atoms = vec![
            mk(1, "Why?".into()),   // too short
            mk(2, "a".repeat(300)), // too long
            mk(
                3,
                "What actually grounds a claim like this in the shipped corpus?".into(),
            ),
        ];
        let picks = rank_starter_questions(&atoms, 5);
        assert_eq!(picks.len(), 1, "only the middle-length question survives");
        assert!(picks[0].text.ends_with('?'));
    }

    #[test]
    fn starter_question_ranker_limit_zero_returns_empty() {
        let picks = rank_starter_questions(&[], 0);
        assert!(picks.is_empty());
    }
}
