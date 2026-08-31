// SPDX-License-Identifier: AGPL-3.0-or-later
//! `structure_first` code-corpus branch.
//!
//! Walks an already-indexed code corpus (one chunk per symbol) plus
//! its companion SCIP graph and emits a three-tier atlas:
//!
//! ```text
//!     Crate ──contains──> Module ──contains──> Item
//!                                                 │
//!                                              uses/refs
//!                                                 ▼
//!                                              Item   (or external Crate)
//! ```
//!
//! All atoms have `enrichment_depth = Structural`. All edges are
//! `Involves` with provenance set to one of the four code-structural
//! variants on [`EdgeProvenance`]:
//!
//!   - `ContainmentStructural` — Crate→Module, Module→Item
//!   - `ScipStructural`        — Item→Item from the SCIP refs table
//!   - `CargoStructural`       — Crate→ExternalCrate dependency edges
//!   - `TreeSitterStructural`  — fallback for items SCIP didn't resolve
//!
//! No LLM calls. Output is byte-deterministic across runs against the
//! same workspace state (modulo metadata timestamps in
//! `schema_validation.json`): all collections are BTreeMap/BTreeSet,
//! and every atom id is a `AtomId::entity_content_hash` of the atom's
//! stable identity (crate/module canonical name, item qualified_name)
//! — NOT a positional counter — so rebuilding the same workspace yields
//! byte-identical ids, which is what lets the Inc-6 atoms-delta patch
//! dedup by id and upsert per-symbol without orphaning prior atoms.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use crate::engine::CorpusEngine;
use crate::enrichment::atlas::atoms::{AtomEnvelope, AtomId, AtomsFile, ChunkRef, Entity};
use crate::enrichment::atlas::atoms_delta::AtomsDelta;
use crate::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType, EdgesFile};
use crate::enrichment::atlas::ingestion::AtlasData;
use crate::enrichment::code_intel::SymbolEnrichment;
use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
use crate::error::{Error, Result};
use crate::progress::{IngestProgress, ProgressCallback};
use corpus_engine_scip::ScipGraph;

// ── Public config ───────────────────────────────────────────

/// Tunables for the code-walk pass. Surfaces on the CLI as
/// `--include-functions` / `--include-private` flags.
#[derive(Debug, Clone, Default)]
pub struct CodeWalkConfig {
    pub source_corpus_id: String,
    pub include_functions: bool,
    pub include_private: bool,
}

// ── Chunk metadata shape ────────────────────────────────────

/// Subset of [`crate::extractors::code::CodeChunk::metadata_json`]
/// the walker consumes. Defaulting `is_public`/`doc_comment` keeps
/// older indexes (written before those fields were added) readable
/// — they just look like all-private, all-undocumented corpora.
#[derive(Debug, Clone, Deserialize)]
struct CodeChunkMeta {
    #[serde(default)]
    pub symbol_name: String,
    #[serde(default)]
    pub symbol_kind: String,
    pub file_path: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub is_public: bool,
    #[serde(default)]
    pub doc_comment: Option<String>,
}

/// Returns true when the chunk metadata blob has the shape produced
/// by the code extractor (rather than the Wikipedia extractor).
/// Cheap O(1) test on the raw JSON; used by `structure_first` to
/// dispatch between the two branches without reading the recipe.
///
/// Also exposed as a public surface so integration tests can sanity-
/// check the dispatch signature against real fixtures.
pub fn metadata_looks_like_code(metadata_raw: &str) -> bool {
    // The presence of `language` + `file_path` + `symbol_name` keys
    // is a reliable signature; Wikipedia metadata uses
    // `section_path`, `section_type`, `outgoing_links`.
    let parsed: serde_json::Result<serde_json::Value> = serde_json::from_str(metadata_raw);
    match parsed {
        Ok(v) => {
            let obj = match v.as_object() {
                Some(o) => o,
                None => return false,
            };
            obj.contains_key("symbol_name")
                && obj.contains_key("file_path")
                && obj.contains_key("language")
        }
        Err(_) => false,
    }
}

// ── Workspace discovery ─────────────────────────────────────

/// One Cargo workspace member or single-crate root.
#[derive(Debug, Clone)]
struct CrateRecord {
    /// Crate name as declared in `[package] name`.
    name: String,
    /// Crate root directory, absolute.
    abs_root: PathBuf,
    /// Path of the crate root relative to the workspace source root
    /// — this is the prefix matched against chunk `file_path` to
    /// route a chunk to its crate.
    rel_root: PathBuf,
    /// Crate-root rustdoc (`//!` at top of `lib.rs` / `main.rs`).
    crate_rustdoc: Option<String>,
    /// Description from `[package] description`, used as fallback
    /// when `crate_rustdoc` is empty.
    cargo_description: Option<String>,
    /// Direct dependency names declared in this crate's Cargo.toml.
    /// Used for `Crate → ExternalCrate` edges.
    dependencies: BTreeSet<String>,
}

/// All crates discovered under the source corpus's root path.
#[derive(Debug, Default)]
struct WorkspaceMap {
    /// Crates keyed by name (deterministic iteration via BTreeMap).
    crates: BTreeMap<String, CrateRecord>,
    /// Source root the corpus was extracted from. Used for path
    /// arithmetic; not preserved in atlas output.
    #[allow(dead_code)]
    source_root: PathBuf,
}

impl WorkspaceMap {
    /// Find the crate containing a given chunk's `file_path`. Returns
    /// `None` for chunks outside any known crate (e.g., top-level
    /// scripts, README excerpts). Longest-prefix match: with two
    /// crates `sovereign/crates/sovereign-core` and
    /// `sovereign/crates/sovereign-core/src/runtime`, a chunk at the
    /// latter wins.
    fn crate_for_path(&self, file_path: &str) -> Option<&CrateRecord> {
        let path = Path::new(file_path);
        let mut best: Option<&CrateRecord> = None;
        let mut best_len = 0;
        for c in self.crates.values() {
            let prefix = c.rel_root.as_path();
            // Empty rel_root matches everything (single-crate root).
            // Otherwise the chunk path must start with the prefix.
            let matches = if prefix.as_os_str().is_empty() {
                true
            } else {
                path.starts_with(prefix)
            };
            if matches {
                let len = prefix.components().count();
                if best.is_none() || len > best_len {
                    best = Some(c);
                    best_len = len;
                }
            }
        }
        best
    }
}

/// Walk `source_root` for `Cargo.toml` files and parse each into a
/// [`CrateRecord`]. Skips workspace-only manifests (no `[package]`
/// table). Skips standard junk dirs (`target`, `.git`, `node_modules`).
fn discover_workspace(source_root: &Path) -> Result<WorkspaceMap> {
    let mut crates: BTreeMap<String, CrateRecord> = BTreeMap::new();
    for entry in walkdir::WalkDir::new(source_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !(e.depth() > 0
                && (name == ".git"
                    || name == "node_modules"
                    || name == "target"
                    || name == "dist"
                    || name == "build"
                    || name == "__pycache__"))
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name() != "Cargo.toml" {
            continue;
        }
        let toml_path = entry.path();
        let raw = match std::fs::read_to_string(toml_path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let parsed: toml::Value = match toml::from_str(&raw) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let pkg = match parsed.get("package").and_then(|v| v.as_table()) {
            Some(t) => t,
            None => continue, // Workspace-only manifest, skip.
        };
        let name = match pkg.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let cargo_description = pkg
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let abs_root = toml_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| source_root.to_path_buf());
        let rel_root = abs_root
            .strip_prefix(source_root)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| PathBuf::new());

        // Crate-root rustdoc: `//!` block at the top of lib.rs or
        // main.rs. Read up to first non-comment line.
        let crate_rustdoc = ["src/lib.rs", "src/main.rs"]
            .iter()
            .find_map(|rel| read_inner_rustdoc(&abs_root.join(rel)));

        // Dependencies. `[dependencies]`, `[dev-dependencies]`,
        // `[build-dependencies]`. Atlas treats them all the same —
        // an external dep is an external dep.
        let mut deps: BTreeSet<String> = BTreeSet::new();
        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(table) = parsed.get(section).and_then(|v| v.as_table()) {
                for k in table.keys() {
                    deps.insert(k.clone());
                }
            }
        }
        // Workspace-table targets (`[target.'cfg(...)'.dependencies]`)
        // are best-effort; the demo doesn't depend on capturing them.

        crates.insert(
            name.clone(),
            CrateRecord {
                name,
                abs_root,
                rel_root,
                crate_rustdoc,
                cargo_description,
                dependencies: deps,
            },
        );
    }
    Ok(WorkspaceMap {
        crates,
        source_root: source_root.to_path_buf(),
    })
}

