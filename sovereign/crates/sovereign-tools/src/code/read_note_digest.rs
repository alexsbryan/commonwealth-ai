//! `read_note_digest` — Fast-slot summarization of scope-filtered notes.
//!
//! The digest is how a freshly-compacted agent session rebuilds context
//! in ≤2k tokens. The opencode plugin's `experimental.session.compacting`
//! hook calls this tool and injects the result as the preamble of the
//! post-compaction turn. Claude Code sessions can call it directly.
//!
//! Cache semantics
//! ---------------
//! Cache key: `(scope_hash, notes_version)`. `notes_version` is the
//! monotonic counter that [`NoteStore`] bumps on every write / delete /
//! retire — so a cached digest is valid precisely until the next note
//! mutation. `scope_hash` is a SHA-256 over the serialized filter
//! params, so two callers asking for the same cut share a cache slot.
//!
//! Miss path: gather up to 100 recent matching notes, format them as
//! `[kind] content` lines (one per note), send to the Fast slot with a
//! fixed system message asking for a ≤2k-token summary that references
//! notes by id, write the result to the cache, return it.
//!
//! Fallback: when `inference` is absent (Claude-Code-only deployment
//! with no local model loaded) the tool returns a concatenated header
//! view with a banner making the degraded state visible. Better to
//! surface the failure than silently emit a stale digest.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{InferenceProvider, Tool};
use sovereign_core::types::*;

use corpus_engine::{NoteRow, NoteScope, NoteStore, ScopeFilter};

pub struct ReadNoteDigestTool {
    notes: Arc<NoteStore>,
    inference: Option<Arc<dyn InferenceProvider>>,
}

impl ReadNoteDigestTool {
    pub fn new(notes: Arc<NoteStore>) -> Self {
        Self { notes, inference: None }
    }

    pub fn with_inference(mut self, inference: Arc<dyn InferenceProvider>) -> Self {
        self.inference = Some(inference);
        self
    }
}

