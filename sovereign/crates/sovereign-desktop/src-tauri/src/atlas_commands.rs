// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri commands for the desktop's Atlas Inspector surface.
//!
//! Read-only browsing today (Phase 1): list corpora that have an
//! atlas, list/filter atoms within one corpus (Step 3), inspect a
//! single atom (Step 4). Phase 2 will grow curation-edit commands
//! here — overlay reads ride on `sovereign_tools::atlas_view`'s
//! existing methods, so this module stays the only Tauri surface for
//! atlas inspection.
//!
//! These commands live outside `commands.rs` deliberately:
//! `commands.rs` is already the workspace's largest file (§3.3 in
//! sovereign/ARCH_PRINCIPLES.md), and atlas inspection is a distinct
//! concern from the "reading from a citation" flow that owns the
//! `read_*` commands.
//!
//! # Where the browse half is decided (sv-surface D4)
//!
//! The six browse commands hold NO reader. Each is one call onto
//! `sovereign_mesh::atlas_http`'s routes over `client_base_url()` — ONE
//! path in both boot modes, because Local means the daemon is in-process
//! over this process's own `corpus_engine`. Their return types are
//! unchanged and unwrapped: the routes answer the same
//! `sovereign_tools::atlas_view` types the local `FileAtlasReader`
//! returned, so the frontend contract is byte-identical — and the
//! section→chunk map that makes `atlas_get_atom_detail`'s evidence rows
//! clickable is now built and cached ONCE per host rather than once per
//! surface.
//!
//! The six conversation-tiered commands crossed on the same rung, over
//! `/internal/atlas/conv/` and the daemon's own
//! `runtime.lane_sources.conv_tiered` — so this file holds no reader and
//! no store handle at all.
//!
//! What is deliberately still local, and why: the two GLiNER commands
//! download a model into this app's own data dir (app-local by the
//! campaign's closed set).

use std::sync::Arc;

use sovereign_tools::atlas_view::{
    AtlasBuildReport, AtlasCorpusSummary, AtlasMemberSummary, AtomDetail, AtomFilter, AtomListPage,
    ConvCorpusSummary, ConvDetailView, ConvEntityChip, ConvListPage, PageCursor,
};
use tauri::State;

use crate::state::AppState;

/// The client for the daemon's atlas-browse surface — the same
/// `FileAtlasReader` over the same `index_dir` the desktop used to
/// construct here, reached over loopback instead (sv-surface D4).
fn atlas_client(state: &AppState) -> sovereign_turn_client::TurnClient {
    sovereign_turn_client::TurnClient::new(state.client_base_url())
}

