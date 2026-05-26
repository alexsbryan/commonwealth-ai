//! `MeshLandscapeDigestClient` — desktop-side `LandscapeDigestProvider`
//! that fetches assembled digest blocks from an attached daemon's
//! `POST /v1/knowledge/landscape_digest` endpoint.
//!
//! Used by `sovereign-desktop` in attach mode: the desktop has no
//! local `KnowledgeViewManager` (the daemon owns enrichment), so
//! prompt-side digest splicing comes from the daemon over HTTP.
//!
//! Soft-fails on transport / non-success / malformed-response (each
//! handled identically to `MeshKnowledgeClient`): on any failure
//! the splice path inserts an empty digest list, which the runtime
//! treats as "no enriched context this turn" — the same behaviour
//! the desktop got pre-attach-mode-cutover when `KnowledgeView` was
//! disabled in Settings. Never propagate.

use std::time::Duration;

use async_trait::async_trait;
use commonwealth_inference::oicp::{LandscapeDigestRequest, LandscapeDigestResponse};
use sovereign_core::traits::LandscapeDigestProvider;
use sovereign_core::types::{ConversationContext, LandscapeDigest};

const CLIENT_TIMEOUT: Duration = Duration::from_secs(6);

/// Talks to `http://<base_url>/v1/knowledge/landscape_digest`. In
/// production this is `http://127.0.0.1:9741` — the daemon's client
/// API port. Cheap to clone.
///
/// Holds the caller-side `local_only_skill_ids` so the trait
/// impl's `active_skill: Option<&str>` parameter can be resolved
/// to the daemon's `active_is_local_only: bool` wire field
/// without requiring the daemon to re-implement the skill
/// registry. The desktop has the canonical registry; the daemon
/// just trusts the resolved flag.
pub struct MeshLandscapeDigestClient {
    http: reqwest::Client,
    base_url: String,
    local_only_skill_ids: Vec<String>,
}

impl MeshLandscapeDigestClient {
    /// Construct a client posting to the given base URL. Returns
    /// `Err` only if reqwest fails to build the underlying client
    /// (effectively never — bad TLS config doesn't apply to
    /// localhost HTTP).
    ///
    /// `local_only_skill_ids` should match the desktop's
    /// `SkillRegistry::local_only_skill_ids` so the privacy gate
    /// applied daemon-side stays consistent with what an
    /// in-process `KnowledgeViewManager` would have applied. Pass
    /// an empty vec to disable the gate (treat all skills as
    /// non-local-only).
    pub fn new(
        base_url: impl Into<String>,
        local_only_skill_ids: Vec<String>,
    ) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder().timeout(CLIENT_TIMEOUT).build()?;
        Ok(Self {
            http,
            base_url: base_url.into(),
            local_only_skill_ids,
        })
    }
}

