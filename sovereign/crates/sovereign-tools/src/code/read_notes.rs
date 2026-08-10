// SPDX-License-Identifier: AGPL-3.0-or-later
//! `read_notes` — retrieve agent working notes.
//!
//! Supports full-text search (BM25), symbol/file filtering, and kind
//! filtering. Without a `query`, results are ordered by recency (newest
//! first). With a `query`, results are ordered by BM25 relevance.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine_notes::{NoteScope, NoteStore, ScopeFilter};

/// Anchors whose notes are OPERATIONAL RECORD rather than knowledge.
///
/// The comaintainer seat keeps its stewardship log as global `decision`
/// notes anchored to `comaintainer-seat`: pool state, spawn bookkeeping,
/// verdict resolutions. That is the right place for them — they exist to
/// be AUDITED, with `related_to: "comaintainer-seat"`. What they are not
/// is knowledge about this codebase, so arriving unasked in someone
/// else's topical search, or in the UserPromptSubmit index (which asks
/// for recent global decisions and nothing else), spends every session's
/// budget on another session's bookkeeping. Measured in the seat's own
/// evaluation, note e10b02a8; backlog 371e3d5f.
///
/// An OPEN set — a list to append to, not a match arm (ARCH §4). Two
/// ways to ask for these notes remain, and neither is touched:
///   * `related_to: "<anchor>"` — a different code path entirely;
///   * naming the anchor in `query`, which turns the hiding off.
/// Whenever rows ARE hidden the response says so and names the anchor
/// (ARCH §18.3 — absence is reported, never defaulted).
const OPERATIONAL_ANCHORS: &[&str] = &["comaintainer-seat"];

/// The anchors this call is not asking for. Empty when the query names
/// one, which is how a caller opts back in.
fn anchors_to_hide(query: Option<&str>) -> Vec<&'static str> {
    let q = query.unwrap_or("").to_ascii_lowercase();
    OPERATIONAL_ANCHORS
        .iter()
        .copied()
        .filter(|anchor| !q.contains(&anchor.to_ascii_lowercase()))
        .collect()
}

/// Is this row anchored to one of the anchors being hidden?
fn is_hidden_anchor(related_entity: Option<&str>, hidden: &[&str]) -> bool {
    match related_entity {
        Some(e) => hidden.iter().any(|a| e.eq_ignore_ascii_case(a)),
        None => false,
    }
}

pub struct ReadNotesTool {
    store: Arc<NoteStore>,
}

