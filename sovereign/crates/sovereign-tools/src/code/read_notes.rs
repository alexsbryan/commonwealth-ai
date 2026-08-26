// SPDX-License-Identifier: AGPL-3.0-or-later
//! `read_notes` — retrieve agent working notes.
//!
//! Supports full-text search (BM25), symbol/file filtering, and kind
//! filtering. Without a `query`, results are ordered by recency (newest
//! first). With a `query`, results are ordered by BM25 relevance.

use std::sync::Arc;

use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use corpus_engine_notes::{NoteScope, NoteStore, ScopeFilter};
use sovereign_core::tool_manifest::DeclaredTool;

/// Anchors whose notes are OPERATIONAL RECORD rather than knowledge.
///
/// The comaintainer seat keeps its stewardship log as global `decision`
/// notes anchored to `comaintainer-seat`: pool state, spawn bookkeeping,
/// verdict resolutions. Order seat-durable-rail added `order-seat`
/// (co-order.sh write-through) and `directive-log` (co-directive-log.sh
/// write-through). Those are the right places for them — they exist to
/// be AUDITED, with `related_to: "<anchor>"`. What they are not is
/// knowledge about this codebase, so arriving unasked in someone
/// else's topical search, or in the UserPromptSubmit index (which asks
/// for recent global decisions and nothing else), spends every session's
/// budget on another session's bookkeeping. Measured in the seat's own
/// evaluation, note e10b02a8; backlog 371e3d5f.
///
/// An OPEN set — a registry, not a match arm (ARCH §4). The registry is
/// `quality/operational-anchors.toml`; this const is the compiled-in
/// FLOOR it degrades to when the file is missing, unreadable, or empty —
/// never to nothing, because un-hiding the seat log into every session
/// is the one failure UC-D4 must not have. The mirror test
/// `the_registry_file_mirrors_the_compiled_in_floor` keeps the two equal.
/// Three ways to ask for these notes remain, and none is touched:
///   * `related_to: "<anchor>"` — a different code path entirely;
///   * naming the anchor in `query`, which turns the hiding off;
///   * `include_operational: true` — the seat path (UC-D4 inverse).
/// Whenever rows ARE hidden the response says so and names the anchor
/// (ARCH §18.3 — absence is reported, never defaulted).
const DEFAULT_OPERATIONAL_ANCHORS: &[&str] = &["comaintainer-seat", "order-seat", "directive-log"];

/// The registry file, relative to the workspace root — beside
/// env-flags.toml and backlog-ruler.toml in `quality/` (ARCH §6:
/// a list that grows is versioned data, not a const in a .rs).
const OPERATIONAL_ANCHORS_TOML: &str = "quality/operational-anchors.toml";

/// `[[anchor]] name = "..."` rows from the registry file.
#[derive(serde::Deserialize)]
struct OperationalAnchorsFile {
    anchor: Vec<OperationalAnchorRow>,
}

#[derive(serde::Deserialize)]
struct OperationalAnchorRow {
    name: String,
}

