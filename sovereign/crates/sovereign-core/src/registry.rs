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
        self.tools
            .iter()
            .find(|t| t.descriptor().id == tool_id)
            .map(|t| t.as_ref())
            .ok_or_else(|| Error::ToolNotFound(tool_id.to_string()))
    }

    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.tools.iter().map(|t| t.descriptor()).collect()
    }

    pub fn count(&self) -> usize {
        self.tools.len()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