impl ReadNotesTool {
    pub fn new(store: Arc<NoteStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for ReadNotesTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "notes".to_string(),
            name: "Read Notes".to_string(),
            description: "Retrieve working notes written by write_note. \
                          Search by keyword (FTS), filter by symbol names, \
                          file paths, or note kind. Call at session start \
                          to recover context from previous sessions, and \
                          before modifying a symbol to find related decisions \
                          or invariants. Every note carries `author` (the \
                          machine that wrote it, e.g. \"BeefyMac (peer)\") and \
                          `author_relation` (self|peer|unknown|ambiguous|\
                          unattributed). Notes about MACHINE STATE — GPU load, \
                          a held daemon lock, a running job — apply only to the \
                          machine in `author`; notes about the CODE apply \
                          everywhere regardless of who wrote them. \
                          OPERATIONAL RECORD is withheld by default: notes \
                          anchored to another role's log (currently \
                          `comaintainer-seat`) are not returned unless your \
                          `query` names the anchor or you pass \
                          `related_to`. When any are withheld the response \
                          says how many and names the anchor."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Full-text search query. Omit to retrieve recent notes."
                    },
                    "symbols": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Return only notes mentioning any of these symbols"
                    },
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Return only notes mentioning any of these file paths"
                    },
                    "kinds": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": [
                                "decision", "attempt", "invariant", "todo", "reflection",
                                "uncertainty", "postmortem_pointer", "redteam_finding",
                                "deviation", "commitment", "follow_up", "goal",
                                "research_finding", "capability_request", "recipe_issue",
                                "checkpoint", "checkpoint_restored", "deferred_question",
                                "tool_decision"
                            ]
                        },
                        "description": "Return only notes of these kinds. \
                                        decision/attempt/invariant/todo: classic working notes. \
                                        reflection: tool calibration notes from prior sessions. \
                                        uncertainty/postmortem_pointer/redteam_finding/deviation: ATOS notes. \
                                        commitment/follow_up/goal: relational + strategic notes \
                                        anchored to a Person, Organization, or Initiative."
                    },
                    "limit": {
                        "type": "integer",
                        "default": 10,
                        "description": "Maximum number of notes to return (capped at 100)"
                    },
                    "scope": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["global", "feature", "session"]
                        },
                        "description": "ATOS scope filter. Omit for legacy behavior (all scopes). \
                                        Common use: scope=['global','feature'] with feature_id \
                                        returns globals plus one feature's notes."
                    },
                    "feature_id": {
                        "type": "string",
                        "description": "Feature id to pair with scope=['feature']. Notes \
                                        in other features are excluded."
                    },
                    "related_to": {
                        "type": "string",
                        "description": "When set, ignore query/symbols/files and return \
                                        notes related to this symbol/file/entity via the \
                                        T2 entity-graph (co-occurrence ranking). Use this \
                                        to discover decisions, invariants, or attempts \
                                        thematically connected to a symbol even when the \
                                        symbol isn't mentioned in their content."
                    },
                    "semantic": {
                        "type": "boolean",
                        "default": true,
                        "description": "When `query` is set and the daemon's embed slot is \
                                        wired, blend BM25 with cosine similarity over the \
                                        note embeddings (T1 retrieval). Default on; set \
                                        false to force FTS5-only behavior."
                    }
                },
                "required": []
            }),
            examples: vec![
                ToolExample {
                    situation: "You're starting work on a symbol and want to know if any previous session recorded a decision, invariant, or failed attempt about it. Do this BEFORE reading code — it can save you from re-discovering constraints the hard way.".into(),
                    call: serde_json::json!({ "symbols": ["EmbedSlot"] }),
                },
                ToolExample {
                    situation: "You're picking up a session and want to find open tasks or recent decisions relevant to the area you're working on.".into(),
                    call: serde_json::json!({ "query": "embedding GPU Metal", "kinds": ["invariant", "attempt"] }),
                },
                ToolExample {
                    situation: "You want to check what the session_reflection tool has flagged about a tool you're about to use heavily — surface known blind spots before relying on it.".into(),
                    call: serde_json::json!({ "kinds": ["reflection"], "query": "blast_radius" }),
                },
                ToolExample {
                    situation: "You're about to modify a symbol and want every note thematically connected to it — even notes whose content doesn't literally mention the symbol. Uses the T2 entity-graph co-occurrence path; surfaces invariants, decisions, and attempts on the same conceptual axis.".into(),
                    call: serde_json::json!({ "related_to": "UrlAllowlistConstraint" }),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "notes": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id":         { "type": "string" },
                                "kind":       { "type": "string" },
                                "content":    { "type": "string" },
                                "symbols":    { "type": "array", "items": { "type": "string" } },
                                "files":      { "type": "array", "items": { "type": "string" } },
                                "scope":      { "type": "string" },
                                "feature_id": { "type": "string" },
                                "created_at": { "type": "integer" },
                                "session_id": { "type": "string" },
                                "author": {
                                    "type": "string",
                                    "description": "Machine that wrote this note, e.g. \"RuggedFox (this machine)\" or \"BeefyMac (peer)\". \"unknown origin\" when the note predates author tracking."
                                },
                                "author_relation": {
                                    "type": "string",
                                    "enum": ["self", "peer", "unknown", "ambiguous", "unattributed"],
                                    "description": "Machine-readable form of `author`. Only \"self\" means this note was written on the machine now reading it — treat notes about machine state (GPU load, held locks, running jobs) as applying ONLY to the machine named in `author`."
                                }
                            }
                        }
                    },
                    "total": { "type": "integer" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    /// Signal: count of unretired `todo`-kind notes. Surfaces backlog
    /// the agent might otherwise miss, without needing a feature
    /// context. Silent when the todo list is empty.
    async fn signal(&self) -> Option<String> {
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

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
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
        // newest are seat bookkeeping.
        let hidden = anchors_to_hide(query);
        let fetch_limit = if hidden.is_empty() {
            limit
        } else {
            limit.saturating_mul(3).min(200)
        };
        let notes = self
            .store
            .read_notes_scoped_semantic(
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
                hidden.first().copied().unwrap_or("")
            ));
        }
        Ok(StepOutput::Json(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failing input this exclusion exists for (ARCH §18.1): a
    /// topical query that names nothing about the seat, against a store
    /// whose newest global decisions are seat bookkeeping.
    #[test]
    fn topical_query_hides_the_seat_log() {
        let hidden = anchors_to_hide(Some("native grounding"));
        assert_eq!(hidden, vec!["comaintainer-seat"]);
        assert!(is_hidden_anchor(Some("comaintainer-seat"), &hidden));
        assert!(!is_hidden_anchor(Some("Sarah Chen"), &hidden));
        assert!(!is_hidden_anchor(None, &hidden));
    }

    #[test]
    fn a_query_naming_the_anchor_opts_back_in() {
        for q in [
            "comaintainer-seat",
            "what did the COMAINTAINER-SEAT decide",
            "seat log: comaintainer-seat pool state",
        ] {
            assert!(
                anchors_to_hide(Some(q)).is_empty(),
                "query {q:?} names the anchor and must not hide it"
            );
        }
    }

    /// No query at all is the UserPromptSubmit index's shape — recent
    /// global decisions, nothing else. That is precisely the read the
    /// seat log was flooding, so it must hide.
    #[test]
    fn the_unqueried_recency_index_hides() {
        assert_eq!(anchors_to_hide(None), vec!["comaintainer-seat"]);
        assert_eq!(anchors_to_hide(Some("")), vec!["comaintainer-seat"]);
    }

    /// Anchor matching is exact-but-case-insensitive, never a substring:
    /// a note anchored to "comaintainer-seat-notes" is a different log
    /// and stays visible until someone adds it to the registry.
    #[test]
    fn anchor_match_is_whole_value_not_substring() {
        let hidden = anchors_to_hide(Some("anything"));
        assert!(is_hidden_anchor(Some("Comaintainer-Seat"), &hidden));
        assert!(!is_hidden_anchor(Some("comaintainer-seat-notes"), &hidden));
    }
}
