use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::tool_result_cache::{wrap_cached, CacheKey, ToolResultCache};
use crate::traits::Tool;
use crate::types::{Idempotency, StepOutput, ToolContext, ToolDescriptor};

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

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

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

    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.tools.iter().map(|t| t.descriptor()).collect()
    }

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