/// The compiled-in floor as owned strings — the shape the filter works on.
fn floor_anchors() -> Vec<String> {
    DEFAULT_OPERATIONAL_ANCHORS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Resolve the workspace root for the registry, mirroring the daemon's
/// chain (daemon_cmd/workspace.rs): explicit root (the builder) →
/// SOVEREIGN_WORKSPACE_DIR → `~/.svrnmesh/workspace` → ascent from the
/// cwd for the repo signature. `None` only when every source fails,
/// in which case the floor still holds.
fn resolve_workspace_root(explicit: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    // The two CONFIGURED sources, resolved the daemon's way because there is
    // now only one way (`sovereign_contracts::workspace`). This block used to
    // re-derive them under a comment claiming it mirrored the daemon: it
    // accepted an untrimmed `" "` as a workspace, returned paths that do not
    // exist, and read `$HOME/.svrnmesh/workspace` directly — so on a host with
    // a relocated root it read a DIFFERENT pin file than the daemon wrote.
    if let Some(p) = sovereign_contracts::workspace::configured_workspace_dir() {
        return Some(p);
    }
    if let Ok(cwd) = std::env::current_dir() {
        for dir in cwd.ancestors() {
            // The same repo signature the daemon's auto-detect uses.
            if dir.join("scripts/sovereign-lint.sh").is_file() && dir.join("Cargo.toml").is_file() {
                return Some(dir.to_path_buf());
            }
        }
    }
    None
}

/// The anchors this call is not asking for. Empty when the query names
/// one (that is how a caller opts back in) or when `include_operational`
/// is set (the seat path — the whole hiding is skipped).
fn anchors_to_hide(
    anchors: &[String],
    query: Option<&str>,
    include_operational: bool,
) -> Vec<String> {
    if include_operational {
        return Vec::new();
    }
    let q = query.unwrap_or("").to_ascii_lowercase();
    anchors
        .iter()
        .filter(|anchor| !q.contains(&anchor.to_ascii_lowercase()))
        .cloned()
        .collect()
}

/// Is this row anchored to one of the anchors being hidden?
fn is_hidden_anchor(related_entity: Option<&str>, hidden: &[String]) -> bool {
    match related_entity {
        Some(e) => hidden.iter().any(|a| e.eq_ignore_ascii_case(a)),
        None => false,
    }
}

pub struct ReadNotesTool {
    store: Arc<NoteStore>,
    workspace_root: Option<std::path::PathBuf>,
}

impl ReadNotesTool {
    pub fn new(store: Arc<NoteStore>) -> Self {
        Self {
            store,
            workspace_root: None,
        }
    }

    /// Where `quality/operational-anchors.toml` lives. Mirrors the
    /// BriefingTool/SessionStateTool builder in daemon_cmd/tool_registry.rs.
    /// Without it the loader falls back to SOVEREIGN_WORKSPACE_DIR, the
    /// `~/.svrnmesh/workspace` file, then an ascent from the cwd, then
    /// the compiled-in floor.
    pub fn with_workspace_root(mut self, root: std::path::PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    /// Load the registry on every call, so appending an anchor takes
    /// effect without a daemon restart. Any failure — missing file,
    /// unreadable, unparseable, or an empty registry — degrades to the
    /// compiled-in floor and is named in a tracing event, never silent
    /// and never zero (ARCH §18.3, UC-D4).
    fn operational_anchors(&self) -> Vec<String> {
        let Some(root) = resolve_workspace_root(self.workspace_root.as_deref()) else {
            tracing::debug!(
                target = "notes",
                "notes: no workspace root resolved; operational anchors = compiled-in floor"
            );
            return floor_anchors();
        };
        let path = root.join(OPERATIONAL_ANCHORS_TOML);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                tracing::debug!(
                    target = "notes",
                    path = %path.display(),
                    error = %e,
                    "notes: operational-anchors registry unreadable; using compiled-in floor"
                );
                return floor_anchors();
            }
        };
        match toml::from_str::<OperationalAnchorsFile>(&text) {
            Ok(file) if !file.anchor.is_empty() => {
                file.anchor.into_iter().map(|a| a.name).collect()
            }
            Ok(_) => {
                // Empty is treated as a broken file, not a decision:
                // honoring it would silently un-hide the seat log into
                // every session — the UC-D4 failure. Deliberately
                // emptying the registry goes through the mirror test.
                tracing::warn!(
                    target = "notes",
                    path = %path.display(),
                    "notes: operational-anchors registry has zero anchors; using compiled-in floor"
                );
                floor_anchors()
            }
            Err(e) => {
                tracing::warn!(
                    target = "notes",
                    path = %path.display(),
                    error = %e,
                    "notes: operational-anchors registry unparseable; using compiled-in floor"
                );
                floor_anchors()
            }
        }
    }
}

