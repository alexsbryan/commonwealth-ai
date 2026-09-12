// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local-corpus configuration — the ENGINE-facing half.
//!
//! **The schema itself moved to `sovereign_contracts::daemon_wire::
//! local_corpus::config` at svt-6 (2026-09-12)** and is re-exported below at
//! every historical path, so `sovereign_tools::local_corpus::config::Name`
//! and `sovereign_tools::local_corpus::LocalCorpusConfig` keep resolving. The
//! reason is in that module's header: those types are pure serde, and a
//! client that wanted to spell one had to link this crate and everything
//! under it.
//!
//! What is left here is what names `corpus-engine`: rendering the Recipe
//! `CorpusEngine::ingest` consumes, and the `DisplayMeta` that rides on it.
//! `display_meta` is a FREE FUNCTION rather than a method because an inherent
//! impl has to live in the crate that defines the type, and the type lives a
//! layer down now — its four siblings (`is_watched`, `should_reconcile`,
//! `reconcile_kind`, `watched_config`) return nothing engine-shaped, so they
//! moved with the enum and are still methods.

use std::path::{Path, PathBuf};

pub use sovereign_contracts::daemon_wire::local_corpus::config::*;

/// Display metadata for this source type — the SSOT consumed by
/// `recipe_toml` (ingest-time `[display]` stamp), the manager's
/// init-time backfill, and `enable_enrichment`'s re-stamp. The
/// category is load-bearing beyond UI: `is_tiered_category`
/// (sovereign-core/conv_briefing) gates the tiered retrieval
/// surface — RAPTOR briefings + the entity-PPR rerank — on it, so
/// a corpus without a category is silently exempt from
/// entity-aware retrieval. `DocumentFolder` reuses the
/// `watched_folder` category on purpose: a one-shot import enriches
/// like a watched folder (minus the sync worker), so it must be
/// retrieval-visible too.
pub fn display_meta(
    source_type: &LocalCorpusSourceType,
) -> Option<corpus_engine::recipe::DisplayMeta> {
    match source_type {
        LocalCorpusSourceType::ObsidianVault { .. } => Some(corpus_engine::recipe::DisplayMeta {
            category: Some("vault".to_string()),
            icon: Some("book-open".to_string()),
        }),
        LocalCorpusSourceType::WatchedFolder(_) => Some(corpus_engine::recipe::DisplayMeta {
            category: Some("watched_folder".to_string()),
            icon: Some("folder".to_string()),
        }),
        // A one-shot drag-drop import enriches like a watched folder
        // (RAPTOR + entity atlas), minus the sync worker — so it reuses
        // the `watched_folder` category, which is what lands it in
        // TIERED_DISPLAY_CATEGORIES for entity-aware retrieval. Watching,
        // not enrichment, is the only thing DocumentFolder gives up.
        LocalCorpusSourceType::DocumentFolder => Some(corpus_engine::recipe::DisplayMeta {
            category: Some("watched_folder".to_string()),
            icon: Some("folder".to_string()),
        }),
    }
}
/// Render the Recipe that `CorpusEngine::ingest` will consume for this
/// local corpus. The returned TOML string is written to a temp file by
/// the manager and handed to the engine via `CorpusSpec::RecipePath`.
///
/// `jsonl_source` is the path to the pre-extracted JSONL file that
/// `ExtractStage` produces. Corpus-engine's `jsonl` extractor reads the
/// `content` / `title` / `source_id` fields we wrote.
pub fn recipe_toml(config: &LocalCorpusConfig, jsonl_source: &Path) -> String {
    let chunk = match &config.chunker {
        ChunkerKind::Paragraph {
            max_chars,
            overlap_chars,
        } => format!(
            r#"[chunk]
type = "paragraph"
max_chars = {max_chars}
overlap_chars = {overlap_chars}
"#
        ),
        // v1: the engine's `Semantic` variant only accepts `max_chars`.
        // M3 extends it to carry the heading list; until then we emit a
        // paragraph chunker with the same max_chars so ingestion works.
        // When the engine variant is extended, update this branch.
        ChunkerKind::Semantic {
            max_chars,
            overlap_chars,
            ..
        } => format!(
            r#"[chunk]
type = "paragraph"
max_chars = {max_chars}
overlap_chars = {overlap_chars}
"#
        ),
    };

    let description = format!(
        "Local corpus ({}): {}",
        source_type_tag(&config.source_type),
        config.display_name
    );

    // The `[display]` section routes the recipe into the tiered
    // retrieval surface (GLiNER + per-source-doc RAPTOR + entity-walk
    // rerank). `is_tiered_category` in `sovereign-core/src/conv_briefing.rs`
    // gates on these category strings — without one, the recipe lands
    // in the index but the daemon's FolderTieredProvider never picks
    // it up.
    //
    // The `[enrichment] type = "tiered"` section routes the ingest
    // through `corpus-engine::enrichment::tiered::run_tiered_enrichment`
    // → `run_folder_tiered_enrichment`, which dispatches per-note
    // GLiNER NER + per-note RAPTOR via `FolderTieredProvider`. Without
    // it, ingest writes chunks but never invokes the tiered surface
    // (legacy field-model engine wouldn't fit either — vault has no
    // matching pipeline since `obsidian_atlas` was retired).
    // Category/icon come from the `display_meta()` SSOT so the
    // ingest-time stamp, the manager's init backfill, and
    // `enable_enrichment`'s re-stamp can never disagree.
    let display = match display_meta(&config.source_type) {
        Some(d) => format!(
            "[display]\ncategory = \"{}\"\nicon = \"{}\"\n\n",
            d.category.unwrap_or_default(),
            d.icon.unwrap_or_default()
        ),
        // Defensive: every source type currently returns a category from
        // display_meta(), so this arm is unreached — kept for a future
        // uncategorised source kind.
        None => String::new(),
    };
    // Enrichment is a property of ingest, not of watching: every folder
    // import gets the tiered atlas (RAPTOR + entity extraction). A
    // DocumentFolder differs from a WatchedFolder only in that no sync
    // worker is spawned for it — see `display_meta` above and the enrich
    // lifecycle in `manager.rs`.
    let enrichment = "[enrichment]\nenabled = true\ntype = \"tiered\"\n\n";

    format!(
        r#"[corpus]
id = "{id}"
name = "{display_name}"
description = "{description}"
license = "local"
mesh_sharing = false
scope = "{scope}"
grantable = true
schema_version = 1

[acquire]
type = "local_file"
path = "{source_path}"

[extract]
type = "jsonl"
content_field = "content"
title_field = "title"

{chunk}
[index]
fts = true
vector = true

[retrieval]
personal_scope = true

{display}{enrichment}"#,
        id = escape_toml(&config.id),
        display_name = escape_toml(&config.display_name),
        description = escape_toml(&description),
        scope = config.scope.as_recipe_str(),
        source_path = escape_toml(&jsonl_source.to_string_lossy()),
        chunk = chunk,
        display = display,
        enrichment = enrichment,
    )
}

