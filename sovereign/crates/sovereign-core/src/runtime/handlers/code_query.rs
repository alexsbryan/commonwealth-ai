// SPDX-License-Identifier: AGPL-3.0-or-later
//! `CodeQuery` handler — the first-class route for "how does this code work"
//! questions (Inc 4).
//!
//! Code questions ("how does inference run", "what calls gate_answer", "where is
//! X implemented", "trace the request flow") earn a dedicated route. Retrieval
//! rides the intent-summary bridge (plain-English -> symbol) and the answer is
//! grounded in the SCIP call-graph trace — which the KnowledgeQuery synthesis
//! path already injects via `code_trace::build_code_trace_block`.
//!
//! What this route adds over KnowledgeQuery is **scope**: it restricts retrieval
//! to CODE corpora (those with a `scip_graph.db`), so the 30+ non-code corpora
//! can't dilute a code answer — the dilution measured in the Inc 3 grade
//! (`process`, conversation hits crowding the code summaries). The
//! implementation is deliberately thin: detect code corpora, narrow the
//! conversation's `enabled_corpora` to them, and delegate to
//! `handle_knowledge_query` (retrieval + trace augmentation + synthesis).
//!
//! Safety / no-over-rotation: when no code corpus is installed, the handler
//! falls straight through to the plain knowledge path — a non-code deployment
//! behaves exactly as before. The code-corpus signal is the on-disk SCIP graph,
//! NOT the `CorpusKind::Code` tag (an indexed code corpus may be `knowledge`-
//! kind, e.g. commonwealth-ai), so detection is robust.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::error::Result;
use crate::types::*;

use super::super::Runtime;

impl Runtime {
    pub(crate) async fn handle_code_query(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        coarse_intent: Option<String>,
        self_assessment: Option<String>,
        routing_trigger: Option<String>,
    ) -> Result<Response> {
        let code_ids = self.code_corpus_ids().await;
        if code_ids.is_empty() {
            // No code corpus installed → behave exactly like KnowledgeQuery.
            tracing::info!(
                target: "runtime.code_query",
                "CodeQuery: no code corpus installed; falling back to the knowledge path"
            );
            return self
                .handle_knowledge_query(
                    message,
                    conversation_id,
                    context,
                    &Intent::KnowledgeQuery,
                    coarse_intent,
                    self_assessment,
                    routing_trigger,
                )
                .await;
        }

        // Scope retrieval to code corpora so non-code corpora can't dilute the
        // answer. Respect an explicit conversation scope by intersecting; if the
        // intersection is empty (the user scoped to non-code corpora yet asked a
        // code question), prefer the code corpora — the question is about code.
        let scoped_ids: Vec<String> = match context.conversation.enabled_corpora.as_deref() {
            Some(enabled) => {
                let allowed: HashSet<&str> = enabled.iter().map(String::as_str).collect();
                let kept: Vec<String> = code_ids
                    .iter()
                    .filter(|c| allowed.contains(c.as_str()))
                    .cloned()
                    .collect();
                if kept.is_empty() {
                    code_ids
                } else {
                    kept
                }
            }
            None => code_ids,
        };

        tracing::info!(
            target: "runtime.code_query",
            corpora = ?scoped_ids,
            "CodeQuery: scoping retrieval to code corpora"
        );

        let mut scoped = context.clone();
        scoped.conversation.enabled_corpora = Some(scoped_ids);

        // Delegate to the knowledge path, which runs retrieval (now code-scoped),
        // the call-graph trace augmentation, and synthesis. Pass KnowledgeQuery
        // as the operation so all the downstream effort/format logic is unchanged.
        self.handle_knowledge_query(
            message,
            conversation_id,
            &scoped,
            &Intent::KnowledgeQuery,
            coarse_intent,
            self_assessment,
            routing_trigger,
        )
        .await
    }

    /// Corpus ids that have an on-disk SCIP graph — the robust "this is a code
    /// corpus" signal, independent of the `CorpusKind::Code` tag (an indexed code
    /// corpus may be `knowledge`-kind, e.g. commonwealth-ai). Best-effort
    /// filesystem scan under the data dir; matches the layout `code_trace` and
    /// the daemon writer use.
    pub(crate) async fn code_corpus_ids(&self) -> Vec<String> {
        let base = std::env::var("SOVEREIGN_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".sovereign"));
        let indexes = base.join("indexes");
        let mut ids = Vec::new();
        if let Some(engine) = &self.corpus_engine {
            for info in engine.installed_indexes().await.unwrap_or_default() {
                if indexes.join(&info.corpus_id).join("scip_graph.db").exists() {
                    ids.push(info.corpus_id);
                }
            }
        }
        ids
    }
}