/// Read the top-of-file `//!` rustdoc block from a Rust source file.
/// Stops at the first non-comment, non-blank line. Returns `None`
/// when the file is missing or carries no `//!` lines.
fn read_inner_rustdoc(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut out: Vec<&str> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            // Blank line allowed inside the block; preserve it as
            // paragraph separator.
            if !out.is_empty() {
                out.push("");
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("//!") {
            out.push(rest.trim_start_matches(' '));
        } else {
            break;
        }
    }
    let joined = out.join("\n").trim().to_string();
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

// ── Chunk aggregation ───────────────────────────────────────

/// All chunk locations grouped by their tier-relevant key.
#[derive(Debug, Default)]
struct ChunkGroups {
    /// Per-crate chunk count and a representative chunk id (first
    /// chunk seen for the crate, used for first_appearance).
    by_crate: BTreeMap<String, CrateGroup>,
    /// Per-module chunk count, primary file, first chunk id.
    by_module: BTreeMap<(String, String), ModuleGroup>,
    /// Items keyed by (crate, module, symbol_name, symbol_kind).
    /// Multiple chunks for the same item (oversized symbols split by
    /// the extractor) collapse into one record — first chunk wins.
    by_item: BTreeMap<ItemKey, ItemRecord>,
    /// Diagnostics.
    chunks_with_metadata: usize,
    chunks_without_metadata: usize,
    chunks_outside_workspace: usize,
}

#[derive(Debug, Clone)]
struct CrateGroup {
    first_chunk_id: u64,
    first_preview: String,
}

#[derive(Debug, Clone)]
struct ModuleGroup {
    first_chunk_id: u64,
    primary_file: String,
    first_preview: String,
    /// Doc comment from the primary file, when one exists.
    rustdoc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ItemKey {
    crate_name: String,
    module_path: String,
    symbol_name: String,
    symbol_kind: String,
}

#[derive(Debug, Clone)]
struct ItemRecord {
    chunk_id: u64,
    content_preview: String,
    doc_comment: Option<String>,
    /// Full SCIP descriptor for this item, looked up from
    /// `symbols_in_file` during aggregation. `None` when SCIP
    /// doesn't carry this item — the atlas walker will skip
    /// SCIP-edge emission for it but still emit containment edges.
    qualified_name: Option<String>,
    /// Carried so the SCIP-lookup pass can find the matching row.
    file_path: String,
}

/// Convert a chunk's repo-relative file path into a `(module_path,
/// is_module_root)` pair, given its containing crate.
///
///   `src/engine/mod.rs`             → ("engine", true)
///   `src/engine/ingest.rs`          → ("engine::ingest", false)
///   `src/lib.rs`                    → ("", true)
///   `src/main.rs`                   → ("", true)
///   `src/extractors/code/mod.rs`    → ("extractors::code", true)
fn module_path_for(file_path: &Path, crate_rel_root: &Path) -> Option<(String, bool)> {
    // Strip the crate root + `src/` prefix.
    let rest = file_path.strip_prefix(crate_rel_root).ok()?;
    let rest = rest.strip_prefix("src").unwrap_or(rest);
    let rest = rest
        .strip_prefix(std::path::MAIN_SEPARATOR.to_string())
        .unwrap_or(rest);

    let components: Vec<String> = rest
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if components.is_empty() {
        return Some((String::new(), true));
    }
    let last = components.last().cloned().unwrap_or_default();
    let is_root = matches!(last.as_str(), "mod.rs" | "lib.rs" | "main.rs");
    let mut parts: Vec<String> = if is_root {
        components
            .iter()
            .take(components.len() - 1)
            .cloned()
            .collect()
    } else {
        let mut p: Vec<String> = components
            .iter()
            .take(components.len() - 1)
            .cloned()
            .collect();
        if let Some(stem) = Path::new(&last).file_stem().and_then(|s| s.to_str()) {
            p.push(stem.to_string());
        }
        p
    };
    // Drop a leading "src" if the crate root has it implicit (some
    // odd manifests put modules outside src/).
    if parts.first().map(String::as_str) == Some("src") {
        parts.remove(0);
    }
    Some((parts.join("::"), is_root))
}

async fn aggregate_chunks(
    corpus: &CorpusEngine,
    workspace: &WorkspaceMap,
    cfg: &CodeWalkConfig,
) -> Result<ChunkGroups> {
    // TRANSIENT: same shape as structure_first — one bulk aggregate over
    // the source, never a query.
    let index = corpus
        .open_index_for_corpus_transient(&cfg.source_corpus_id)
        .await
        .map_err(|e| {
            Error::Database(format!(
                "open source corpus `{}`: {e}",
                cfg.source_corpus_id
            ))
        })?;
    let chunks = index.all_chunks_full().await?;

    let mut groups = ChunkGroups::default();
    for chunk in chunks {
        let raw = match chunk.metadata_raw.as_deref() {
            Some(r) => r,
            None => {
                groups.chunks_without_metadata += 1;
                continue;
            }
        };
        let meta: CodeChunkMeta = match serde_json::from_str(raw) {
            Ok(m) => m,
            Err(_) => {
                groups.chunks_without_metadata += 1;
                continue;
            }
        };
        groups.chunks_with_metadata += 1;

        // Skip non-source extensions defensively (extractor already
        // filters; this catches stale or hand-rolled corpora).
        if meta.language.is_empty() {
            groups.chunks_outside_workspace += 1;
            continue;
        }

        // Locate crate.
        let krate = match workspace.crate_for_path(&meta.file_path) {
            Some(c) => c,
            None => {
                groups.chunks_outside_workspace += 1;
                continue;
            }
        };

        // Module path.
        let path = Path::new(&meta.file_path);
        let (module_path, is_module_root) = match module_path_for(path, &krate.rel_root) {
            Some(pair) => pair,
            None => {
                groups.chunks_outside_workspace += 1;
                continue;
            }
        };

        let preview = preview_text(&chunk.content, 120);

        // Crate-level group.
        groups
            .by_crate
            .entry(krate.name.clone())
            .or_insert_with(|| CrateGroup {
                first_chunk_id: chunk.id,
                first_preview: preview.clone(),
            });

        // Module-level group. The "primary file" is the file with
        // the matching mod.rs / lib.rs / main.rs name, when one
        // exists; otherwise the first file that registered the
        // module group wins.
        let key = (krate.name.clone(), module_path.clone());
        let module_entry = groups.by_module.entry(key).or_insert_with(|| ModuleGroup {
            first_chunk_id: chunk.id,
            primary_file: meta.file_path.clone(),
            first_preview: preview.clone(),
            rustdoc: None,
        });
        if is_module_root && module_entry.rustdoc.is_none() {
            // Read the rustdoc from disk so we get the file-level
            // `//!` block (chunks themselves don't carry it, since
            // they're symbol-scoped).
            let abs_path = krate.abs_root.join(
                Path::new(&meta.file_path)
                    .strip_prefix(&krate.rel_root)
                    .unwrap_or_else(|_| Path::new(&meta.file_path)),
            );
            module_entry.rustdoc = read_inner_rustdoc(&abs_path);
            // Promote this file as the primary for the module if
            // we hadn't already locked one in.
            module_entry.primary_file = meta.file_path.clone();
        }

        // Item-level group. Filter by visibility and kind here so
        // the maps stay small.
        if !cfg.include_private && !meta.is_public {
            continue;
        }
        if !cfg.include_functions
            && (meta.symbol_kind == "function" || meta.symbol_kind == "method")
        {
            continue;
        }
        // Skip impl blocks — they're not entity-shaped (no name).
        if meta.symbol_kind == "impl" {
            continue;
        }

        let item_key = ItemKey {
            crate_name: krate.name.clone(),
            module_path: module_path.clone(),
            symbol_name: meta.symbol_name.clone(),
            symbol_kind: meta.symbol_kind.clone(),
        };
        groups
            .by_item
            .entry(item_key)
            .or_insert_with(|| ItemRecord {
                chunk_id: chunk.id,
                content_preview: preview,
                doc_comment: meta.doc_comment.clone(),
                qualified_name: None, // populated by attach_qualified_names
                file_path: meta.file_path.clone(),
            });
    }

    Ok(groups)
}

// ── Entity emission ─────────────────────────────────────────

/// Maps each tier's logical key to its assigned `AtomId`. The ids
/// themselves are content-hashes (`entity-<16 hex>`) of the atom's
/// stable identity + entity_type + corpus_id (see [`emit_entities`]);
/// this map is the local crate/module/item/external → id lookup the
/// edge pass resolves source/target atoms through.
#[derive(Debug, Default)]
struct AtomIndex {
    crates: BTreeMap<String, AtomId>,
    modules: BTreeMap<(String, String), AtomId>,
    items: BTreeMap<ItemKey, AtomId>,
    externals: BTreeMap<String, AtomId>,
}

fn module_canonical(crate_name: &str, module_path: &str) -> String {
    if module_path.is_empty() {
        crate_name.replace('-', "_")
    } else {
        format!("{}::{}", crate_name.replace('-', "_"), module_path)
    }
}

fn item_canonical(crate_name: &str, module_path: &str, symbol: &str) -> String {
    let m = module_canonical(crate_name, module_path);
    format!("{m}::{symbol}")
}

fn entity_type_for_module() -> EntityType {
    EntityType::Other("module".to_string())
}

fn entity_type_for_crate() -> EntityType {
    EntityType::Other("crate".to_string())
}

fn entity_type_for_external_crate() -> EntityType {
    EntityType::Other("external_crate".to_string())
}

fn entity_type_for_item(symbol_kind: &str) -> EntityType {
    EntityType::Other(symbol_kind.to_string())
}

fn first_doc_paragraph(doc: &str, max_chars: usize) -> String {
    let trimmed = doc.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // First blank-line-separated paragraph.
    let para = trimmed.split("\n\n").next().unwrap_or(trimmed).trim();
    // Then first sentence.
    let end = para
        .find(". ")
        .or_else(|| para.find("? "))
        .or_else(|| para.find("! "))
        .map(|i| i + 1)
        .unwrap_or(para.len());
    let candidate = &para[..end];
    if candidate.chars().count() <= max_chars {
        candidate.to_string()
    } else {
        candidate.chars().take(max_chars).collect::<String>() + "…"
    }
}

fn preview_text(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        trimmed.chars().take(max_chars).collect::<String>() + "…"
    }
}

/// Load the corpus's code-intel summary cache (`code_intel_cache.json`,
/// a `Vec<SymbolEnrichment>` written by the code-intel enrichment pass)
/// keyed by `meta.qualified_name` — the SAME SCIP descriptor that
/// `attach_qualified_names` stamps onto items (both read the symbols
/// table's `qualified_name` column), so a summary joins to its item by
/// exact qualified-name match. Best-effort + graceful: a missing or
/// unparseable cache yields an empty map and the walker falls back to
/// the rustdoc first-paragraph (no error). Mirrors `code_intel::pass`'s
/// loader but re-keys by qualified_name instead of body_hash.
fn load_code_intel_summaries(
    index_dir: &Path,
    canonical_corpus_id: &str,
) -> HashMap<String, SymbolEnrichment> {
    let path = index_dir
        .join(canonical_corpus_id)
        .join("code_intel_cache.json");
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str::<Vec<SymbolEnrichment>>(&s)
            .map(|v| {
                v.into_iter()
                    .map(|e| (e.meta.qualified_name.clone(), e))
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Produce all entity atoms (crate, module, item, external) and the
/// AtomIndex used by edge emission.
///
/// Every atom id is `AtomId::entity_content_hash(stable_name,
/// entity_type, canonical_corpus_id)` — a content hash, NOT a positional
/// counter — so rebuilding the same workspace yields byte-identical ids
/// (Inc 1, the patchability invariant). `stable_name` per tier:
///   - crate  → crate name
///   - module → `module_canonical(crate, module_path)`
///   - item   → `qualified_name` when SCIP carried it, else `item_canonical`
///   - external → the external crate name
///
/// `first_appearance.chunk_id` is set to the STABLE per-symbol doc-id
/// (what `doc_to_atoms::extract_doc_id` returns and Inc-6 upserts by),
/// NOT the numeric lance chunk id — the latter is preserved in
/// `attributes["chunk_id"]` so source retrieval still works.
///
/// Item descriptions prefer the code-intel plain-English `summary`
/// (joined by qualified_name through `code_intel`) and fall back to the
/// rustdoc first paragraph (Inc 2). Iteration stays on the sorted
/// BTreeMaps so atom *ordering* in the file is also deterministic.
fn emit_entities(
    workspace: &WorkspaceMap,
    groups: &ChunkGroups,
    referenced_externals: &BTreeSet<String>,
    canonical_corpus_id: &str,
    code_intel: &HashMap<String, SymbolEnrichment>,
) -> (Vec<Entity>, AtomIndex) {
    let mut entities: Vec<Entity> = Vec::new();
    let mut idx = AtomIndex::default();

    // Crates.
    for (name, krate) in &workspace.crates {
        let group = groups.by_crate.get(name);
        let (chunk_id, preview) = match group {
            Some(g) => (g.first_chunk_id, g.first_preview.clone()),
            // Crate has no chunks (empty crate / not yet indexed).
            // Description still usable; the doc-anchor is the crate name.
            None => (0, String::new()),
        };
        let description = krate
            .crate_rustdoc
            .as_deref()
            .map(|s| first_doc_paragraph(s, 280))
            .filter(|s| !s.is_empty())
            .or_else(|| krate.cargo_description.clone())
            .unwrap_or_default();
        let entity_type = entity_type_for_crate();
        let atom_id = AtomId::entity_content_hash(name, &entity_type, canonical_corpus_id);
        idx.crates.insert(name.clone(), atom_id.clone());
        // Preserve source retrieval: first_appearance now carries the stable
        // doc-anchor (the crate name), so stash the numeric lance chunk id of
        // the crate's representative chunk here — same treatment as items.
        let mut attributes = serde_json::Map::new();
        attributes.insert(
            "chunk_id".to_string(),
            serde_json::Value::String(chunk_id.to_string()),
        );
        entities.push(Entity {
            id: atom_id,
            canonical_name: name.replace('-', "_"),
            aliases: if name.contains('-') {
                vec![name.clone()]
            } else {
                Vec::new()
            },
            entity_type,
            // Stable doc-anchor: the crate name (NOT the numeric chunk id).
            first_appearance: ChunkRef::new(name.clone(), Some(preview)),
            description,
            defining_quote: None,
            salience: 0.5, // overwritten by compute_salience
            enrichment_depth: EnrichmentDepth::Structural,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes,
            concept_kind: None,
        });
    }

    // Modules.
    for ((crate_name, module_path), group) in &groups.by_module {
        let canonical = module_canonical(crate_name, module_path);
        let description = group
            .rustdoc
            .as_deref()
            .map(|s| first_doc_paragraph(s, 280))
            .unwrap_or_default();
        let entity_type = entity_type_for_module();
        let atom_id = AtomId::entity_content_hash(&canonical, &entity_type, canonical_corpus_id);
        idx.modules
            .insert((crate_name.clone(), module_path.clone()), atom_id.clone());
        // Preserve source retrieval: stash the module's representative lance
        // chunk id (first_appearance now holds the stable module_canonical
        // doc-anchor) — same treatment as items + crates.
        let mut attributes = serde_json::Map::new();
        attributes.insert(
            "chunk_id".to_string(),
            serde_json::Value::String(group.first_chunk_id.to_string()),
        );
        entities.push(Entity {
            id: atom_id,
            canonical_name: canonical.clone(),
            aliases: Vec::new(),
            entity_type,
            // Stable doc-anchor: module_canonical (NOT the numeric chunk id).
            first_appearance: ChunkRef::new(canonical, Some(group.first_preview.clone())),
            description,
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Structural,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes,
            concept_kind: None,
        });
    }

    // Items.
    for (key, item) in &groups.by_item {
        let canonical = item_canonical(&key.crate_name, &key.module_path, &key.symbol_name);
        let entity_type = entity_type_for_item(&key.symbol_kind);

        // Stable identity for the content-hash id: the SCIP qualified_name
        // (survives line shifts; the join key the code-intel cache + the
        // Inc-6 patch key by), falling back to the module-qualified
        // canonical name when SCIP didn't carry this item.
        let id_name = item
            .qualified_name
            .clone()
            .unwrap_or_else(|| canonical.clone());
        // Stable per-symbol doc-id for first_appearance.chunk_id — exactly
        // what `extract_doc_id(Entity)` returns and the Inc-6 patch upserts
        // by: qualified_name, or `<file>#<symbol>` when SCIP is silent.
        let doc_anchor = item
            .qualified_name
            .clone()
            .unwrap_or_else(|| format!("{}#{}", item.file_path, key.symbol_name));

        // Inc 2: prefer the code-intel plain-English summary (joined by
        // qualified_name) over the rustdoc first paragraph.
        let summary = item
            .qualified_name
            .as_deref()
            .and_then(|q| code_intel.get(q));
        let description = match summary {
            Some(e) => e.summary.clone(),
            None => item
                .doc_comment
                .as_deref()
                .map(|s| first_doc_paragraph(s, 280))
                .unwrap_or_default(),
        };

        let mut attributes = serde_json::Map::new();
        // Preserve source retrieval: the numeric lance chunk id no longer
        // lives in first_appearance (now the stable doc-id), so stash it
        // here for any reader that needs to fetch the symbol's raw chunk.
        attributes.insert(
            "chunk_id".to_string(),
            serde_json::Value::String(item.chunk_id.to_string()),
        );
        if let Some(e) = summary {
            attributes.insert(
                "asks".to_string(),
                serde_json::Value::Array(
                    e.asks
                        .iter()
                        .map(|a| serde_json::Value::String(a.clone()))
                        .collect(),
                ),
            );
            attributes.insert(
                "summary_body_hash".to_string(),
                serde_json::Value::String(e.body_hash.clone()),
            );
            attributes.insert(
                "source".to_string(),
                serde_json::Value::String("code_intel_summary".to_string()),
            );
        }

        let atom_id = AtomId::entity_content_hash(&id_name, &entity_type, canonical_corpus_id);
        idx.items.insert(key.clone(), atom_id.clone());
        entities.push(Entity {
            id: atom_id,
            canonical_name: canonical,
            aliases: vec![key.symbol_name.clone()],
            entity_type,
            first_appearance: ChunkRef::new(doc_anchor, Some(item.content_preview.clone()))
                .with_source_doc(Some(item.file_path.clone())),
            description,
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Structural,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes,
            concept_kind: None,
        });
    }

    // Externals (placeholders). Match the Wikipedia off-corpus pattern:
    // empty description, salience 0, enrichment_depth = Structural. The id
    // is content-hashed on the external name so it too is stable across
    // rebuilds; first_appearance stays a synthetic "0" (externals are
    // off-corpus — they have no source doc to anchor to).
    for ext_name in referenced_externals {
        if workspace.crates.contains_key(ext_name) {
            continue; // It's actually a workspace member.
        }
        let entity_type = entity_type_for_external_crate();
        let atom_id = AtomId::entity_content_hash(ext_name, &entity_type, canonical_corpus_id);
        idx.externals.insert(ext_name.clone(), atom_id.clone());
        entities.push(Entity {
            id: atom_id,
            canonical_name: ext_name.clone(),
            aliases: Vec::new(),
            entity_type,
            first_appearance: ChunkRef::new("0".to_string(), None),
            description: String::new(),
            defining_quote: None,
            salience: 0.0,
            enrichment_depth: EnrichmentDepth::Structural,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        });
    }

    (entities, idx)
}

// ── Edge emission ───────────────────────────────────────────

async fn emit_edges(
    workspace: &WorkspaceMap,
    groups: &ChunkGroups,
    atom_index: &AtomIndex,
    scip: Option<&ScipGraph>,
) -> Result<(Vec<Edge>, EdgeStats)> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut next_idx: usize = 1;
    let mut stats = EdgeStats::default();

    // Containment edges: Crate → Module (BTreeMap iteration is sorted).
    for (crate_name, module_path) in groups.by_module.keys() {
        let (Some(crate_atom), Some(module_atom)) = (
            atom_index.crates.get(crate_name),
            atom_index
                .modules
                .get(&(crate_name.clone(), module_path.clone())),
        ) else {
            continue;
        };
        edges.push(Edge {
            id: EdgeId::new(next_idx),
            edge_type: EdgeType::Involves,
            source: crate_atom.clone(),
            target: module_atom.clone(),
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::ContainmentStructural,
        });
        next_idx += 1;
        stats.containment += 1;
    }

    // Containment edges: Module → Item.
    for key in groups.by_item.keys() {
        let (Some(module_atom), Some(item_atom)) = (
            atom_index
                .modules
                .get(&(key.crate_name.clone(), key.module_path.clone())),
            atom_index.items.get(key),
        ) else {
            continue;
        };
        edges.push(Edge {
            id: EdgeId::new(next_idx),
            edge_type: EdgeType::Involves,
            source: module_atom.clone(),
            target: item_atom.clone(),
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::ContainmentStructural,
        });
        next_idx += 1;
        stats.containment += 1;
    }

    // SCIP cross-reference edges. With schema v3, every symbol +
    // ref carries its full SCIP descriptor (`qualified_name` /
    // `caller_qualified` / `callee_qualified`). That gives us
    // unambiguous cross-crate resolution: two items named `Error`
    // in different modules have different qualified_names and
    // resolve to different atoms.
    //
    // Build a `qualified_name -> AtomId` reverse index over items
    // that successfully picked up a qualified_name during the
    // SCIP-attach pass. Items that didn't (SCIP doesn't index them
    // — macro-generated, uncovered language) get no SCIP edges.
    let qualified_to_atom = build_qualified_to_atom(atom_index, groups);
    // Inc 3 fanout bound. A single function references stdlib + helpers
    // dozens of times; with the function tier enabled (~22k functions ×
    // their refs) an uncapped SCIP edge pass explodes the graph. Two
    // deterministic guards:
    //   1. Cap emitted callee edges per source symbol at MAX_CALLEE_FANOUT.
    //      `find_callees_qualified` returns rows ORDER BY (file_path, line),
    //      so "the first N" is stable across rebuilds.
    //   2. For function-tier sources, drop edges whose target is an EXTERNAL
    //      placeholder — keep first-party fn→fn edges; the Crate→ExternalCrate
    //      Cargo edges below still record the dependency structurally.
    // `MAX_CALLEE_FANOUT` is module-level (shared with the Inc-6 scoped path).
    let mut scip_seen: BTreeSet<(AtomId, AtomId)> = BTreeSet::new();
    if let Some(graph) = scip {
        for (key, item) in &groups.by_item {
            let Some(source_atom) = atom_index.items.get(key) else {
                continue;
            };
            let Some(caller_q) = &item.qualified_name else {
                continue; // not in SCIP, no callees to query
            };
            let source_is_function = key.symbol_kind == "function" || key.symbol_kind == "method";
            let callees = match graph.find_callees_qualified(caller_q).await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let mut emitted_for_source = 0usize;
            for callee in callees {
                if emitted_for_source >= MAX_CALLEE_FANOUT {
                    break;
                }
                if callee.callee_qualified.is_empty() {
                    // Legacy/v2 ref with no qualified — skip rather
                    // than fall back to bare-name (avoids the
                    // disambiguation problem we built v3 to fix).
                    continue;
                }
                let Some((target_atom, target_is_external)) = resolve_callee_target(
                    &qualified_to_atom,
                    atom_index,
                    workspace,
                    &callee.callee_qualified,
                ) else {
                    continue;
                };
                // Function tier: skip fn→external edges (guard #2).
                if source_is_function && target_is_external {
                    continue;
                }
                let pair = (source_atom.clone(), target_atom.clone());
                if !scip_seen.insert(pair) {
                    continue;
                }
                edges.push(Edge {
                    id: EdgeId::new(next_idx),
                    edge_type: EdgeType::Involves,
                    source: source_atom.clone(),
                    target: target_atom,
                    evidence: Vec::new(),
                    trigger_event: None,
                    sub_question: None,
                    confidence: 1.0,
                    provenance: EdgeProvenance::ScipStructural,
                });
                next_idx += 1;
                stats.scip += 1;
                emitted_for_source += 1;
            }
        }
    }

    // Cargo dependency edges: Crate → ExternalCrate (only when the
    // external is referenced by SCIP — i.e., showed up in
    // `atom_index.externals`). For dev-deps we emit too, since the
    // distinction isn't structurally meaningful for the demo.
    for (crate_name, krate) in &workspace.crates {
        let Some(crate_atom) = atom_index.crates.get(crate_name) else {
            continue;
        };
        for dep in &krate.dependencies {
            // Workspace members get a SCIP-driven edge already; skip.
            if workspace.crates.contains_key(dep) {
                continue;
            }
            let Some(ext_atom) = atom_index.externals.get(dep) else {
                continue;
            };
            edges.push(Edge {
                id: EdgeId::new(next_idx),
                edge_type: EdgeType::Involves,
                source: crate_atom.clone(),
                target: ext_atom.clone(),
                evidence: Vec::new(),
                trigger_event: None,
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::CargoStructural,
            });
            next_idx += 1;
            stats.cargo += 1;
        }
    }

    Ok((edges, stats))
}

/// Reverse index `qualified_name -> AtomId` over items that picked up a
/// qualified_name during the SCIP-attach pass. The exact + unambiguous
/// resolution key for SCIP cross-reference edges (two `Error`s in different
/// modules have different qualified_names → different atoms). Shared by the
/// full-build [`emit_edges`] and the Inc-6 scoped delta builder
/// [`extract_atoms_for_symbols`].
fn build_qualified_to_atom(
    atom_index: &AtomIndex,
    groups: &ChunkGroups,
) -> HashMap<String, AtomId> {
    let mut qualified_to_atom: HashMap<String, AtomId> = HashMap::new();
    for (key, atom) in &atom_index.items {
        if let Some(item) = groups.by_item.get(key) {
            if let Some(q) = &item.qualified_name {
                qualified_to_atom.insert(q.clone(), atom.clone());
            }
        }
    }
    qualified_to_atom
}

/// Resolve a SCIP callee descriptor to its target atom + an `is_external`
/// flag. First-party items win by exact qualified-name match; otherwise the
/// descriptor's package field routes to an external-crate placeholder. `None`
/// when neither resolves (skip the edge). Shared by [`emit_edges`] and the
/// Inc-6 scoped delta builder so a patched function's edges resolve targets
/// byte-identically to a full rebuild.
fn resolve_callee_target(
    qualified_to_atom: &HashMap<String, AtomId>,
    atom_index: &AtomIndex,
    workspace: &WorkspaceMap,
    callee_qualified: &str,
) -> Option<(AtomId, bool)> {
    if let Some(a) = qualified_to_atom.get(callee_qualified) {
        Some((a.clone(), false))
    } else {
        scip_descriptor_to_external(atom_index, workspace, callee_qualified).map(|a| (a, true))
    }
}

/// Deterministic, collision-free edge id for an Inc-6 delta-re-emitted edge.
/// Derived from `(tag, source, target)` — the endpoints are already
/// content-hash atom ids, so the pair is unique per edge and STABLE across
/// re-patches of the same symbol (re-emitting the same edge yields the same
/// id → `apply_atom_delta` replaces in place rather than duplicating). The
/// `tag` prefix (`cont` / `scip`) keeps these out of the full build's
/// sequential `edge-NNNNN` namespace, so a delta never accidentally
/// overwrites an unrelated full-build edge.
fn stable_edge_id(tag: &str, source: &AtomId, target: &AtomId) -> EdgeId {
    EdgeId::from_raw(format!("{tag}:{}>{}", source.as_str(), target.as_str()))
}

/// Bulk-attach `qualified_name` from SCIP to every item in
/// `groups.by_item`. The chunk metadata can't carry this — chunks
/// come from the tree-sitter walker, qualified_names come from
/// rust-analyzer's SCIP export. Match by `(file_path, name)`: the
/// SCIP `symbols` table has both, and `(file, name)` is unique
/// enough in practice. Returns the count of items that picked up
/// a qualified_name.
async fn attach_qualified_names(scip: &ScipGraph, groups: &mut ChunkGroups) -> Result<usize> {
    // Build a per-file cache so we hit SCIP once per file rather
    // than once per item (~10× fewer queries for typical files).
    let mut file_cache: HashMap<String, Vec<corpus_engine_scip::SymbolRow>> = HashMap::new();
    let mut attached = 0usize;
    for (key, item) in groups.by_item.iter_mut() {
        let rows = if let Some(cached) = file_cache.get(&item.file_path) {
            cached
        } else {
            let rows = scip
                .symbols_in_file(&item.file_path)
                .await
                .unwrap_or_default();
            file_cache.entry(item.file_path.clone()).or_insert(rows)
        };
        if let Some(row) = rows.iter().find(|r| r.name == key.symbol_name) {
            if !row.qualified_name.is_empty() {
                item.qualified_name = Some(row.qualified_name.clone());
                attached += 1;
            }
        }
    }
    Ok(attached)
}

/// Parse the third whitespace-separated token of a rust-analyzer
/// SCIP descriptor — that's the package/crate name. Returns `None`
/// for non-rust-analyzer or malformed descriptors.
///
/// Format: `<scheme> <manager> <package> <version> <descriptor...>`
/// Example: `rust-analyzer cargo corpus_engine 0.1.0 src/engine/mod.rs/CorpusEngine#`
fn scip_descriptor_crate(qualified_name: &str) -> Option<&str> {
    let mut tokens = qualified_name.split_whitespace();
    let _scheme = tokens.next()?;
    let _manager = tokens.next()?;
    let pkg = tokens.next()?;
    Some(pkg)
}

/// Resolve a callee SCIP descriptor to an external-crate AtomId
/// when the descriptor's package field doesn't match any
/// workspace crate. Returns `None` when the package is in the
/// workspace (the caller should have already found a qualified
/// match in that case) or unknown to the externals table.
fn scip_descriptor_to_external(
    atom_index: &AtomIndex,
    workspace: &WorkspaceMap,
    qualified_name: &str,
) -> Option<AtomId> {
    let pkg = scip_descriptor_crate(qualified_name)?;
    let pkg_dash = pkg.replace('_', "-");
    if workspace.crates.contains_key(pkg)
        || workspace.crates.contains_key(&pkg_dash)
        || workspace.crates.keys().any(|k| k.replace('-', "_") == pkg)
    {
        return None;
    }
    atom_index
        .externals
        .get(pkg)
        .or_else(|| atom_index.externals.get(&pkg_dash))
        .cloned()
}

/// Mine external-crate names from SCIP callees. With qualified
/// names available, we can parse the package field of each callee
/// and union the unique non-workspace packages into `out`.
async fn collect_externals_from_scip(
    scip: &ScipGraph,
    workspace: &WorkspaceMap,
    groups: &ChunkGroups,
    out: &mut BTreeSet<String>,
) -> Result<()> {
    for item in groups.by_item.values() {
        let Some(caller_q) = &item.qualified_name else {
            continue;
        };
        let callees = match scip.find_callees_qualified(caller_q).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        for callee in callees {
            if callee.callee_qualified.is_empty() {
                continue;
            }
            let Some(pkg) = scip_descriptor_crate(&callee.callee_qualified) else {
                continue;
            };
            let pkg_dash = pkg.replace('_', "-");
            let in_workspace = workspace.crates.contains_key(pkg)
                || workspace.crates.contains_key(&pkg_dash)
                || workspace.crates.keys().any(|k| k.replace('-', "_") == pkg);
            if !in_workspace {
                out.insert(pkg.to_string());
            }
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
struct EdgeStats {
    containment: usize,
    scip: usize,
    cargo: usize,
}

// ── Salience ────────────────────────────────────────────────

/// Compute per-tier in-degree salience and overwrite each entity's
/// `salience` field. Crate / module / item have separate normalisers
/// so a small workspace doesn't drown its modules under crate-tier
/// totals. External crates keep `salience = 0.0` (their value is
/// "this dep exists" — not a centrality signal).
fn compute_salience(mut entities: Vec<Entity>, edges: &[Edge]) -> Vec<Entity> {
    let mut in_degree: HashMap<AtomId, usize> = HashMap::new();
    for edge in edges {
        *in_degree.entry(edge.target.clone()).or_insert(0) += 1;
    }

    let mut max_per_kind: HashMap<&'static str, usize> = HashMap::new();
    for ent in &entities {
        let kind = entity_kind_str(&ent.entity_type);
        let degree = in_degree.get(&ent.id).copied().unwrap_or(0);
        let slot = max_per_kind.entry(kind).or_insert(0);
        if degree > *slot {
            *slot = degree;
        }
    }
    for ent in entities.iter_mut() {
        let kind = entity_kind_str(&ent.entity_type);
        if kind == "external_crate" {
            ent.salience = 0.0;
            continue;
        }
        let degree = in_degree.get(&ent.id).copied().unwrap_or(0);
        let max = max_per_kind.get(kind).copied().unwrap_or(1).max(1);
        ent.salience = (degree as f32 / max as f32).clamp(0.0, 1.0);
    }
    entities
}

fn entity_kind_str(t: &EntityType) -> &'static str {
    match t {
        EntityType::Other(s) => match s.as_str() {
            "crate" => "crate",
            "module" => "module",
            "external_crate" => "external_crate",
            _ => "item",
        },
        _ => "item",
    }
}

// ── Public entry ────────────────────────────────────────────

/// The deterministic, no-LLM result of phases 1-5 of the code-walk: the
/// workspace map, the per-tier chunk groups (with SCIP qualified_names
/// attached), the full crate/module/item/external [`AtomIndex`], every
/// entity atom (pre-salience), the open SCIP graph, and the canonical corpus
/// id. Shared by the full build ([`extract_code_corpus`]) and the Inc-6
/// scoped delta builder ([`extract_atoms_for_symbols`]) so both derive
/// byte-identical atom ids + edge targets — the whole point of the
/// content-hash id scheme is that a patch can rebuild a single symbol's atom
/// and have it line up against the live atlas.
struct PreparedCodeAtlas {
    workspace: WorkspaceMap,
    groups: ChunkGroups,
    atom_index: AtomIndex,
    /// Entity atoms in deterministic (BTreeMap) order, BEFORE
    /// `compute_salience` overwrites the per-tier in-degree salience. The
    /// delta path keeps the 0.5 default (salience is a GLOBAL in-degree
    /// statistic — see the module note + Inc-6 limitation: a per-symbol
    /// patch does not recompute it).
    entities: Vec<Entity>,
    scip: Option<ScipGraph>,
}

/// Run code-walk phases 1-5 (read meta → discover workspace → aggregate
/// chunks → open SCIP + attach qualified_names → collect externals → emit
/// entities). Pure data derivation, no LLM, no edges, no salience. Both the
/// full build and the scoped delta start here so the atom universe + ids
/// match exactly.
async fn prepare_code_atlas(
    corpus: &CorpusEngine,
    cfg: &CodeWalkConfig,
) -> Result<PreparedCodeAtlas> {
    // Phase 1: read the corpus's source root + canonical corpus_id
    // from `_corpus_meta.json`, then walk the source for Cargo.toml
    // manifests. The canonical corpus_id may differ from the
    // directory name — partition-aware indexes land at
    // `<canonical>-partition-<node>/` while the SCIP graph lives at
    // `<canonical>/scip_graph.db`. Reading the meta lets us bridge
    // the two without hard-coding the partitioning convention.
    let (source_root, canonical_corpus_id) =
        read_source_metadata(corpus.index_dir(), &cfg.source_corpus_id)?;
    tracing::info!(
        source_corpus = %cfg.source_corpus_id,
        source_root = %source_root.display(),
        "code_walk: discovering workspace"
    );
    let workspace = discover_workspace(&source_root)?;
    tracing::info!(
        crates = workspace.crates.len(),
        "code_walk: workspace discovered"
    );

    // Phase 2: aggregate chunks into crate / module / item groups.
    let mut groups = aggregate_chunks(corpus, &workspace, cfg).await?;
    tracing::info!(
        crates = groups.by_crate.len(),
        modules = groups.by_module.len(),
        items = groups.by_item.len(),
        with_metadata = groups.chunks_with_metadata,
        without_metadata = groups.chunks_without_metadata,
        outside_workspace = groups.chunks_outside_workspace,
        "code_walk: chunks aggregated"
    );

    // Phase 3: open the SCIP graph (best-effort; missing graph =>
    // skip cross-reference edges, fall back to containment + Cargo).
    // Try the canonical corpus_id first, then fall back to the
    // partition-keyed path. This handles both the partitioned
    // layout (chunks at `<canonical>-partition-<node>/`, SCIP at
    // `<canonical>/`) and the legacy single-dir layout (both at
    // `<canonical>/`).
    let canonical_scip = corpus
        .index_dir()
        .join(&canonical_corpus_id)
        .join("scip_graph.db");
    let partition_scip = corpus
        .index_dir()
        .join(&cfg.source_corpus_id)
        .join("scip_graph.db");
    let scip_path = if canonical_scip.exists() {
        canonical_scip
    } else {
        partition_scip
    };
    let scip = if scip_path.exists() {
        match ScipGraph::open(&scip_path, &canonical_corpus_id) {
            Ok(g) => Some(g),
            Err(e) => {
                tracing::warn!(error = %e, "code_walk: SCIP open failed, continuing without cross-refs");
                None
            }
        }
    } else {
        tracing::warn!(
            scip_path = %scip_path.display(),
            "code_walk: no SCIP graph for corpus, skipping cross-ref edges"
        );
        None
    };

    // Phase 3.5: bulk-attach SCIP qualified_name to every item.
    // The chunk metadata can't carry this — chunks come from
    // tree-sitter, qualified_names come from rust-analyzer's SCIP
    // export. We resolve them by file_path + bare name lookup.
    if let Some(graph) = scip.as_ref() {
        let scip_attached = attach_qualified_names(graph, &mut groups).await?;
        tracing::info!(
            scip_items = scip_attached,
            total_items = groups.by_item.len(),
            "code_walk: SCIP qualified_name attached"
        );
    }

    // Phase 4: collect external-crate placeholder names. With
    // qualified_name attached, we can mine externals out of SCIP
    // callees by parsing the SCIP descriptor's crate field —
    // unambiguous because rust-analyzer stamps each symbol with its
    // originating crate. Fallback to declared Cargo deps if SCIP
    // unavailable.
    let referenced_externals = if scip.is_some() {
        let mut set: BTreeSet<String> = BTreeSet::new();
        if let Some(graph) = scip.as_ref() {
            collect_externals_from_scip(graph, &workspace, &groups, &mut set).await?;
        }
        // Always union declared deps so workspace-known deps still
        // appear even if they're never invoked (e.g. build-deps).
        for krate in workspace.crates.values() {
            for dep in &krate.dependencies {
                if !workspace.crates.contains_key(dep) {
                    set.insert(dep.clone());
                }
            }
        }
        set
    } else {
        collect_referenced_externals(&workspace)
    };

    // Phase 5: emit entities. Load the code-intel summary cache
    // (best-effort) so item descriptions prefer the plain-English summary
    // over rustdoc (Inc 2); thread the canonical corpus id so every atom
    // id is content-hashed and stable across rebuilds (Inc 1).
    let code_intel = load_code_intel_summaries(corpus.index_dir(), &canonical_corpus_id);
    tracing::info!(
        summaries = code_intel.len(),
        "code_walk: code-intel summaries loaded"
    );
    let (entities, atom_index) = emit_entities(
        &workspace,
        &groups,
        &referenced_externals,
        &canonical_corpus_id,
        &code_intel,
    );
    tracing::info!(
        total_entities = entities.len(),
        "code_walk: entities emitted"
    );

    Ok(PreparedCodeAtlas {
        workspace,
        groups,
        atom_index,
        entities,
        scip,
    })
}

/// Run the code-corpus structural pass end-to-end.
pub async fn extract_code_corpus(
    corpus: Arc<CorpusEngine>,
    cfg: &CodeWalkConfig,
    progress: Arc<ProgressCallback>,
) -> Result<AtlasData> {
    // Phases 1-5: the shared deterministic derivation.
    let PreparedCodeAtlas {
        workspace,
        groups,
        atom_index,
        entities,
        scip,
    } = prepare_code_atlas(&corpus, cfg).await?;
    (progress)(IngestProgress::Extracting {
        documents_processed: groups.chunks_with_metadata as u64,
    });

    // Phase 6: emit edges.
    let (edges, edge_stats) = emit_edges(&workspace, &groups, &atom_index, scip.as_ref()).await?;
    tracing::info!(
        containment = edge_stats.containment,
        scip = edge_stats.scip,
        cargo = edge_stats.cargo,
        total = edges.len(),
        "code_walk: edges emitted"
    );

    // Phase 7: compute per-tier salience.
    let entities = compute_salience(entities, &edges);

    (progress)(IngestProgress::Complete {
        total_chunks: groups.chunks_with_metadata as u64,
        duration_secs: 0,
    });

    // ── Compose AtlasData ───────────────────────────────────
    let atom_envelopes: Vec<AtomEnvelope> =
        entities.into_iter().map(AtomEnvelope::Entity).collect();
    let atoms_file = AtomsFile::new(atom_envelopes);
    let edges_file = EdgesFile::new(edges);

    // Record the source-corpus linkage + the visibility config in
    // schema_validation so the Inc-6 `enrich atlas-patch-code` verb can
    // resolve the source code corpus from a freshly-built atlas and
    // reproduce the same atom universe (without it the verb falls back to
    // the `<source>-self-atlas` naming convention). Forward-looking: atlases
    // built before this field fall back to the convention.
    let schema_validation = serde_json::json!({
        "strategy": "structure_first",
        "branch": "code",
        "source_corpus_id": cfg.source_corpus_id,
        "include_functions": cfg.include_functions,
        "include_private": cfg.include_private,
        "stats": {
            "crates": atom_index.crates.len(),
            "modules": atom_index.modules.len(),
            "items": atom_index.items.len(),
            "external_crates": atom_index.externals.len(),
            "containment_edges": edge_stats.containment,
            "scip_edges": edge_stats.scip,
            "cargo_edges": edge_stats.cargo,
            "chunks_with_metadata": groups.chunks_with_metadata,
            "chunks_without_metadata": groups.chunks_without_metadata,
            "chunks_outside_workspace": groups.chunks_outside_workspace,
        },
    });

    Ok(AtlasData {
        atoms: serde_json::to_value(&atoms_file)
            .map_err(|e| Error::Serialization(format!("atoms serialise: {e}")))?,
        edges: serde_json::to_value(&edges_file)
            .map_err(|e| Error::Serialization(format!("edges serialise: {e}")))?,
        trajectories: serde_json::json!({}),
        manifest: serde_json::json!({}),
        schema_validation,
        dominant_depth: EnrichmentDepth::Structural,
    })
}

// ── Inc 6: first-class patchability ──────────────────────────
//
// `extract_atoms_for_symbols` is the code analogue of
// `structure_first::extract_atoms_for_articles` (the wiki per-doc delta
// template). Given the set of changed + disappeared qualified-names (the
// change-set computed by diffing the source corpus's prior vs refreshed
// `code_intel_cache.json`), it re-derives ONLY those symbols' atoms + edges
// and returns an `AtomsDelta` ready for `apply_atom_delta`.
//
// The win is incremental: it runs the cheap, deterministic phases 1-5
// (chunk metadata aggregation + SCIP qualified_name attach + content-hash
// entity emission — no LLM) so the patched atom's id, kind, description, and
// edge targets are byte-identical to a full rebuild, but it scopes the
// expensive per-symbol SCIP callee queries to the CHANGED symbols only
// (a full build queries every one of ~22k functions). The atom id is stable
// across body edits (qualified_name + kind + corpus_id don't change when a
// body changes), so `apply_atom_delta` replaces the changed symbol's atom
// in place rather than orphaning it.
//
// Re-emitted per changed item: its containment `Module → Item` edge + its
// outgoing `ScipStructural` callee edges (bounded fanout, same resolution as
// `emit_edges`). Both are dropped by `apply_atom_delta` when the item's atom
// is dropped for the upsert, so they must be re-added to keep the item's
// downward neighborhood intact.
//
// KNOWN LIMITS (see report): (1) salience is a global in-degree statistic and
// is NOT recomputed on a patch — the patched atom keeps the 0.5 default.
// (2) INCOMING `ScipStructural` edges (unchanged-caller → changed-symbol) are
// dropped with the upsert and not re-added — "what calls X" can under-report
// X's callers until the next full rebuild. The outgoing direction (the
// primary "how does this work" lens) stays fresh.

/// Re-derive atoms + edges for a scoped set of changed/removed symbols and
/// return an [`AtomsDelta`]. `changed_qnames` / `removed_qnames` are SCIP
/// qualified-names == the item atoms' stable doc-anchors == `apply_atom_delta`
/// doc-ids. Symbols not present in the live atlas's item universe (e.g.
/// excluded by `include_functions`) are skipped. Empty change-set → empty
/// (no-op) delta.
pub async fn extract_atoms_for_symbols(
    corpus: Arc<CorpusEngine>,
    cfg: &CodeWalkConfig,
    changed_qnames: &BTreeSet<String>,
    removed_qnames: &BTreeSet<String>,
) -> Result<AtomsDelta> {
    if changed_qnames.is_empty() && removed_qnames.is_empty() {
        return Ok(AtomsDelta::default());
    }

    let PreparedCodeAtlas {
        workspace,
        groups,
        atom_index,
        entities,
        scip,
    } = prepare_code_atlas(&corpus, cfg).await?;

    // Entity by stable doc-anchor (== qualified_name for SCIP-resolved
    // items). Crate/module/external doc-anchors are their canonical names,
    // which never collide with a SCIP descriptor, so looking a changed
    // qualified-name up here only ever resolves to an item entity.
    let mut entity_by_anchor: HashMap<&str, &Entity> = HashMap::with_capacity(entities.len());
    for e in &entities {
        entity_by_anchor.insert(e.first_appearance.chunk_id.as_str(), e);
    }

    // qualified_name → ItemKey, for resolving the changed symbol's module
    // (containment) + driving its SCIP callee query.
    let mut qname_to_key: HashMap<&str, &ItemKey> = HashMap::new();
    for (key, item) in &groups.by_item {
        if let Some(q) = &item.qualified_name {
            qname_to_key.insert(q.as_str(), key);
        }
    }

    let qualified_to_atom = build_qualified_to_atom(&atom_index, &groups);

    let mut upserted_docs: Vec<(String, Vec<AtomEnvelope>)> = Vec::new();
    let mut added_edges: Vec<Edge> = Vec::new();
    let mut edge_seen: BTreeSet<EdgeId> = BTreeSet::new();
    let mut matched = 0usize;
    let mut unmatched = 0usize;

    for qname in changed_qnames {
        let Some(ent) = entity_by_anchor.get(qname.as_str()) else {
            // The changed symbol isn't an atom in this atlas (e.g. a private
            // fn with include_private off, or a test fn). Nothing to upsert.
            unmatched += 1;
            continue;
        };
        matched += 1;
        upserted_docs.push((qname.clone(), vec![AtomEnvelope::Entity((*ent).clone())]));

        let Some(key) = qname_to_key.get(qname.as_str()).copied() else {
            continue;
        };
        let Some(item_atom) = atom_index.items.get(key) else {
            continue;
        };

        // Containment Module → Item (dropped with the item on upsert; re-add).
        if let Some(module_atom) = atom_index
            .modules
            .get(&(key.crate_name.clone(), key.module_path.clone()))
        {
            let id = stable_edge_id("cont", module_atom, item_atom);
            if edge_seen.insert(id.clone()) {
                added_edges.push(Edge {
                    id,
                    edge_type: EdgeType::Involves,
                    source: module_atom.clone(),
                    target: item_atom.clone(),
                    evidence: Vec::new(),
                    trigger_event: None,
                    sub_question: None,
                    confidence: 1.0,
                    provenance: EdgeProvenance::ContainmentStructural,
                });
            }
        }

        // Outgoing ScipStructural callee edges — same resolution + fanout
        // bound + function→external guard as `emit_edges`, scoped to this
        // one symbol.
        let source_is_function = key.symbol_kind == "function" || key.symbol_kind == "method";
        if let Some(graph) = scip.as_ref() {
            let callees = match graph.find_callees_qualified(qname).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(qname = %qname, error = %e, "atlas_patch_code: find_callees_qualified failed; skipping outgoing edges");
                    Vec::new()
                }
            };
            let mut emitted = 0usize;
            for callee in callees {
                if emitted >= MAX_CALLEE_FANOUT {
                    break;
                }
                if callee.callee_qualified.is_empty() {
                    continue;
                }
                let Some((target_atom, target_is_external)) = resolve_callee_target(
                    &qualified_to_atom,
                    &atom_index,
                    &workspace,
                    &callee.callee_qualified,
                ) else {
                    continue;
                };
                if source_is_function && target_is_external {
                    continue;
                }
                let id = stable_edge_id("scip", item_atom, &target_atom);
                if !edge_seen.insert(id.clone()) {
                    continue;
                }
                added_edges.push(Edge {
                    id,
                    edge_type: EdgeType::Involves,
                    source: item_atom.clone(),
                    target: target_atom,
                    evidence: Vec::new(),
                    trigger_event: None,
                    sub_question: None,
                    confidence: 1.0,
                    provenance: EdgeProvenance::ScipStructural,
                });
                emitted += 1;
            }
        }
    }

    tracing::info!(
        changed = changed_qnames.len(),
        removed = removed_qnames.len(),
        matched_atoms = matched,
        unmatched_symbols = unmatched,
        upserted_docs = upserted_docs.len(),
        added_edges = added_edges.len(),
        "atlas_patch_code: scoped delta built"
    );

    Ok(AtomsDelta {
        added: Vec::new(),
        removed_doc_ids: removed_qnames.iter().cloned().collect(),
        upserted_docs,
        added_edges,
    })
}

/// Inc 3 fanout bound, shared by the full-build [`emit_edges`] and the Inc-6
/// scoped [`extract_atoms_for_symbols`]: cap emitted callee edges per source
/// symbol so a hot function referencing stdlib + helpers dozens of times
/// can't explode the graph. `find_callees_qualified` returns rows ORDER BY
/// (file_path, line), so "the first N" is stable across rebuilds.
const MAX_CALLEE_FANOUT: usize = 12;

/// Recover the `(include_functions, include_private)` config the atlas was
/// built with, so an Inc-6 patch reproduces the SAME item universe (a patch
/// that aggregated with `include_functions = true` against an atlas built
/// without functions would wrongly inject brand-new function atoms). Reads
/// the fields [`extract_code_corpus`] records in `schema_validation.json`;
/// for a pre-Inc-6 atlas that lacks them, INFERS `include_functions` from the
/// presence of function/method item atoms (private items are
/// indistinguishable in the atom shape, so it assumes the build default,
/// `false`). The verb's `--include-functions` / `--include-private` flags
/// override this.
pub fn read_code_walk_visibility(atlas_dir: &Path) -> (bool, bool) {
    if let Ok(s) = std::fs::read_to_string(atlas_dir.join("schema_validation.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
            let inc_fn = v.get("include_functions").and_then(|b| b.as_bool());
            let inc_priv = v.get("include_private").and_then(|b| b.as_bool());
            if let (Some(f), Some(p)) = (inc_fn, inc_priv) {
                return (f, p);
            }
        }
    }
    let has_functions = crate::enrichment::atlas::read_atlas_atoms(atlas_dir)
        .map(|af| {
            af.atoms.iter().any(|a| {
                matches!(a, AtomEnvelope::Entity(e)
                    if matches!(&e.entity_type, EntityType::Other(k) if k == "function" || k == "method"))
            })
        })
        .unwrap_or(true);
    (has_functions, false)
}

/// Read the original source tree root from a code corpus's
/// `_corpus_meta.json` `source_path`. Shared with the watcher's code-atlas
/// delta (which re-enumerates current symbol bodies to detect changes).
pub fn read_corpus_source_root(corpus_dir: &Path) -> Result<PathBuf> {
    let raw = std::fs::read_to_string(corpus_dir.join("_corpus_meta.json"))
        .map_err(|e| Error::Database(format!("read _corpus_meta.json: {e}")))?;
    let v: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| Error::Serialization(format!("parse _corpus_meta.json: {e}")))?;
    v.get("source_path")
        .and_then(|s| s.as_str())
        .map(PathBuf::from)
        .ok_or_else(|| {
            Error::InvalidInput("corpus has no `source_path` in _corpus_meta.json".into())
        })
}

/// Build the set of external-crate placeholder names. The SCIP
/// exporter in this codebase stores bare symbol names — we can't
/// recover the originating crate from a name like `Vec` or
/// `HashMap`. So instead of trying to mine externals out of SCIP
/// callees, we read declared dependencies from each Cargo.toml. This
/// matches the spec's intent (placeholders for "what this codebase
/// depends on, structurally") and avoids polluting the atlas with
/// stdlib type names.
///
/// `_scip` is accepted for symmetry with the previous signature; it's
/// no longer needed but retained to keep the call site stable. A
/// future SCIP version that records fully-qualified names could read
/// it; today we ignore it.
fn collect_referenced_externals(workspace: &WorkspaceMap) -> BTreeSet<String> {
    let mut externals: BTreeSet<String> = BTreeSet::new();
    for krate in workspace.crates.values() {
        for dep in &krate.dependencies {
            if !workspace.crates.contains_key(dep) {
                externals.insert(dep.clone());
            }
        }
    }
    externals
}

/// Read `(source_path, canonical_corpus_id)` from the source
/// corpus's `_corpus_meta.json`. The canonical id is the value of
/// the `corpus_id` field — distinct from the directory name when
/// the index is partitioned (e.g., directory `corpus-engine-partition-local`
/// carries `corpus_id = "corpus-engine"`).
fn read_source_metadata(index_dir: &Path, corpus_id: &str) -> Result<(PathBuf, String)> {
    let path = index_dir.join(corpus_id).join("_corpus_meta.json");
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        Error::Database(format!(
            "code_walk: read _corpus_meta.json at {}: {e}",
            path.display()
        ))
    })?;
    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        Error::Serialization(format!(
            "code_walk: parse _corpus_meta.json at {}: {e}",
            path.display()
        ))
    })?;
    let source = v
        .get("source_path")
        .and_then(|s| s.as_str())
        .ok_or_else(|| {
            Error::InvalidInput(format!(
                "code_walk: corpus `{corpus_id}` has no `source_path` in _corpus_meta.json"
            ))
        })?;
    let canonical = v
        .get("corpus_id")
        .and_then(|s| s.as_str())
        .unwrap_or(corpus_id) // graceful fallback for legacy meta without the field
        .to_string();
    Ok((PathBuf::from(source), canonical))
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_path_for_handles_common_layouts() {
        let crate_root = Path::new("");
        assert_eq!(
            module_path_for(Path::new("src/lib.rs"), crate_root),
            Some((String::new(), true))
        );
        assert_eq!(
            module_path_for(Path::new("src/main.rs"), crate_root),
            Some((String::new(), true))
        );
        assert_eq!(
            module_path_for(Path::new("src/engine/mod.rs"), crate_root),
            Some(("engine".to_string(), true))
        );
        assert_eq!(
            module_path_for(Path::new("src/engine/ingest.rs"), crate_root),
            Some(("engine::ingest".to_string(), false))
        );
        assert_eq!(
            module_path_for(Path::new("src/extractors/code/mod.rs"), crate_root),
            Some(("extractors::code".to_string(), true))
        );
    }

    #[test]
    fn module_path_for_strips_crate_prefix() {
        let crate_root = Path::new("crates/sovereign-core");
        assert_eq!(
            module_path_for(
                Path::new("crates/sovereign-core/src/runtime.rs"),
                crate_root,
            ),
            Some(("runtime".to_string(), false))
        );
    }

    #[test]
    fn metadata_looks_like_code_recognises_code_chunks() {
        let code = r#"{"symbol_name":"foo","file_path":"src/lib.rs","language":"rust"}"#;
        assert!(metadata_looks_like_code(code));
        let wiki = r#"{"section_path":["Lead"],"section_type":"lead","outgoing_links":[]}"#;
        assert!(!metadata_looks_like_code(wiki));
        let bogus = r#"not even json"#;
        assert!(!metadata_looks_like_code(bogus));
    }

    #[test]
    fn first_doc_paragraph_caps_and_picks_first_sentence() {
        let doc = "First sentence here. Second sentence.";
        assert_eq!(first_doc_paragraph(doc, 280), "First sentence here.");
        let multi = "Para one.\n\nPara two starts here.";
        assert_eq!(first_doc_paragraph(multi, 280), "Para one.");
    }

    #[test]
    fn read_inner_rustdoc_picks_up_module_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.rs");
        std::fs::write(
            &path,
            "//! First line of crate docs.\n//! Second line.\n\npub fn foo() {}\n",
        )
        .unwrap();
        let docs = read_inner_rustdoc(&path);
        assert_eq!(
            docs.as_deref(),
            Some("First line of crate docs.\nSecond line.")
        );
    }
}