#[async_trait]
impl LandscapeDigestProvider for MeshLandscapeDigestClient {
    async fn splice_landscape_digests(
        &self,
        ctx: &mut ConversationContext,
        active_skill: Option<&str>,
    ) {
        let active_is_local_only = active_skill
            .map(|s| self.local_only_skill_ids.iter().any(|id| id == s))
            .unwrap_or(false);
        let body = LandscapeDigestRequest {
            active_skill: active_skill.map(|s| s.to_string()),
            active_is_local_only,
            conversation_messages: ctx
                .conversation
                .messages
                .iter()
                .map(|m| m.content.clone())
                .collect(),
        };
        let url = format!("{}/v1/knowledge/landscape_digest", self.base_url);

        let response = match self.http.post(&url).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                // Daemon not running, port not bound, local
                // firewall — never propagate. Splice degrades to an
                // empty digest list (identical to KnowledgeView=off).
                tracing::debug!(
                    url = %url,
                    error = %e,
                    "landscape digest client: transport error"
                );
                ctx.set_landscape_digests(Vec::new());
                return;
            }
        };

        if !response.status().is_success() {
            tracing::debug!(
                url = %url,
                status = %response.status(),
                "landscape digest client: non-success status"
            );
            ctx.set_landscape_digests(Vec::new());
            return;
        }

        let parsed: LandscapeDigestResponse = match response.json().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    url = %url,
                    error = %e,
                    "landscape digest client: malformed response"
                );
                ctx.set_landscape_digests(Vec::new());
                return;
            }
        };

        let digests: Vec<LandscapeDigest> = parsed
            .digests
            .into_iter()
            .map(|e| LandscapeDigest {
                view_id: e.view_id,
                body: e.body,
            })
            .collect();
        tracing::debug!(
            count = digests.len(),
            "landscape digest client: spliced digests from daemon"
        );
        ctx.set_landscape_digests(digests);
    }

    // entity_inventory uses the trait default (None). The daemon
    // exposes its inventory via the manager's atlas state, but the
    // current memory-decay path treats `None` as "uniform decay" —
    // a strictly-correct fallback. Wiring an HTTP endpoint for the
    // inventory is a separate pass once we measure whether the
    // half-rate decay actually moves the needle in attach mode.
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_inference::oicp::{LandscapeDigestEntry, LandscapeDigestResponse};
    use sovereign_core::types::{Conversation, ConversationId, Message, MessageId, Role};

    /// Spin up a hand-rolled axum server that returns a fixed
    /// `LandscapeDigestResponse` and verify the client maps it into
    /// `ctx.knowledge_view_digests`.
    #[tokio::test]
    async fn client_maps_response_to_context() {
        use axum::{routing::post, Json, Router};

        let response = LandscapeDigestResponse {
            digests: vec![
                LandscapeDigestEntry {
                    view_id: "personal-knowledge".into(),
                    body: "## Personal\n- thing\n".into(),
                },
                LandscapeDigestEntry {
                    view_id: "conversation-history".into(),
                    body: "## Past chats\n- other\n".into(),
                },
            ],
        };
        let resp_clone = response.clone();
        let app = Router::new().route(
            "/v1/knowledge/landscape_digest",
            post(move |Json(_req): Json<LandscapeDigestRequest>| {
                let r = resp_clone.clone();
                async move { Json(r) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let client = MeshLandscapeDigestClient::new(format!("http://{addr}"), vec![]).unwrap();

        let conv_id = ConversationId::new();
        let mut ctx = ConversationContext {
            conversation: Conversation {
                id: conv_id.clone(),
                created_at: 0,
                updated_at: 0,
                version: 0,
                deleted_at: None,
                messages: vec![Message {
                    id: MessageId::new(),
                    conversation_id: conv_id.clone(),
                    role: Role::User,
                    content: "hello".into(),
                    created_at: 0,
                    metadata: None,
                    version: 0,
                }],
                skill_id: None,
                title: None,
                enabled_corpora: None,
            searched_sources: None,
            },
            memories: vec![],
            working_memory: None,
            installed_corpora: vec![],
            document_session: None,
            topic_context: None,
            knowledge_view_digests: None,
            temporal_tensions: Vec::new(),
            compacted_history: None,
            tool_dossier: None,
            intent_policy: None,
        };

        client.splice_landscape_digests(&mut ctx, None).await;
        let digests = ctx.knowledge_view_digests.expect("digests must be set");
        assert_eq!(digests.len(), 2);
        assert_eq!(digests[0].view_id, "personal-knowledge");
        assert_eq!(digests[1].view_id, "conversation-history");
    }

    /// Transport failure (daemon not bound) must not panic — the
    /// client sets an empty digest list and returns.
    #[tokio::test]
    async fn transport_failure_yields_empty_digests() {
        // Bind & immediately close to get a guaranteed-unused port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let client = MeshLandscapeDigestClient::new(format!("http://{addr}"), vec![]).unwrap();

        let conv_id = ConversationId::new();
        let mut ctx = ConversationContext {
            conversation: Conversation {
                id: conv_id,
                created_at: 0,
                updated_at: 0,
                version: 0,
                deleted_at: None,
                messages: vec![],
                skill_id: None,
                title: None,
                enabled_corpora: None,
            searched_sources: None,
            },
            memories: vec![],
            working_memory: None,
            installed_corpora: vec![],
            document_session: None,
            topic_context: None,
            knowledge_view_digests: None,
            temporal_tensions: Vec::new(),
            compacted_history: None,
            tool_dossier: None,
            intent_policy: None,
        };

        client.splice_landscape_digests(&mut ctx, None).await;
        let digests = ctx.knowledge_view_digests.expect("digests must be Some(empty)");
        assert!(digests.is_empty());
    }
}
