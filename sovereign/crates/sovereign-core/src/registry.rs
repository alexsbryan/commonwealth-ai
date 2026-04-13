use crate::error::{Error, Result};
use crate::traits::Tool;
use crate::types::ToolDescriptor;

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
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
