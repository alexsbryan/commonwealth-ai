// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `ToolRegistry` — the daemon's tool inventory and dispatch point.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::tool_result_cache::{wrap_cached, CacheKey, ToolResultCache};
use crate::traits::Tool;
use crate::types::{Idempotency, StepOutput, ToolContext, ToolDescriptor};

/// Owns every registered `Tool`: lookup for dispatch, descriptor listing for
/// prompts, per-tool call counters, and the optional Tier-4 result cache.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
    call_counts: Mutex<HashMap<String, u64>>,
    /// Tier 4 — optional per-conversation result cache. When
    /// wired, `call_cached` consults it before dispatching to
    /// `tool.execute()`, and stores idempotent results back.
    /// Constructor leaves it `None`; daemons set it via
    /// [`Self::with_cache`].
    cache: Option<Arc<ToolResultCache>>,
}

impl ToolRegistry {
    /// Empty registry, no cache wired.
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            call_counts: Mutex::new(HashMap::new()),
            cache: None,
        }
    }

    /// Wire a shared `ToolResultCache` (Tier 4). Callers that
    /// dispatch via [`Self::call_cached`] then benefit from
    /// turn-windowed result reuse on idempotent tools. Sites
    /// that still call `get(...).execute(...)` directly are
    /// unaffected — the cache is opt-in.
    pub fn with_cache(mut self, cache: Arc<ToolResultCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    /// Add a tool. No dedupe — on duplicate ids the first registered wins at `get`.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    /// Look up by id: exact match first, then case-insensitive (models sometimes
    /// capitalize tool names). `Error::ToolNotFound` — with a warn log listing
    /// what exists — when absent.
    pub fn get(&self, tool_id: &str) -> Result<&dyn Tool> {
        // Try exact match first, then case-insensitive. Models sometimes
        // capitalize tool names ("Document" vs "document").
        let tool_id_lower = tool_id.to_lowercase();
        self.tools
            .iter()
            .find(|t| {
                let id = t.descriptor().id;
                id == tool_id || id.to_lowercase() == tool_id_lower
            })
            .map(|t| t.as_ref())
            .ok_or_else(|| {
                let available: Vec<String> = self.tools.iter().map(|t| t.descriptor().id).collect();
                tracing::warn!(
                    requested = tool_id,
                    available = ?available,
                    "Tool not found"
                );
                Error::ToolNotFound(tool_id.to_string())
            })
    }

    /// Every registered tool's descriptor — the router/planner catalog.
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.tools.iter().map(|t| t.descriptor()).collect()
    }

    /// Deterministic authority claims over `question` from every
    /// registered tool (FINANCIAL_CORPORA.md §7.3) — the surface the
    /// router's authority pre-check consults before any similarity-based
    /// intent classification. Sorted by `(tool_id, corpus_id)` — THE tie
    /// rule when several in-scope stores claim: the first claim after
    /// this sort is the one the gate names in logs and routing meta, and
    /// the agentic planner sees every claimant regardless.
    pub fn authority_claims(&self, question: &str) -> Vec<crate::types::AuthorityClaim> {
        let mut claims: Vec<_> = self.tools.iter().flat_map(|t| t.claims(question)).collect();
        claims.sort_by(|a, b| {
            (a.tool_id.as_str(), a.corpus_id.as_str())
                .cmp(&(b.tool_id.as_str(), b.corpus_id.as_str()))
        });
        claims
    }

    /// Every corpus any registered tool declares authority over,
    /// question-independent (order authority-guard-at-exit). The
    /// answer-exit numeric guard's arming surface: same declaration
    /// index as [`Self::authority_claims`], read at corpus granularity,
    /// same `(tool_id, corpus_id)` tie-rule sort. Empty on every install
    /// with no authoritative store — the guard's structural no-op case.
    pub fn authority_domains(&self) -> Vec<crate::types::AuthorityClaim> {
        let mut domains: Vec<_> = self
            .tools
            .iter()
            .flat_map(|t| t.authority_domains())
            .collect();
        domains.sort_by(|a, b| {
            (a.tool_id.as_str(), a.corpus_id.as_str())
                .cmp(&(b.tool_id.as_str(), b.corpus_id.as_str()))
        });
        domains
    }

    /// Number of registered tools.
    pub fn count(&self) -> usize {
        self.tools.len()
    }

    /// Increment the call counter for a tool. Called by the MCP handler on every
    /// successful tools/call dispatch. Counts reset when the server restarts.
    pub fn record_call(&self, tool_id: &str) {
        if let Ok(mut counts) = self.call_counts.lock() {
            *counts.entry(tool_id.to_string()).or_insert(0) += 1;
        }
    }

    /// Snapshot of call counts since server start, sorted by count descending.
    pub fn call_counts(&self) -> Vec<(String, u64)> {
        let counts = self.call_counts.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<(String, u64)> = counts.iter().map(|(k, v)| (k.clone(), *v)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// Remove all tools whose ID starts with the given prefix.
    /// Used when reconnecting an MCP server to replace old tool registrations.
    pub fn remove_by_prefix(&mut self, prefix: &str) {
        self.tools
            .retain(|t| !t.descriptor().id.starts_with(prefix));
    }

    /// Tier 4 cache-aware dispatch. Consults the wired
    /// `ToolResultCache` (if any) before invoking the tool, and
    /// stores idempotent results post-call.
    ///
    /// Behaviour:
    /// - Cache miss OR non-idempotent tool → executes normally,
    ///   never wraps the result. Idempotent tool: stores under
    ///   the per-conversation key.
    /// - Cache hit → returns `wrap_cached(...)` carrying the
    ///   `{cached, stored_at_turn, current_turn, result}` envelope.
    ///   The model sees the banner and can choose to re-issue.
    /// - No cache wired → identical to calling
    ///   `get(tool_id).execute(args, ctx)` directly. Sites can
    ///   opt into `call_cached` without depending on cache
    ///   wiring.
    pub async fn call_cached(
        &self,
        tool_id: &str,
        args: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let tool = self.get(tool_id)?;
        let descriptor = tool.descriptor();
        let is_idempotent = matches!(descriptor.idempotency, Idempotency::Idempotent);
        // Cache enabled + tool is idempotent + conversation_id is
        // non-empty: look up before dispatching.
        let key = if self.cache.is_some() && is_idempotent && !ctx.conversation_id.is_empty() {
            Some(CacheKey::new(tool_id, &ctx.conversation_id, args))
        } else {
            None
        };
        if let (Some(cache), Some(k)) = (self.cache.as_ref(), key.as_ref()) {
            if let Some(entry) = cache.get(k, ctx.turn_index) {
                tracing::debug!(
                    tool_id,
                    stored_at_turn = entry.stored_at_turn,
                    current_turn = ctx.turn_index,
                    "tool_result_cache: hit"
                );
                return Ok(StepOutput::Json(wrap_cached(&entry, ctx.turn_index)));
            }
        }
        let result = tool.execute(args, ctx).await?;
        // Store idempotent JSON results. Text / streaming outputs
        // skip caching — they're harder to wrap and typically the
        // sites that produce them (e.g. streaming completions)
        // already manage their own caching.
        if let (Some(cache), Some(k)) = (self.cache.as_ref(), key) {
            if let StepOutput::Json(ref value) = result {
                cache.put(k, value.clone(), ctx.turn_index);
            }
        }
        Ok(result)
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AuthorityClaim, Effect, Idempotency, Latency, Permission, Scope};
    use async_trait::async_trait;

    /// A fake authoritative store: claims any question containing both
    /// "acme" and "revenue" for its declared corpus.
    struct FakeStoreTool {
        corpus: &'static str,
    }

    #[async_trait]
    impl Tool for FakeStoreTool {
        fn descriptor(&self) -> ToolDescriptor {
            ToolDescriptor {
                id: "fake_store".into(),
                name: "Fake Store".into(),
                description: "test".into(),
                parameters: serde_json::json!({}),
                examples: vec![],
                effect: Effect::Read,
                idempotency: Idempotency::Idempotent,
                latency: Latency::Fast,
                scope: Scope::Persistent,
                output_schema: None,
            }
        }
        fn required_permissions(&self) -> Vec<Permission> {
            vec![]
        }
        async fn execute(&self, _p: &serde_json::Value, _c: &ToolContext) -> Result<StepOutput> {
            Ok(StepOutput::Text("ok".into()))
        }
        fn claims(&self, question: &str) -> Vec<AuthorityClaim> {
            let q = question.to_lowercase();
            if q.contains("acme") && q.contains("revenue") {
                vec![AuthorityClaim {
                    tool_id: "fake_store".into(),
                    corpus_id: self.corpus.into(),
                    matched: "entity 'acme' + concept term 'revenue'".into(),
                }]
            } else {
                Vec::new()
            }
        }
        fn authority_domains(&self) -> Vec<AuthorityClaim> {
            vec![AuthorityClaim {
                tool_id: "fake_store".into(),
                corpus_id: self.corpus.into(),
                matched: "declared authoritative".into(),
            }]
        }
    }

    #[test]
    fn authority_claims_default_is_empty_and_claims_are_sorted() {
        let mut reg = ToolRegistry::new();
        // Register in REVERSE corpus order to prove the tie rule sorts.
        reg.register(Box::new(FakeStoreTool { corpus: "corpus-b" }));
        reg.register(Box::new(FakeStoreTool { corpus: "corpus-a" }));

        // The failing input, by name: a question with no entity match
        // claims nothing — generic finance wording never routes on
        // authority ("What's the difference between gross and net
        // margin?" stays a knowledge question).
        assert!(reg
            .authority_claims("What's the difference between gross and net margin?")
            .is_empty());

        let claims = reg.authority_claims("What was Acme's revenue in fiscal 2025?");
        assert_eq!(claims.len(), 2, "both in-scope stores claim");
        // Tie rule: (tool_id, corpus_id) sort — corpus-a is named first
        // regardless of registration order.
        assert_eq!(claims[0].corpus_id, "corpus-a");
        assert_eq!(claims[1].corpus_id, "corpus-b");
    }

    /// The corpus-granularity read of the same declaration index
    /// (order authority-guard-at-exit): question-independent, so a
    /// question `claims` deliberately declines ("why did …") still sees
    /// the declaration; same tie-rule sort; and an empty registry — the
    /// no-authoritative-store install — declares nothing, which is the
    /// exit guard's structural no-op.
    #[test]
    fn authority_domains_are_question_independent_and_sorted() {
        assert!(ToolRegistry::new().authority_domains().is_empty());

        let mut reg = ToolRegistry::new();
        reg.register(Box::new(FakeStoreTool { corpus: "corpus-b" }));
        reg.register(Box::new(FakeStoreTool { corpus: "corpus-a" }));

        // The failing input for question-level arming, by name: this
        // question claims NOTHING at question granularity …
        assert!(reg
            .authority_claims("Why did Acme's sales increase?")
            .is_empty());
        // … yet the corpus-level declaration is visible regardless.
        let domains = reg.authority_domains();
        assert_eq!(domains.len(), 2);
        assert_eq!(domains[0].corpus_id, "corpus-a");
        assert_eq!(domains[1].corpus_id, "corpus-b");
    }
}