impl ReadNotesTool {
    /// Bind this tool's state to its `notes` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("notes", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_signal({
            let state = Arc::clone(&state);
            Arc::new(move || {
                let state = Arc::clone(&state);
                Box::pin(async move { state.signal_now().await })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>>
            })
        })
    }

    /// The executable half of `notes`.
    async fn run(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let query = params.get("query").and_then(|v| v.as_str());
        let symbols: Vec<String> = params
            .get("symbols")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let files: Vec<String> = params
            .get("files")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let kinds: Vec<String> = params
            .get("kinds")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(10);

        let scopes: Vec<NoteScope> = params
            .get("scope")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().and_then(NoteScope::parse))
                    .collect()
            })
            .unwrap_or_default();
        let feature_id = params
            .get("feature_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        // T2 path: when `related_to` is set, surface notes related
        // to that symbol/file/entity via the entity-graph
        // co-occurrence ranking. Other filters (kind, scope, file)
        // don't apply — the path is "find notes connected to X",
        // not "find notes matching X under filters".
        if let Some(seed) = params
            .get("related_to")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            let related = self
                .store
                .read_notes_related(seed, limit)
                .await
                .map_err(|e| Error::Tool {
                    tool_id: "notes".to_string(),
                    message: e.to_string(),
                })?;
            let total = related.len();
            let note_values: Vec<serde_json::Value> = related
                .into_iter()
                .map(|n| {
                    json!({
                        "id": n.id,
                        "kind": n.kind,
                        "content": n.content,
                        "symbols": n.symbols,
                        "files": n.files,
                        "session_id": n.session_id,
                        "created_at": n.created_at,
                        "scope": n.scope,
                        "feature_id": n.feature_id,
                        "related_entity": n.related_entity,
                        // Which machine wrote this. A note can be about the
                        // CODE (applies everywhere) or about the BOX it was
                        // written on ("holding the daemon", "GPU busy") — the
                        // reader cannot tell those apart without the author.
                        "author": self.store.attribution(n.origin_node_id.as_deref()).label(),
                        "author_relation": self.store.attribution(n.origin_node_id.as_deref()).as_str(),
                        // Two-sided delivery receipts (order commons-fluency
                        // fix 3): `sent_at` is the ORIGIN's publish clock
                        // (null = never published), `received_at` is THIS
                        // node's apply clock (null = authored here, never
                        // received). On a peer's row both are set and
                        // bracket sent_at <= received_at <= now.
                        "sent_at": n.sent_at,
                        "received_at": n.received_at,
                    })
                })
                .collect();
            return Ok(StepOutput::Json(json!({
                "notes": note_values,
                "total": total,
                "path": "related",
                "seed": seed,
            })));
        }

        // T1 path: when query is set + semantic on (default), use
        // `read_notes_scoped_semantic`. It auto-falls-back to
        // FTS5-only when embed_fn isn't wired (so callers don't
        // have to know whether T1 is live) and is byte-identical
        // to the baseline when `SOVEREIGN_NOTES_EMBED_WEIGHT=0`.
        let include_operational = params
            .get("include_operational")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let semantic_enabled = params
            .get("semantic")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let semantic_query = if semantic_enabled && query.map(|q| !q.is_empty()).unwrap_or(false) {
            query
        } else {
            None
        };
        let filter = ScopeFilter { scopes, feature_id };
        // Over-fetch when an operational anchor is being hidden, so the
        // exclusion narrows the SET and not the WINDOW: "the 20 most
        // recent global decisions" must still return 20 when the six
        // newest are seat bookkeeping. The registry is loaded per call,
        // so appending an anchor takes effect without a restart.
        let hidden = anchors_to_hide(&self.operational_anchors(), query, include_operational);
        let fetch_limit = if hidden.is_empty() {
            limit
        } else {
            limit.saturating_mul(3).min(200)
        };
        let outcome = self
            .store
            .read_notes_scoped_semantic_outcome(
                query,
                &symbols,
                &files,
                &kinds,
                fetch_limit,
                false,
                &filter,
                semantic_query,
            )
            .await
            .map_err(|e| Error::Tool {
                tool_id: "notes".to_string(),
                message: e.to_string(),
            })?;
        // The store collapsed identical-content duplicates (same
        // kind/content/related_entity) to one representative row each,
        // before its truncate. The count is named, never silent (§18.3).
        let collapsed_duplicates = outcome.collapsed;
        let notes = outcome.rows;

        let mut excluded = 0usize;
        let notes: Vec<_> = notes
            .into_iter()
            .filter(|n| {
                let hide = is_hidden_anchor(n.related_entity.as_deref(), &hidden);
                if hide {
                    excluded += 1;
                }
                !hide
            })
            .take(limit)
            .collect();
        if excluded > 0 {
            tracing::debug!(
                target = "notes",
                excluded,
                anchors = ?hidden,
                "notes: operational-anchor rows withheld from this read"
            );
        }

        let total = notes.len();
        let note_values: Vec<serde_json::Value> = notes
            .into_iter()
            .map(|n| {
                json!({
                    "id": n.id,
                    "kind": n.kind,
                    "content": n.content,
                    "symbols": n.symbols,
                    "files": n.files,
                    "session_id": n.session_id,
                    "created_at": n.created_at,
                    "scope": n.scope,
                    "feature_id": n.feature_id,
                    // The anchor is part of the row's identity: a reader
                    // that cannot see it cannot tell a note ABOUT the code
                    // from a note about somebody's operational record.
                    "related_entity": n.related_entity,
                    // Two-sided delivery receipts (order commons-fluency
                    // fix 3): origin publish clock vs. this node's apply
                    // clock. Nulls are first-class answers — a seat note
                    // written on this node has sent_at set and
                    // received_at null; one that arrived from a peer has
                    // both, with sent_at <= received_at.
                    "sent_at": n.sent_at,
                    "received_at": n.received_at,
                    // See the `related_to` path above — same reasoning.
                    "author": self.store.attribution(n.origin_node_id.as_deref()).label(),
                    "author_relation": self.store.attribution(n.origin_node_id.as_deref()).as_str(),
                })
            })
            .collect();

        let mut out = json!({
            "notes": note_values,
            "total": total
        });
        if collapsed_duplicates > 0 {
            // Named, never silent (ARCH §18.3): the caller is told that
            // identical-content rows were collapsed to one representative.
            out["collapsed_duplicates"] = json!(collapsed_duplicates);
        }
        if excluded > 0 {
            // Named, never silent (ARCH §18.3): the caller is told what
            // was withheld and how to ask for it.
            out["withheld_operational"] = json!(excluded);
            out["withheld_anchors"] = json!(hidden);
            out["withheld_hint"] = json!(format!(
                "{excluded} operational-record note(s) anchored to {} were not \
                 returned. Ask for them with related_to:\"{}\", or name the \
                 anchor in your query.",
                hidden.join("/"),
                hidden.first().map(String::as_str).unwrap_or("")
            ));
        }
        Ok(StepOutput::Json(out))
    }

    async fn signal_now(&self) -> Option<String> {

        // Cap the query at 50 — we only need the count for the signal
        // and an order-of-magnitude is enough context. If there are
        // 50+ open todos the agent already knows there's a backlog.
        let open = self.store.open_todos(50).await.ok()?;
        if open.is_empty() {
            return None;
        }
        let n = open.len();
        let suffix = if n >= 50 { "+" } else { "" };
        Some(format!("{n}{suffix} open todo note(s)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine_notes::NoteSource;
    use serde_json::json;
    use sovereign_core::types::ToolContext;
    use std::path::Path;

    fn floor() -> Vec<String> {
        floor_anchors()
    }

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: "read-notes-test".into(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
            question: None,
        }
    }

    /// The failing input this exclusion exists for (ARCH §18.1): a
    /// topical query that names nothing about the seat, against a store
    /// whose newest global decisions are seat bookkeeping.
    #[test]
    fn topical_query_hides_the_operational_logs() {
        let hidden = anchors_to_hide(&floor(), Some("native grounding"), false);
        assert_eq!(
            hidden,
            vec![
                "comaintainer-seat".to_string(),
                "order-seat".to_string(),
                "directive-log".to_string(),
            ]
        );
        assert!(is_hidden_anchor(Some("comaintainer-seat"), &hidden));
        assert!(is_hidden_anchor(Some("order-seat"), &hidden));
        assert!(!is_hidden_anchor(Some("Sarah Chen"), &hidden));
        assert!(!is_hidden_anchor(None, &hidden));
    }

    #[test]
    fn a_query_naming_one_anchor_opts_that_anchor_back_in() {
        // A query that names an anchor is an explicit ask for THAT rail
        // (UC-D4 opt-in, per anchor): the named rail comes back, and the
        // guard stays tight for the others — asking about directives
        // must not spill the seat's own log.
        let hidden = anchors_to_hide(&floor(), Some("order-seat"), false);
        assert!(!hidden.iter().any(|a| a == "order-seat"));
        assert!(hidden.iter().any(|a| a == "comaintainer-seat"));
        assert!(hidden.iter().any(|a| a == "directive-log"));
        // Case-insensitive, mid-sentence.
        let hidden = anchors_to_hide(&floor(), Some("what did the ORDER-SEAT open"), false);
        assert!(!hidden.iter().any(|a| a == "order-seat"));
        assert!(hidden.iter().any(|a| a == "directive-log"));
        // A query naming a DIFFERENT anchor leaves this one hidden.
        let hidden = anchors_to_hide(&floor(), Some("directive-log stats"), false);
        assert!(!hidden.iter().any(|a| a == "directive-log"));
        assert!(hidden.iter().any(|a| a == "order-seat"));
    }

    /// No query at all is the UserPromptSubmit index's shape — recent
    /// global decisions, nothing else. That is precisely the read the
    /// seat log was flooding, so it must hide.
    #[test]
    fn the_unqueried_recency_index_hides() {
        assert_eq!(anchors_to_hide(&floor(), None, false).len(), 3);
        assert_eq!(anchors_to_hide(&floor(), Some(""), false).len(), 3);
    }

    /// Anchor matching is exact-but-case-insensitive, never a substring:
    /// a note anchored to "comaintainer-seat-notes" is a different log
    /// and stays visible until someone adds it to the registry.
    #[test]
    fn anchor_match_is_whole_value_not_substring() {
        let hidden = anchors_to_hide(&floor(), Some("anything"), false);
        assert!(is_hidden_anchor(Some("Comaintainer-Seat"), &hidden));
        assert!(!is_hidden_anchor(Some("comaintainer-seat-notes"), &hidden));
    }

    /// The seat's ambient read (UC-D4 inverse): include_operational
    /// turns the hiding off entirely, so the seat session carries its
    /// own coordination rail into context.
    #[test]
    fn include_operational_is_the_seat_s_opt_in() {
        let hidden = anchors_to_hide(&floor(), None, true);
        assert!(hidden.is_empty(), "seat read must not hide anything");
    }

    /// The floor is the invariant: a missing or unreadable registry
    /// must degrade to the compiled-in anchors, never to nothing
    /// (UC-D4 hard gate — zero hiding would leak the seat log into
    /// every session).
    #[test]
    fn a_missing_registry_degrades_to_the_floor_never_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = ReadNotesTool::new(Arc::new(
            corpus_engine_notes::NoteStore::open(&tmp.path().join("notes.db")).unwrap(),
        ))
        .with_workspace_root(tmp.path().to_path_buf()); // no quality/ toml inside
        assert_eq!(tool.operational_anchors(), floor());
    }

    /// A registry row lands in the hide set without a rebuild — the
    /// whole point of the registry over the const.
    #[test]
    fn a_registry_only_anchor_hides_too() {
        let tmp = tempfile::tempdir().unwrap();
        let quality = tmp.path().join("quality");
        std::fs::create_dir_all(&quality).unwrap();
        std::fs::write(
            quality.join("operational-anchors.toml"),
            "[[anchor]]\nname = \"verdict-log\"\n",
        )
        .unwrap();
        let tool = ReadNotesTool::new(Arc::new(
            corpus_engine_notes::NoteStore::open(&tmp.path().join("notes.db")).unwrap(),
        ))
        .with_workspace_root(tmp.path().to_path_buf());
        let anchors = tool.operational_anchors();
        assert!(anchors.iter().any(|a| a == "verdict-log"));
        assert_eq!(anchors.len(), 1, "the registry is the source, not a union");
    }

    /// An empty registry is a broken file, not a decision: it degrades
    /// to the floor (never hides nothing), and the mirror test below
    /// would redden on a deliberately emptied file anyway.
    #[test]
    fn an_empty_registry_degrades_to_the_floor() {
        let tmp = tempfile::tempdir().unwrap();
        let quality = tmp.path().join("quality");
        std::fs::create_dir_all(&quality).unwrap();
        std::fs::write(quality.join("operational-anchors.toml"), "").unwrap();
        let tool = ReadNotesTool::new(Arc::new(
            corpus_engine_notes::NoteStore::open(&tmp.path().join("notes.db")).unwrap(),
        ))
        .with_workspace_root(tmp.path().to_path_buf());
        assert_eq!(tool.operational_anchors(), floor());
    }

    /// The one list, both sides (ARCH §10.6): the compiled-in floor
    /// and quality/operational-anchors.toml must name the same
    /// anchors. Adding an anchor means touching BOTH; this test is
    /// the reminder.
    #[test]
    fn the_registry_file_mirrors_the_compiled_in_floor() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .find(|d| d.join("scripts/sovereign-lint.sh").is_file())
            .expect("ascent must find the repo root");
        let text = std::fs::read_to_string(root.join("quality/operational-anchors.toml")).unwrap();
        let file: OperationalAnchorsFile = toml::from_str(&text).unwrap();
        let mut file_names: Vec<&str> = file.anchor.iter().map(|a| a.name.as_str()).collect();
        let mut floor_names: Vec<&str> = DEFAULT_OPERATIONAL_ANCHORS.to_vec();
        file_names.sort_unstable();
        floor_names.sort_unstable();
        assert_eq!(
            file_names, floor_names,
            "quality/operational-anchors.toml and DEFAULT_OPERATIONAL_ANCHORS disagree — append to BOTH"
        );
    }

    /// Identical-content duplicate rows (the harvest-era flood — the
    /// same commit-message note written once per corpus session) are
    /// collapsed to one representative row before the limit applies,
    /// and the response NAMES the collapse count — never silent
    /// (§18.3), mirroring the `withheld_*` reporting pattern.
    #[tokio::test]
    async fn execute_collapses_identical_content_duplicates_and_names_the_count() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(NoteStore::open(&tmp.path().join("notes.db")).unwrap());
        let content = "feat(bench): chaos --naked true-baseline (bare model, no prompts/retrieval)";
        for session in [
            "harvest-commonwealth",
            "harvest-sovereign",
            "harvest-corpus-engine",
        ] {
            store
                .write_note_full_v9(
                    "decision",
                    content,
                    vec![],
                    vec![],
                    session,
                    NoteScope::Global,
                    None,
                    Some("00e33f1bbaaad006e651b58390ebc8584b79f108"),
                    NoteSource::Committed,
                    None,
                    None,
                    false,
                )
                .await
                .unwrap();
        }
        store
            .write_note_full_v9(
                "decision",
                "the real chaos decision: what actually matters",
                vec![],
                vec![],
                "session-a",
                NoteScope::Global,
                None,
                None,
                NoteSource::Agent,
                None,
                None,
                false,
            )
            .await
            .unwrap();

        let tool = ReadNotesTool::new(Arc::clone(&store));
        let out = tool
            .run(&json!({"query": "chaos", "limit": 10}), &ctx())
            .await
            .unwrap();
        let StepOutput::Json(v) = out else {
            panic!("expected Json response, got {out:?}");
        };
        let notes = v["notes"].as_array().expect("notes array");
        assert_eq!(
            notes.len(),
            2,
            "3 dupes + 1 distinct → 2 rows after collapse"
        );
        assert_eq!(v["total"].as_u64(), Some(2));
        assert_eq!(
            v["collapsed_duplicates"].as_u64(),
            Some(2),
            "the collapse count is named, never silent (§18.3)"
        );
        // The representative rows are distinct content.
        let contents: Vec<&str> = notes.iter().filter_map(|n| n["content"].as_str()).collect();
        assert_eq!(contents.len(), 2);
        assert_ne!(contents[0], contents[1]);
    }
}
