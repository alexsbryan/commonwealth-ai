use std::collections::HashMap;
use std::sync::Arc;

use crate::error::Result;
use crate::registry::ToolRegistry;
use crate::traits::{ApprovalChannel, InferenceProvider, StateStore};
use crate::types::{Plan, StepOutput, Task};

pub struct Executor {
    pub inference: Arc<dyn InferenceProvider>,
    pub tools: Arc<ToolRegistry>,
    pub store: Arc<dyn StateStore>,
}

pub struct TaskContext {
    pub task: Task,
    pub approval: Box<dyn ApprovalChannel>,
    pub completed: HashMap<usize, StepOutput>,
}

impl Executor {
    pub fn new(
        inference: Arc<dyn InferenceProvider>,
        tools: Arc<ToolRegistry>,
        store: Arc<dyn StateStore>,
    ) -> Self {
        Self {
            inference,
            tools,
            store,
        }
    }

    pub async fn run(&self, _plan: &Plan, _ctx: &mut TaskContext) -> Result<String> {
        todo!("Phase 5: implement plan execution")
    }
}