/// List every installed corpus that has an atlas on disk. Drives the
/// desktop's `/atlas` index route — one row per corpus, with
/// per-atom-type counts so the type tabs can show badges before the
/// user clicks in.
#[tauri::command]
pub async fn atlas_list_corpora(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<AtlasCorpusSummary>, String> {
    atlas_client(&state)
        .atlas_corpora::<Vec<AtlasCorpusSummary>>()
        .await
        .map_err(|e| format!("atlas_list_corpora: {e}"))
}

/// What the last build found about one corpus — the desktop's build report
/// card (ontology-v1 P6.4).
///
/// Reads the artefacts a build already wrote; it does not re-derive anything,
/// so the card shows a VERDICT with an age rather than a live measurement.
/// A corpus whose report step has not run comes back `reported: false` — a
/// successful answer the card renders as "not built yet", never an error.
#[tauri::command]
pub async fn atlas_build_report(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<AtlasBuildReport, String> {
    atlas_client(&state)
        .atlas_build_report::<AtlasBuildReport>(&corpus_id)
        .await
        .map_err(|e| format!("atlas_build_report: {e}"))
}

/// List the **member atlases** of a collection corpus — the sibling
/// `<corpus_id>-<slug>` indexes that carry the map when the parent's
/// own atlas is empty (SEP: one atlas per encyclopedia entry).
///
/// Drives a collection notebook's Explore tab, which opens as an
/// article picker rather than an atom list. An empty result is the
/// answer for every ordinary corpus — that is exactly how the
/// frontend decides which Explore surface to render, so this must
/// never error for a non-collection corpus.
#[tauri::command]
pub async fn atlas_list_members(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Vec<AtlasMemberSummary>, String> {
    atlas_client(&state)
        .atlas_members::<Vec<AtlasMemberSummary>>(&corpus_id)
        .await
        .map_err(|e| format!("atlas_list_members: {e}"))
}

/// Browse atoms within one corpus — filterable by type, searchable
/// by display name, paginated. Drives the desktop's per-corpus
/// inspector view.
///
/// Heavy first call (atoms.json deserialisation) is cached
/// in-process; subsequent filter/search changes are served from the
/// cached vec. Mtime + size key on atoms.json invalidates the cache
/// automatically when extraction reruns.
#[tauri::command]
pub async fn atlas_list_atoms(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    filter: Option<AtomFilter>,
    page: Option<PageCursor>,
) -> Result<AtomListPage, String> {
    // `atlas_http::AtomBrowseRequest`'s two keys, carrying the SAME
    // `Option` semantics this command already had — an absent value is the
    // type's `Default`, now applied by the route so the default has one
    // decider rather than two (ARCH §10.6).
    let request = serde_json::json!({ "filter": filter, "page": page });
    atlas_client(&state)
        .atlas_atoms::<_, AtomListPage>(&corpus_id, &request)
        .await
        .map_err(|e| format!("atlas_list_atoms: {e}"))
}

/// Build the curated landscape **Map** subgraph for one corpus — nodes
/// (atoms, sized by salience/degree) + edges (relationships; `Tension` edges
/// carry their disagreement `crux`), capped so a large corpus reads as a map
/// rather than a hairball (the epistemic spine — tension endpoints, every
/// argument + question — is always kept). Drives `AtlasGraph.svelte`.
#[tauri::command]
pub async fn atlas_subgraph(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    max_nodes: Option<usize>,
) -> Result<sovereign_tools::atlas_view::AtlasSubgraph, String> {
    // `None` travels as an absent `max_nodes` and the route applies
    // `atlas_view::DEFAULT_MAX_NODES` — the cap keeps ONE decider, and it
    // is not this file.
    atlas_client(&state)
        .atlas_subgraph::<sovereign_tools::atlas_view::AtlasSubgraph>(&corpus_id, max_nodes)
        .await
        .map_err(|e| format!("atlas_subgraph: {e}"))
}

/// Full inspector record for one atom — full type-specific shape +
/// one-hop related atoms + cross-corpus bridges + evidence
/// excerpts. Drives the desktop's `AtomDetail.svelte`.
///
/// The evidence excerpts arrive with their `section_id`s ALREADY
/// resolved to numeric `chunk_id`s — the route does that half now, off
/// the same per-corpus cache and the same never-build-on-the-click-path
/// policy the desktop used to keep privately. A row whose section did
/// not resolve stays `chunk_id: None` and renders non-clickable, exactly
/// as before; the map fills in the background and later clicks resolve.
///
/// Returns `Ok(None)` when the atom id isn't present in the corpus's
/// atoms.json (stale UI link, or extraction renumbered atom_ids
/// since the last list_atoms call) — the route's 404, which
/// `atlas_atom_detail` maps to `None` while leaving every OTHER
/// non-success an error, so "corpus will not open" cannot read as
/// "atom absent".
#[tauri::command]
pub async fn atlas_get_atom_detail(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    atom_id: String,
) -> Result<Option<AtomDetail>, String> {
    atlas_client(&state)
        .atlas_atom_detail::<AtomDetail>(&corpus_id, &atom_id)
        .await
        .map_err(|e| format!("atlas_get_atom_detail: {e}"))
}

// ── Conversation tiered-retrieval commands (spec CONV_TIERED_PORT.md
//    §"Retrieval surface — A1/A2") ──────────────────────────────────
//
// Conv corpora never wrote atoms.json — their tiered enrichment lives
// in the `conv_skeletons` / `conv_raptor_nodes` / `conv_motifs` SQLite
// sidecar tables. These six commands hold NO reader either (sv-surface
// D4 remainder): each is one call onto `sovereign_mesh::atlas_http`'s
// `/internal/atlas/conv/` routes over `client_base_url()`, which read
// the daemon's `runtime.lane_sources.conv_tiered`. ONE path in both
// boot modes. AtlasIndex still calls BOTH atlas_list_corpora
// (atoms.json) and atlas_list_conv_corpora (these), then merges
// client-side — that fold is unchanged, because the return types are
// the same `atlas_view` / `conv_tiered` types the store reads built.
//
// TWO SUBSTITUTIONS ARE GONE, deliberately (§18.3). The store-side
// bodies swallowed two failures:
//
//   * `list_conv_raptor_nodes(..).unwrap_or_default()` inside the list
//     fold — a reader error rendered as "this conversation has no
//     entities" on every row.
//   * `get_active_correction(..).ok().flatten()` inside the detail —
//     a reader error rendered as "not revised by you", which is the
//     provenance badge saying the opposite of what happened.
//
// The routes REPORT both. These commands therefore surface an `Err`
// where they used to return a plausible empty — the pane shows the
// host's words instead of a wrong answer. Absence still has its own
// answers and they are NOT errors: an unknown conversation is
// `Ok(None)` (the 404) and never-extracted chunk progress is
// `Ok(None)` from an explicit `null` body.

/// List every conv corpus with at least one row in `conv_skeletons`,
/// plus its state-bucket counts and its display metadata. Drives the
/// desktop Atlas index "Conversations" group.
///
/// The display-name/icon lookup that used to live here (a
/// best-effort `installed_indexes()` walk) is the route's now — one
/// decider, and the route has the engine beside the reader.
#[tauri::command]
pub async fn atlas_list_conv_corpora(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ConvCorpusSummary>, String> {
    atlas_client(&state)
        .conv_corpora::<ConvCorpusSummary>()
        .await
        .map_err(|e| format!("atlas_list_conv_corpora: {e}"))
}

/// Paginated list of conversations in one corpus, filterable by
/// substring on `overview`. The page size is the HOST's (200) — the
/// same 200 this command hard-coded, moved down to the one decider,
/// which is why `limit` is not a parameter here.
#[tauri::command]
pub async fn atlas_list_conversations(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    filter: Option<String>,
    offset: Option<u64>,
) -> Result<ConvListPage, String> {
    let filter = filter.as_deref().map(str::trim).filter(|s| !s.is_empty());
    atlas_client(&state)
        .conv_conversations::<ConvListPage>(&corpus_id, filter, offset)
        .await
        .map_err(|e| format!("atlas_list_conversations: {e}"))
}

/// Full conversation detail: skeleton + RAPTOR tree + any active
/// summary correction. Drives the ConvDetail.svelte component (tree
/// view).
///
/// `Ok(None)` is the route's 404 and ONLY that: this corpus has no
/// such conversation. A daemon with no conv-tiered reader is a 503 and
/// stays an `Err`, so "the reader is missing" cannot render as "the
/// conversation is missing".
#[tauri::command]
pub async fn atlas_get_conv_detail(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    conv_uuid: String,
) -> Result<Option<ConvDetailView>, String> {
    atlas_client(&state)
        .conv_detail::<ConvDetailView>(&corpus_id, &conv_uuid)
        .await
        .map_err(|e| format!("atlas_get_conv_detail: {e}"))
}

/// GliNER model availability + path for the Settings → Imports
/// surface. Returns whether the configured model is installed +
/// the expected on-disk path so the UI can show "Install model"
/// vs "Re-download" affordances. Spec: Phase 1 model UX.
#[derive(serde::Serialize, Clone)]
pub struct GlinerModelStatus {
    pub installed: bool,
    pub model_id: String,
    pub expected_path: String,
    pub size_estimate_mb: u64,
}

#[tauri::command]
pub async fn atlas_check_gliner_model() -> Result<GlinerModelStatus, String> {
    let model_id = sovereign_gliner::gliner_ner::DEFAULT_MODEL_ID.to_string();
    let installed = sovereign_gliner::gliner_ner::probe_model_available(&model_id);
    let expected_path = sovereign_gliner::gliner_ner::models_root()
        .join(&model_id)
        .display()
        .to_string();
    Ok(GlinerModelStatus {
        installed,
        model_id,
        expected_path,
        // Empirical: gliner_small-v2.1 = ~600MB (ONNX f32 + tokenizer).
        size_estimate_mb: 600,
    })
}

/// Kicks off a model download. Streams progress via Tauri events
/// on the channel `gliner-download-progress` (payload: `{ file,
/// downloaded, total }`). Returns when the download completes or
/// errors. Idempotent: skips files already present.
#[tauri::command]
pub async fn atlas_download_gliner_model(
    app: tauri::AppHandle,
    model_id: Option<String>,
) -> Result<(), String> {
    use tauri::Emitter;
    let model_id =
        model_id.unwrap_or_else(|| sovereign_gliner::gliner_ner::DEFAULT_MODEL_ID.to_string());
    let app_for_cb = app.clone();
    let on_progress = move |file: &str, downloaded: u64, total: u64| {
        let _ = app_for_cb.emit(
            "gliner-download-progress",
            serde_json::json!({
                "file": file,
                "downloaded": downloaded,
                "total": total,
            }),
        );
    };
    sovereign_gliner::gliner_ner::download_model(&model_id, on_progress)
        .await
        .map_err(|e| format!("atlas_download_gliner_model: {e}"))?;
    let _ = app.emit(
        "gliner-download-progress",
        serde_json::json!({ "file": "__complete__", "downloaded": 0u64, "total": 0u64 }),
    );
    Ok(())
}

/// Aggregate one entity's footprint inside a corpus. Powers the
/// Atlas-view entity drawer: click an `entity-chip`, the UI invokes
/// this to render mention/conv counts, label breakdown, top convs,
/// and co-occurring entities. Matches `text` case-insensitively so
/// the drawer collapses casing variance but splits homonyms by label.
///
/// The two drawer caps (20 co-occurring, 10 conversations) moved down
/// to the route with the read — one decider.
#[tauri::command]
pub async fn atlas_get_entity_aggregate(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    text: String,
) -> Result<sovereign_core::conv_tiered::EntityAggregateRow, String> {
    atlas_client(&state)
        .conv_entity_aggregate::<sovereign_core::conv_tiered::EntityAggregateRow>(&corpus_id, &text)
        .await
        .map_err(|e| format!("atlas_get_entity_aggregate: {e}"))
}

/// Per-corpus entity-extraction progress. Drives the AtlasIndex
/// "X% extracted" badge that appears alongside per-state enrichment
/// counts while extraction is running.
///
/// `Ok(None)` is an explicit `null` body on a 200 — the corpus exists
/// and extraction never ran. It is not "no route" and not "no reader";
/// both of those are `Err`.
#[tauri::command]
pub async fn atlas_get_chunk_entity_progress(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Option<sovereign_core::conv_tiered::ChunkEntityProgressRow>, String> {
    atlas_client(&state)
        .conv_chunk_entity_progress::<sovereign_core::conv_tiered::ChunkEntityProgressRow>(
            &corpus_id,
        )
        .await
        .map_err(|e| format!("atlas_get_chunk_entity_progress: {e}"))
}

/// Top-N entity chips for one conversation (A2). Drives the entity
/// chip row above `ConversationChunkRenderer`'s message bubbles.
/// Tiny convs return an empty list — the UI suppresses the chip row.
///
/// The salience rank and the N (12) are the route's now: the same
/// ranking feeds `atlas_list_conversations`' `top_entities`, and two
/// copies of one formula is the §10.6 smell.
#[tauri::command]
pub async fn atlas_get_conv_entities(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    conv_uuid: String,
) -> Result<Vec<ConvEntityChip>, String> {
    atlas_client(&state)
        .conv_entities::<ConvEntityChip>(&corpus_id, &conv_uuid)
        .await
        .map_err(|e| format!("atlas_get_conv_entities: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    // The browse commands no longer construct a reader (sv-surface D4);
    // these two tests still drive `FileAtlasReader` DIRECTLY, because what
    // they pin is the on-disk fixture the route will read, not the command.
    use corpus_engine::enrichment::atlas::atoms::{
        AtomEnvelope, AtomId, AtomsFile, ChunkRef, Entity,
    };
    use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
    use sovereign_tools::atlas_view::FileAtlasReader;

    fn write_atoms(atlas_dir: &std::path::Path, atoms: Vec<AtomEnvelope>) {
        std::fs::create_dir_all(atlas_dir).unwrap();
        let file = AtomsFile::new(atoms);
        std::fs::write(
            atlas_dir.join("atoms.json"),
            serde_json::to_vec_pretty(&file).unwrap(),
        )
        .unwrap();
    }

    fn sample_entity(id: usize, name: &str) -> AtomEnvelope {
        AtomEnvelope::Entity(Entity {
            id: AtomId::entity(id),
            canonical_name: name.into(),
            aliases: vec![],
            entity_type: EntityType::Concept,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "x".into(),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: vec![],
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        })
    }

    // The Tauri command itself can't be unit-tested without a full
    // AppState (which owns a corpus engine + runtime). The
    // command's two ingredients ARE separately testable:
    //
    //   - FileAtlasReader::list_corpora — covered by 7 tests in
    //     sovereign_tools::atlas_view::reader::tests.
    //   - The DTO serialisation — covered by
    //     `atlas_corpus_summary_serialises_cleanly` in the same
    //     module.
    //
    // This test pins the *wire-level* end-to-end behaviour without
    // a Tauri runtime: build a FileAtlasReader against a tempdir
    // fixture (mimicking what the command does internally) and
    // verify the JSON the desktop receives.

    #[tokio::test]
    async fn atlas_list_corpora_returns_serialisable_summaries() {
        let tmp = tempfile::tempdir().unwrap();
        write_atoms(
            &tmp.path().join("wikipedia").join("atlas"),
            vec![sample_entity(1, "Earth"), sample_entity(2, "Mars")],
        );
        write_atoms(
            &tmp.path().join("sep-mind").join("atlas"),
            vec![sample_entity(1, "Consciousness")],
        );
        let reader = FileAtlasReader::new(tmp.path().to_path_buf());
        let summaries = reader.list_corpora().await.expect("list_corpora succeeds");
        // What the Tauri command Ok-arm returns:
        let wire = serde_json::to_value(&summaries).unwrap();
        assert!(wire.is_array());
        let arr = wire.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        // Sorted alphabetically — sep-mind first.
        assert_eq!(arr[0]["corpus_id"], "sep-mind");
        assert_eq!(arr[0]["total_atoms"], 1);
        assert_eq!(arr[1]["corpus_id"], "wikipedia");
        assert_eq!(arr[1]["total_atoms"], 2);
    }

    /// The numismatics real-mode fixture, validated through the SAME reader
    /// the Tauri commands use.
    ///
    /// `numismatics.real.spec.ts` asserts a pill row, a filtered list and an
    /// atom inspector over a checked-in atlas that global-setup overlays onto
    /// a real ingested corpus. Every number in that spec comes from these
    /// three files, so if the fixture drifts — a serde field renamed, a
    /// hand-edited atom that no longer deserialises, `ontology.json` dropped
    /// from the overlay list — the browser spec fails minutes into a real-mode
    /// run with a UI-shaped error a long way from the cause. This fails in
    /// milliseconds and says which file.
    ///
    /// Modelled on `governance_real_fixture_is_valid_or_regenerated`
    /// (`governance_commands.rs`), including the "absent → skip" arm: the
    /// fixture is committed, so absence means a partial checkout, not a
    /// regression.
    #[tokio::test]
    async fn numismatics_real_fixture_carries_the_census_its_spec_asserts() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/e2e/real/fixtures/numismatics-atlas");
        if !fixture.join("ontology.json").exists() {
            eprintln!(
                "[skip] numismatics real-mode fixture absent at {}",
                fixture.display()
            );
            return;
        }

        // Lay it out exactly as `plantNumismaticsCorpus` does: one index dir
        // with an `atlas/` holding the three overlay files.
        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("numismatics-e2e").join("atlas");
        std::fs::create_dir_all(&atlas_dir).unwrap();
        for f in ["atoms.json", "edges.json", "ontology.json"] {
            std::fs::copy(fixture.join(f), atlas_dir.join(f))
                .unwrap_or_else(|e| panic!("copying {f}: {e}"));
        }

        let reader = FileAtlasReader::new(tmp.path().to_path_buf());
        let rows = reader.list_corpora().await.expect("list_corpora succeeds");
        let row = rows
            .iter()
            .find(|r| r.corpus_id == "numismatics-e2e")
            .expect("the fixture corpus is listed — if not, atoms.json failed to parse");

        assert_eq!(row.total_atoms, 12);

        // The declaration reached the summary. WITHOUT `ontology.json` in the
        // overlay this vector is empty and the desktop falls back to the
        // generic atom kinds — the spec would then assert nothing about
        // ontology while still passing on the kind pills.
        let declared: Vec<&str> = row.declared_types.iter().map(|t| t.name.as_str()).collect();
        // ALPHABETICAL, not declaration order. The recipe declares coin,
        // sceatta, ruler, mint, attribution; `_summary.json` v4 carries the
        // declaration as a `BTreeMap<String, String>`, so the order is lost
        // before `AtlasCorpusSummary` ever sees it — and the desktop's pill
        // row therefore separates `sceatta` from the `coin` it specializes.
        // Pinned as it IS rather than as the P6 design assumed, so a later
        // order-preserving change is a deliberate edit here and not a
        // surprise (§18.3).
        assert_eq!(
            declared,
            vec!["attribution", "coin", "mint", "ruler", "sceatta"],
            "the summary's declaration map is a BTreeMap — this is pill order",
        );
        let sceatta = row
            .declared_types
            .iter()
            .find(|t| t.name == "sceatta")
            .expect("sceatta is declared");
        assert_eq!(
            sceatta.specializes.as_deref(),
            Some("coin"),
            "the roll-up the `coin` pill's badge depends on",
        );

        // OWN counts. The spec's `coin` badge is 5 — 3 + the 2 sceattas —
        // and nothing here carries that total.
        assert_eq!(row.subtype_counts.get("coin"), Some(&3));
        assert_eq!(row.subtype_counts.get("sceatta"), Some(&2));
        assert_eq!(row.subtype_counts.get("mint"), Some(&2));
        assert_eq!(row.subtype_counts.get("attribution"), Some(&2));
        assert_eq!(
            row.subtype_counts.get("ruler"),
            Some(&1),
            "a `role_of` type is counted across kinds, not inside Entity",
        );
        assert_eq!(row.atom_counts.values().sum::<u64>(), 12);

        // Clicking the `coin` pill: exact match, no roll-up.
        let page = reader
            .list_atoms(
                "numismatics-e2e",
                AtomFilter {
                    subtypes: vec!["coin".into()],
                    ..Default::default()
                },
                PageCursor::default(),
            )
            .await
            .unwrap();
        let names: Vec<&str> = page.items.iter().map(|a| a.display_name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Marlow Field 1", "Marlow Field 2", "Marlow Field 3"],
            "the three coins, NOT the two sceattas the badge counts",
        );

        // Clicking `ruler`: a State atom on the person, found without the
        // caller naming a kind.
        let page = reader
            .list_atoms(
                "numismatics-e2e",
                AtomFilter {
                    subtypes: vec!["ruler".into()],
                    ..Default::default()
                },
                PageCursor::default(),
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].display_name, "King of Mercia");
        assert_eq!(
            page.items[0].atom_type,
            corpus_engine::enrichment::atlas::atoms::AtomType::State,
        );

        // The attribution claim's "About" link, and the `ref` attribute that
        // resolves beside one that is only what the source said.
        let detail = reader
            .get_atom_detail("numismatics-e2e", "claim-0001")
            .await
            .unwrap()
            .expect("the attribution claim is in the fixture");
        assert_eq!(
            detail.referenced_atoms["entity-0008"].display_name, "Marlow Field 4",
            "the claim's subject resolves",
        );
        let sceatta = reader
            .get_atom_detail("numismatics-e2e", "entity-0008")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            sceatta.referenced_atoms["entity-0003"].display_name, "Canterbury",
            "the `mint` ref attribute resolves",
        );
        let unresolved = reader
            .get_atom_detail("numismatics-e2e", "entity-0009")
            .await
            .unwrap()
            .unwrap();
        assert!(
            unresolved.referenced_atoms.is_empty(),
            "\"an unidentified continental mint\" is the source's words, not a link: {:?}",
            unresolved.referenced_atoms,
        );
    }
}