#[async_trait]
impl Tool for ReadNoteDigestTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "read_note_digest".to_string(),
            name: "Read Note Digest".to_string(),
            description:
                "Return a markdown digest (≤2k tokens) summarizing notes that match the \
                 scope/feature/kinds filter. Cached per notes_version — a fresh call right after \
                 a write_note will regenerate. Use at session start or after a compaction event \
                 to rebuild working context without rehydrating every raw note. Reference notes \
                 by id via read_note_by_id when you need the full content."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "scope": {
                        "type": "array",
                        "items": {"type": "string", "enum": ["global", "feature", "session"]},
                        "description": "Scope filter. Defaults to ['global','feature'] when \
                                        feature_id is set, ['global'] otherwise."
                    },
                    "feature_id": {
                        "type": "string",
                        "description": "Pairs with scope=['feature']. Narrows to one feature."
                    },
                    "kinds": {
                        "type": "array",
                        "items": {"type": "string",
                                   "enum": ["decision","attempt","invariant","todo","reflection"]},
                        "description": "Kind filter. Defaults to [decision,invariant,attempt]."
                    },
                    "limit": {
                        "type": "integer",
                        "default": 100,
                        "description": "Max notes to consider (capped at 100)."
                    }
                },
                "required": []
            }),
            examples: vec![ToolExample {
                situation: "You just came back from a compaction event and need to rebuild \
                            context for the active feature without fetching every raw note. \
                            Call this first, then expand any [note:...] reference you need."
                    .into(),
                call: serde_json::json!({
                    "scope": ["global", "feature"],
                    "feature_id": "atos-version-flag"
                }),
            }],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Slow,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "markdown": { "type": "string" },
                    "stale":    { "type": "boolean" },
                    "hit":      { "type": "boolean",
                                  "description": "True when the digest was served from cache" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let feature_id = params
            .get("feature_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        let scopes: Vec<NoteScope> = params
            .get("scope")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().and_then(NoteScope::parse))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| {
                if feature_id.is_some() {
                    vec![NoteScope::Global, NoteScope::Feature]
                } else {
                    vec![NoteScope::Global]
                }
            });

        let kinds: Vec<String> = params
            .get("kinds")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_else(|| {
                vec![
                    "decision".into(),
                    "invariant".into(),
                    "attempt".into(),
                ]
            });
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(100)
            .min(100);

        // Cache lookup. scope_hash depends on every dimension that
        // would change the digest's content.
        let version = self
            .notes
            .notes_version()
            .await
            .map_err(|e| Error::Tool {
                tool_id: "read_note_digest".into(),
                message: e.to_string(),
            })?;
        let scope_hash = compute_scope_hash(&scopes, feature_id.as_deref(), &kinds, limit);

        if let Some(cached) = self
            .notes
            .digest_cache_get(&scope_hash, version)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "read_note_digest".into(),
                message: e.to_string(),
            })?
        {
            return Ok(StepOutput::Json(json!({
                "digest_md": cached,
                "cached": true,
                "notes_version": version,
            })));
        }

        // Miss: load notes and build the digest.
        let filter = ScopeFilter {
            scopes: scopes.clone(),
            feature_id: feature_id.clone(),
        };
        let notes = self
            .notes
            .read_notes_scoped(None, &[], &[], &kinds, limit, false, &filter)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "read_note_digest".into(),
                message: e.to_string(),
            })?;

        if notes.is_empty() {
            let digest = "No active notes match the current scope.".to_string();
            let _ = self
                .notes
                .digest_cache_put(&scope_hash, version, &digest, 0)
                .await;
            return Ok(StepOutput::Json(json!({
                "digest_md": digest,
                "cached": false,
                "notes_version": version,
                "note_count": 0,
            })));
        }

        // Render notes into a compact prompt.
        let raw = format_notes_for_prompt(&notes);

        // Fast-slot path. Absent inference: degraded header-only view
        // with an explicit banner so operators see the fallback.
        let digest = match self.inference.as_ref() {
            Some(provider) => summarize_via_fast_slot(provider.as_ref(), &raw).await?,
            None => fallback_header_digest(&notes),
        };

        // Cache write is best-effort; a DB error here should not fail
        // the tool call (the caller already has the digest).
        let _ = self
            .notes
            .digest_cache_put(
                &scope_hash,
                version,
                &digest,
                // Rough char→token heuristic — swapped for real
                // tokenizer counts when one is threaded down.
                (digest.len() / 4) as i64,
            )
            .await;

        Ok(StepOutput::Json(json!({
            "digest_md": digest,
            "cached": false,
            "notes_version": version,
            "note_count": notes.len(),
        })))
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Stable hash over the scope dimensions that define a unique digest.
/// SipHash (std's `DefaultHasher`) is sufficient here: the value lives
/// in a private cache keyed alongside `notes_version`, so collisions
/// cost at worst one extra Fast-slot call — not a correctness issue.
fn compute_scope_hash(
    scopes: &[NoteScope],
    feature_id: Option<&str>,
    kinds: &[String],
    limit: usize,
) -> String {
    let mut hasher = DefaultHasher::new();
    // Sort scope strings so [Global, Feature] hashes the same as
    // [Feature, Global]. Callers pass them in either order.
    let mut scope_strs: Vec<&str> = scopes.iter().map(|s| s.as_str()).collect();
    scope_strs.sort_unstable();
    for s in scope_strs {
        s.hash(&mut hasher);
    }
    feature_id.unwrap_or("").hash(&mut hasher);
    let mut sorted_kinds: Vec<&String> = kinds.iter().collect();
    sorted_kinds.sort();
    for k in sorted_kinds {
        k.hash(&mut hasher);
    }
    limit.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn format_notes_for_prompt(notes: &[NoteRow]) -> String {
    let mut out = String::with_capacity(notes.len() * 160);
    for n in notes {
        let scope_tag = if n.scope == "global" {
            "global".to_string()
        } else {
            format!("{}:{}", n.scope, n.feature_id.as_deref().unwrap_or(""))
        };
        out.push_str(&format!("[note:{}] [{}] [{}] {}\n", n.id, n.kind, scope_tag, n.content.trim()));
    }
    out
}

fn fallback_header_digest(notes: &[NoteRow]) -> String {
    let mut out = String::from(
        "> **Digest fallback.** The Fast inference slot is not available — \
         returning note headers without summarization. The operator should \
         start the mesh daemon to restore full digests.\n\n"
    );
    for n in notes.iter().take(40) {
        let first_line = n.content.lines().next().unwrap_or("").trim();
        let snippet: String = first_line.chars().take(120).collect();
        out.push_str(&format!("- `[note:{}]` [{}] {}\n", n.id, n.kind, snippet));
    }
    if notes.len() > 40 {
        out.push_str(&format!("\n… {} more notes; call read_notes to see them all.\n", notes.len() - 40));
    }
    out
}

async fn summarize_via_fast_slot(
    provider: &dyn InferenceProvider,
    raw: &str,
) -> Result<String> {
    let system = "You are a summarizer. Given a list of engineering notes (one per line, \
                  each prefixed with [note:<id>] [<kind>] [<scope>]), produce a compact \
                  markdown digest of the key invariants, decisions, and active attempts. \
                  Reference notes by id via [note:<id>] so the agent can expand what it \
                  needs. Do not invent facts — stay strictly grounded in the input. \
                  Target 300–600 words total.";
    let request = CompletionRequest {
        prompt: raw.to_string(),
        system_message: Some(system.to_string()),
        preferred_speed: Speed::Fast,
        max_tokens: Some(800),
        temperature: Some(0.1),
        structured_output: None,
        think_budget: Some(0),
        top_k: None,
        top_p: None,
        oicp: None,
        tools: None,
        tool_choice: None,
            model_id: None,
            enable_thinking: None,
    sampling_mode: None,
    assistant_prefix: None,
    cmd_prefix: None,
    url_allowlist: None,
    evidence_id_allowlist: None,
    lark_grammar: None,
    };
    let response = provider.complete(&request).await.map_err(|e| Error::Tool {
        tool_id: "read_note_digest".into(),
        message: e.to_string(),
    })?;
    Ok(response.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_hash_is_stable_for_same_inputs() {
        let scopes = vec![NoteScope::Global, NoteScope::Feature];
        let kinds = vec!["decision".to_string(), "invariant".to_string()];
        let a = compute_scope_hash(&scopes, Some("feat-a"), &kinds, 100);
        let b = compute_scope_hash(&scopes, Some("feat-a"), &kinds, 100);
        assert_eq!(a, b);
    }

    #[test]
    fn scope_hash_differs_when_feature_changes() {
        let scopes = vec![NoteScope::Feature];
        let kinds = vec!["decision".to_string()];
        let a = compute_scope_hash(&scopes, Some("feat-a"), &kinds, 100);
        let b = compute_scope_hash(&scopes, Some("feat-b"), &kinds, 100);
        assert_ne!(a, b);
    }

    #[test]
    fn scope_hash_differs_when_scopes_change() {
        let a = compute_scope_hash(&[NoteScope::Global], None, &[], 100);
        let b = compute_scope_hash(&[NoteScope::Session], None, &[], 100);
        assert_ne!(a, b);
    }

    #[test]
    fn fallback_header_digest_references_notes_by_id() {
        let rows = vec![
            NoteRow {
                id: "abc-1".into(),
                kind: "decision".into(),
                content: "use FTS5".into(),
                symbols: vec![],
                files: vec![],
                session_id: "s".into(),
                created_at: "2026-04-19T00:00:00+00:00".into(),
                tool_name: None,
                retired_at: None,
                retired_by: None,
                scope: "global".into(),
                feature_id: None,
                promoted_from: None,
                related_entity: None,
                source: "agent".into(),
                supersedes: None,
                payload_json: None,
            },
        ];
        let out = fallback_header_digest(&rows);
        assert!(out.contains("[note:abc-1]"));
        assert!(out.contains("decision"));
        assert!(out.contains("Digest fallback"));
    }
}
