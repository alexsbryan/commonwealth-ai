use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::{Error, Result};
use crate::traits::Tool;
use crate::types::ToolDescriptor;

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
    call_counts: Mutex<HashMap<String, u64>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            call_counts: Mutex::new(HashMap::new()),
        }
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
                let available: Vec<String> = self.tools
                    .iter()
                    .map(|t| t.descriptor().id)
                    .collect();
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
        let mut v: Vec<(String, u64)> = counts
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// Remove all tools whose ID starts with the given prefix.
    /// Used when reconnecting an MCP server to replace old tool registrations.
    pub fn remove_by_prefix(&mut self, prefix: &str) {
        self.tools.retain(|t| !t.descriptor().id.starts_with(prefix));
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