fn source_type_tag(t: &LocalCorpusSourceType) -> &'static str {
    match t {
        LocalCorpusSourceType::ObsidianVault { .. } => "obsidian",
        LocalCorpusSourceType::DocumentFolder => "folder",
        LocalCorpusSourceType::WatchedFolder(_) => "watched",
    }
}

fn escape_toml(s: &str) -> String {
    // Quote for a basic-string TOML value. Escape backslashes and quotes.
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str(r#"\""#),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_default_has_no_writeback() {
        let cfg = LocalCorpusConfig::document_folder(
            PathBuf::from("/tmp/some-folder"),
            "City council 2024".into(),
        );
        assert!(cfg.write_back.is_none());
        assert!(cfg.enrichment.is_none());
        // Drag-drop admits the same formats a watched folder does. Pinned as an
        // equality rather than a literal list (that literal lives in
        // `watched_folder_default_extensions_and_scope`) because the bug being
        // guarded is precisely the two DRIFTING APART: drag-drop shipped with
        // only pdf+txt, so a folder of .md notes ingested silently empty while
        // the identical folder worked as a watched folder.
        assert!(
            cfg.extensions.contains(&"md".to_string()),
            "drag-dropped markdown must be ingested, not silently skipped"
        );
        assert_eq!(
            cfg.extensions,
            LocalCorpusConfig::watched_folder(
                PathBuf::from("/tmp/some-folder"),
                "City council 2024".into(),
                WatchedFolderConfig::default(),
            )
            .extensions,
            "drag-drop and watched-folder format support must not drift apart"
        );
        assert_eq!(cfg.scope, CorpusScope::Local);
        assert!(cfg.id.starts_with("folder-"));
        assert!(!cfg.watcher.enabled);
        assert!(cfg.pre_scan.scanned_pdf_detection);
    }

    #[test]
    fn vault_recipe_emits_vault_display_and_tiered_enrichment() {
        // Routes the recipe into FolderTieredProvider's per-source-doc
        // RAPTOR + GLiNER dispatch. Both [display] and [enrichment]
        // are load-bearing: [display] flips the briefing-side gate,
        // [enrichment] flips the ingest-side dispatch into the tiered
        // runner. Without either, the daemon ingests the vault but no
        // tiered enrichment fires (or it fires but never surfaces in
        // chat).
        let snap = PathBuf::from("/tmp/snapshots");
        let cfg = LocalCorpusConfig::obsidian_vault(PathBuf::from("/tmp/my-vault"), snap);
        let toml = recipe_toml(&cfg, &PathBuf::from("/tmp/staged.jsonl"));
        assert!(
            toml.contains("[display]\ncategory = \"vault\""),
            "vault recipe did not emit [display] vault category. \
             Full recipe:\n{toml}"
        );
        assert!(
            toml.contains("[enrichment]\nenabled = true\ntype = \"tiered\""),
            "vault recipe did not emit [enrichment] tiered block. \
             Full recipe:\n{toml}"
        );
    }

    #[test]
    fn watched_folder_recipe_emits_watched_folder_display_and_tiered() {
        let mut cfg = LocalCorpusConfig::document_folder(
            PathBuf::from("/tmp/some-folder"),
            "City council".into(),
        );
        cfg.source_type = LocalCorpusSourceType::WatchedFolder(WatchedFolderConfig::default());
        let toml = recipe_toml(&cfg, &PathBuf::from("/tmp/staged.jsonl"));
        assert!(
            toml.contains("[display]\ncategory = \"watched_folder\""),
            "watched-folder recipe did not emit [display] category. \
             Full recipe:\n{toml}"
        );
        assert!(
            toml.contains("[enrichment]\nenabled = true\ntype = \"tiered\""),
            "watched-folder recipe did not emit [enrichment] tiered block. \
             Full recipe:\n{toml}"
        );
    }

    #[test]
    fn document_folder_recipe_has_display_and_enrichment() {
        // A one-shot drag-drop import enriches like a watched folder minus
        // the sync worker: it stamps the shared `watched_folder` display
        // category (so it's retrieval-visible via TIERED_DISPLAY_CATEGORIES)
        // and requests tiered enrichment. Watching is the only thing it
        // gives up — see `display_meta` and `recipe_toml`.
        let cfg =
            LocalCorpusConfig::document_folder(PathBuf::from("/tmp/folder"), "Manuals".into());
        let toml = recipe_toml(&cfg, &PathBuf::from("/tmp/staged.jsonl"));
        assert!(
            toml.contains("[display]") && toml.contains("category = \"watched_folder\""),
            "document folder recipe should stamp the watched_folder display category. \
             Full recipe:\n{toml}"
        );
        assert!(
            toml.contains("[enrichment]") && toml.contains("type = \"tiered\""),
            "document folder recipe should request tiered enrichment. \
             Full recipe:\n{toml}"
        );
    }

    #[test]
    fn vault_default_has_writeback_and_watcher() {
        let snap = PathBuf::from("/tmp/snapshots");
        let cfg = LocalCorpusConfig::obsidian_vault(PathBuf::from("/tmp/my-vault"), snap);
        let wb = cfg
            .write_back
            .as_ref()
            .expect("vault should have writeback");
        assert_eq!(wb.namespace, "sovereign");
        assert_eq!(wb.index_dir, "_sovereign-index");
        assert_eq!(wb.snapshot_retention, 24);
        assert!(wb.snapshot_dir.starts_with("/tmp/snapshots"));
        assert!(wb.snapshot_dir.to_string_lossy().contains(&cfg.id));
        assert!(cfg.watcher.enabled);
        assert_eq!(cfg.watcher.debounce_ms, 800);
        assert_eq!(cfg.extensions, vec!["md".to_string()]);
        assert!(matches!(cfg.chunker, ChunkerKind::Semantic { .. }));
        assert!(cfg.id.starts_with("obsidian-"));
        // Scanned-PDF detection is pointless for markdown vaults.
        assert!(!cfg.pre_scan.scanned_pdf_detection);
    }

    #[test]
    fn corpus_ids_are_readable_deterministic_and_kind_deduped() {
        // `<kind>-<slug>-<hash6>`: humans read these in bench commands
        // and logs; the kind prefix survives for the stream-axes
        // stability check; the hash keeps distinct paths distinct.
        let id = corpus_id_for("obsidian", Path::new("/x/Obsidian Vault"));
        assert!(
            id.starts_with("obsidian-vault-"),
            "kind token must dedupe out of the slug: {id}"
        );
        assert_eq!(id.len(), "obsidian-vault-".len() + 12, "{id}");

        let watched = corpus_id_for("watched", Path::new("/x/Research Notes (2026)"));
        assert!(
            watched.starts_with("watched-research-notes-2026-"),
            "{watched}"
        );

        // Deterministic: same path → same id; different path → different id.
        assert_eq!(
            id,
            corpus_id_for("obsidian", Path::new("/x/Obsidian Vault"))
        );
        assert_ne!(
            id,
            corpus_id_for("obsidian", Path::new("/y/Obsidian Vault"))
        );

        // Unsluggable basenames fall back to the bare `<kind>-<hash>`.
        let bare = corpus_id_for("watched", Path::new("/x/委員会"));
        assert!(bare.starts_with("watched-"), "{bare}");
    }

    #[test]
    fn watched_folder_config_partial_payload_deserialises_via_defaults() {
        // Regression: when /internal/corpus/watch/register was first
        // wired, the CLI sent a partial config (only the knobs the
        // user explicitly set) and the daemon's WatchedFolderConfig
        // had no #[serde(default)] on its non-optional fields,
        // returning 422 on every register that omitted any of
        // follow_symlinks / deletion_guard / sweep_interval_secs /
        // soft_delete_grace_secs / exclude_globs. The fix is at the
        // *deserializer* (server) — clients should be allowed to send
        // only the fields they care about. This test pins the
        // partial-payload contract.

        // 1. Empty object → all defaults.
        let cfg: WatchedFolderConfig = serde_json::from_str("{}").expect("empty {} parses");
        assert_eq!(cfg.sweep_interval_secs, 120);
        assert_eq!(cfg.soft_delete_grace_secs, 7 * 86_400);
        assert!(!cfg.follow_symlinks);
        assert_eq!(cfg.deletion_guard.absolute_threshold, 100);
        assert!((cfg.deletion_guard.fractional_threshold - 0.25).abs() < 1e-6);
        assert!(cfg.deletion_guard.enabled);
        assert!(cfg.exclude_globs.is_empty());
        assert_eq!(cfg.sync_mode, SyncMode::Continuous);
        assert!(!cfg.sensitive);
        assert!(cfg.additional_roots.is_empty());

        // 2. CLI's actual shape: only the fields the user set.
        let partial = r#"{
            "follow_symlinks": true,
            "exclude_globs": ["COMMONWEALTH/**", ".obsidian/**"]
        }"#;
        let cfg: WatchedFolderConfig =
            serde_json::from_str(partial).expect("CLI partial payload parses");
        assert!(cfg.follow_symlinks);
        assert_eq!(cfg.exclude_globs.len(), 2);
        // Defaults still kick in for omitted fields:
        assert_eq!(cfg.sweep_interval_secs, 120);
        assert_eq!(cfg.deletion_guard.absolute_threshold, 100);
    }

    #[test]
    fn serde_defaults_match_default_impl() {
        // The free functions referenced by `#[serde(default = "…")]`
        // and the `Default` impl must agree. A drift here means a
        // server that builds via Default sees one value while a
        // server that deserialises an empty payload sees another —
        // the bug-fix only works as long as the two stay locked.
        let from_default = WatchedFolderConfig::default();
        let from_empty: WatchedFolderConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(
            from_default.sweep_interval_secs,
            from_empty.sweep_interval_secs
        );
        assert_eq!(
            from_default.soft_delete_grace_secs,
            from_empty.soft_delete_grace_secs
        );
        assert_eq!(from_default.follow_symlinks, from_empty.follow_symlinks);
    }

    #[test]
    fn should_reconcile_covers_watched_and_vault_but_not_folder() {
        let folder = LocalCorpusConfig::document_folder(PathBuf::from("/tmp/f"), "f".into());
        assert!(!folder.source_type.should_reconcile());
        assert!(folder.source_type.reconcile_kind().is_none());

        let watched = LocalCorpusConfig::watched_folder(
            PathBuf::from("/tmp/w"),
            "w".into(),
            WatchedFolderConfig::default(),
        );
        assert!(watched.source_type.should_reconcile());
        assert_eq!(
            watched.source_type.reconcile_kind(),
            Some(ReconcileKind::WatchedFolder)
        );

        let vault =
            LocalCorpusConfig::obsidian_vault(PathBuf::from("/tmp/v"), PathBuf::from("/tmp/snap"));
        assert!(vault.source_type.should_reconcile());
        assert_eq!(
            vault.source_type.reconcile_kind(),
            Some(ReconcileKind::ObsidianVault)
        );
    }

    #[test]
    fn corpus_id_is_stable_for_same_path() {
        let a = LocalCorpusConfig::document_folder(PathBuf::from("/tmp/x"), "x".into());
        let b = LocalCorpusConfig::document_folder(PathBuf::from("/tmp/x"), "x".into());
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn corpus_id_differs_by_path() {
        let a = LocalCorpusConfig::document_folder(PathBuf::from("/tmp/a"), "a".into());
        let b = LocalCorpusConfig::document_folder(PathBuf::from("/tmp/b"), "b".into());
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn recipe_toml_parses_as_valid_recipe() {
        // The round-trip test: our generated TOML must parse cleanly
        // via corpus_engine::Recipe::from_toml. Catches any drift
        // between field names / defaults across crate versions.
        let cfg = LocalCorpusConfig::document_folder(
            PathBuf::from("/tmp/docs"),
            "City council 2024".into(),
        );
        let jsonl = PathBuf::from("/tmp/staged.jsonl");
        let toml = recipe_toml(&cfg, &jsonl);
        let recipe =
            corpus_engine::Recipe::from_toml(&toml).expect("generated recipe TOML must parse");
        assert_eq!(recipe.corpus.id, cfg.id);
        assert_eq!(recipe.corpus.scope.as_deref(), Some("local"));
        assert!(!recipe.corpus.mesh_sharing);
    }

    #[test]
    fn watched_folder_default_extensions_and_scope() {
        let cfg = LocalCorpusConfig::watched_folder(
            PathBuf::from("/tmp/notes"),
            "Research notes".into(),
            WatchedFolderConfig::default(),
        );
        assert_eq!(cfg.scope, CorpusScope::Local);
        assert!(cfg.id.starts_with("watched-"));
        // v1 format breadth: PDF + plain text + markdown + Word
        // docs + EPUBs + saved web pages.
        assert_eq!(
            cfg.extensions,
            vec![
                "pdf".to_string(),
                "txt".into(),
                "md".into(),
                "docx".into(),
                "epub".into(),
                "html".into(),
                "htm".into(),
                "mhtml".into(),
            ]
        );
        assert!(matches!(
            cfg.source_type,
            LocalCorpusSourceType::WatchedFolder(_)
        ));
        assert!(cfg.write_back.is_none()); // Read-only on source — no writeback path.
    }

    #[test]
    fn watched_folder_recipe_toml_pins_privacy_invariants() {
        // ARCH §7: privacy invariants must be structural. A watched
        // folder must serialise with `scope = "local"` and
        // `mesh_sharing = false` regardless of caller config. If a
        // future refactor parameterises either, this test fails first.
        let cfg = LocalCorpusConfig::watched_folder(
            PathBuf::from("/tmp/notes"),
            "Research notes".into(),
            WatchedFolderConfig::default(),
        );
        let jsonl = PathBuf::from("/tmp/staged.jsonl");
        let toml = recipe_toml(&cfg, &jsonl);
        let recipe =
            corpus_engine::Recipe::from_toml(&toml).expect("watched-folder recipe TOML must parse");
        assert_eq!(recipe.corpus.scope.as_deref(), Some("local"));
        assert!(!recipe.corpus.mesh_sharing);
        // 2026-06-10 obsidian audit: local corpora are the user's own
        // files — the recipe must declare `[retrieval] personal_scope`
        // so personal-scope retrieval retains them (the old prefix-only
        // filter silently dropped `watched-<hash>` corpus ids).
        assert!(recipe.retrieval.personal_scope);
    }

    #[test]
    fn display_meta_ssot_agrees_with_recipe_toml() {
        // The category gates the tiered retrieval surface
        // (`is_tiered_category`); the recipe stamp at ingest and the
        // manager's backfill/re-stamp must come from the same values.
        let jsonl = PathBuf::from("/tmp/staged.jsonl");

        let vault = LocalCorpusConfig::obsidian_vault(
            PathBuf::from("/tmp/vault"),
            PathBuf::from("/tmp/snapshots"),
        );
        let vault_recipe = corpus_engine::Recipe::from_toml(&recipe_toml(&vault, &jsonl))
            .expect("vault recipe parses");
        let vault_meta = display_meta(&vault.source_type).expect("vault has display");
        assert_eq!(
            vault_recipe
                .display
                .as_ref()
                .and_then(|d| d.category.clone()),
            vault_meta.category
        );
        assert_eq!(vault_meta.category.as_deref(), Some("vault"));

        let watched = LocalCorpusConfig::watched_folder(
            PathBuf::from("/tmp/notes"),
            "Research notes".into(),
            WatchedFolderConfig::default(),
        );
        let watched_recipe = corpus_engine::Recipe::from_toml(&recipe_toml(&watched, &jsonl))
            .expect("watched recipe parses");
        let watched_meta = display_meta(&watched.source_type).expect("watched folder has display");
        assert_eq!(
            watched_recipe
                .display
                .as_ref()
                .and_then(|d| d.category.clone()),
            watched_meta.category
        );
        assert_eq!(watched_meta.category.as_deref(), Some("watched_folder"));
    }

    #[test]
    fn deletion_guard_defaults_are_on_with_credible_thresholds() {
        let g = DeletionGuardConfig::default();
        assert!(g.enabled);
        assert_eq!(g.absolute_threshold, 100);
        assert!((g.fractional_threshold - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn watched_folder_config_defaults_match_spec() {
        let c = WatchedFolderConfig::default();
        assert_eq!(c.sweep_interval_secs, 120);
        assert_eq!(c.soft_delete_grace_secs, 7 * 86_400);
        assert!(!c.follow_symlinks);
        assert!(c.exclude_globs.is_empty());
        // v1 defaults — both opt-in features stay off until the user
        // deliberately enables them.
        assert_eq!(c.sync_mode, SyncMode::Continuous);
        assert!(!c.sensitive);
    }

    #[test]
    fn watched_folder_config_round_trips_pre_v1_sidecars() {
        // A JSON sidecar written before the v1 fields existed must
        // deserialise as the documented defaults — no manual
        // migration step. The serde(default) attributes are
        // load-bearing for any user with an existing watched corpus.
        let pre_v1 = r#"{
            "follow_symlinks": false,
            "deletion_guard": { "absolute_threshold": 100, "fractional_threshold": 0.25, "enabled": true },
            "sweep_interval_secs": 120,
            "soft_delete_grace_secs": 604800,
            "exclude_globs": []
        }"#;
        let c: WatchedFolderConfig =
            serde_json::from_str(pre_v1).expect("pre-v1 sidecar must deserialise");
        assert_eq!(c.sync_mode, SyncMode::Continuous);
        assert!(!c.sensitive);
        assert!(!c.with_ocr);
    }

    #[test]
    fn watched_folder_config_serializes_manual_sync_mode_lowercase() {
        // The on-disk representation is the snake_case string —
        // pinned so a refactor that drops `#[serde(rename_all)]`
        // can't silently break round-trip with existing sidecars.
        let mut c = WatchedFolderConfig::default();
        c.sync_mode = SyncMode::Manual;
        c.sensitive = true;
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains(r#""sync_mode":"manual""#), "got: {json}");
        assert!(json.contains(r#""sensitive":true"#), "got: {json}");
    }

    #[test]
    fn watched_enrichment_config_default_is_off() {
        // Folder-ingest v1 §3.3: enrichment defaults to Off.
        // Pinned because the spec is explicit: "off is always
        // safe and useful. Enabling is an investment the user
        // makes deliberately." A refactor that flips the default
        // to a per-pipeline enabled state breaks this contract.
        let c = WatchedFolderConfig::default();
        assert_eq!(c.enrichment, WatchedEnrichmentConfig::Off);
    }

    #[test]
    fn watched_enrichment_config_round_trips_on_variant() {
        let on = WatchedEnrichmentConfig::On {
            pipeline_id: "philosophy_atlas".into(),
            last_built_at_unix: 1_700_000_000,
            last_built_doc_count: 42,
        };
        let json = serde_json::to_string(&on).unwrap();
        assert!(json.contains(r#""kind":"on""#), "got: {json}");
        assert!(
            json.contains(r#""pipeline_id":"philosophy_atlas""#),
            "got: {json}"
        );
        let parsed: WatchedEnrichmentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, on);
    }

    #[test]
    fn watched_enrichment_config_pre_v1_sidecars_default_off() {
        // A pre-v1 watched-folder sidecar (no `enrichment` field)
        // must deserialize as `Off` so existing corpora upgrade
        // cleanly. Same `#[serde(default)]` pattern as
        // `additional_roots` and `sensitive`.
        let pre_v1 = r#"{
            "follow_symlinks": false,
            "deletion_guard": { "absolute_threshold": 100, "fractional_threshold": 0.25, "enabled": true },
            "sweep_interval_secs": 120,
            "soft_delete_grace_secs": 604800,
            "exclude_globs": []
        }"#;
        let c: WatchedFolderConfig = serde_json::from_str(pre_v1).unwrap();
        assert_eq!(c.enrichment, WatchedEnrichmentConfig::Off);
    }

    #[test]
    fn recipe_toml_escapes_quotes_and_backslashes() {
        let cfg = LocalCorpusConfig::document_folder(
            PathBuf::from(r#"/tmp/has "quotes" and \backslashes"#),
            r#"Alex's "Notes""#.into(),
        );
        let jsonl = PathBuf::from("/tmp/staged.jsonl");
        let toml = recipe_toml(&cfg, &jsonl);
        let recipe =
            corpus_engine::Recipe::from_toml(&toml).expect("escaped TOML must still parse");
        // Display name round-trips byte-for-byte.
        assert_eq!(recipe.corpus.name, r#"Alex's "Notes""#);
    }
}
