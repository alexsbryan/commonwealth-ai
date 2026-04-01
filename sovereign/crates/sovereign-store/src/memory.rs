use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::sync::RwLock;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::StateStore;
use sovereign_core::types::*;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub struct InMemoryStateStore {
    conversations: RwLock<HashMap<String, Conversation>>,
    messages: RwLock<Vec<Message>>,
    tasks: RwLock<HashMap<String, Task>>,
    memories: RwLock<Vec<Memory>>,
    documents: RwLock<Vec<DocumentChunk>>,
    permissions: RwLock<HashMap<(String, String), bool>>,
}

impl InMemoryStateStore {
    pub fn new() -> Self {
        Self {
            conversations: RwLock::new(HashMap::new()),
            messages: RwLock::new(Vec::new()),
            tasks: RwLock::new(HashMap::new()),
            memories: RwLock::new(Vec::new()),
            documents: RwLock::new(Vec::new()),
            permissions: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryStateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StateStore for InMemoryStateStore {
    async fn save_message(&self, msg: &Message) -> Result<()> {
        // Ensure conversation exists.
        let mut convos = self.conversations.write().await;
        convos
            .entry(msg.conversation_id.clone())
            .or_insert_with(|| Conversation {
                id: msg.conversation_id.clone(),
                title: None,
                messages: Vec::new(),
                created_at: now(),
                updated_at: now(),
            });

        if let Some(convo) = convos.get_mut(&msg.conversation_id) {
            convo.messages.push(msg.clone());
            convo.updated_at = now();
        }

        self.messages.write().await.push(msg.clone());
        Ok(())
    }

    async fn get_conversation(&self, id: &str) -> Result<Conversation> {
        let convos = self.conversations.read().await;
        convos
            .get(id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("Conversation {id}")))
    }

    async fn list_conversations(&self, limit: usize, offset: usize) -> Result<Vec<Conversation>> {
        let convos = self.conversations.read().await;
        let mut list: Vec<Conversation> = convos.values().cloned().collect();
        list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(list.into_iter().skip(offset).take(limit).collect())
    }

    async fn search_messages(&self, query: &str) -> Result<Vec<Message>> {
        let msgs = self.messages.read().await;
        let query_lower = query.to_lowercase();
        Ok(msgs
            .iter()
            .filter(|m| m.content.to_lowercase().contains(&query_lower))
            .cloned()
            .collect())
    }

    async fn delete_conversation(&self, id: &str) -> Result<()> {
        self.conversations.write().await.remove(id);
        self.messages
            .write()
            .await
            .retain(|m| m.conversation_id != id);
        Ok(())
    }

    async fn save_task(&self, task: &Task) -> Result<()> {
        self.tasks
            .write()
            .await
            .insert(task.id.clone(), task.clone());
        Ok(())
    }

    async fn get_task(&self, id: &str) -> Result<Task> {
        let tasks = self.tasks.read().await;
        tasks
            .get(id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("Task {id}")))
    }

    async fn save_memory(&self, memory: &Memory) -> Result<()> {
        self.memories.write().await.push(memory.clone());
        Ok(())
    }

    async fn get_relevant_memories(&self, _context: &str, _limit: usize) -> Result<Vec<Memory>> {
        Ok(Vec::new())
    }

    async fn store_chunks(&self, chunks: &[DocumentChunk]) -> Result<()> {
        let mut docs = self.documents.write().await;
        for chunk in chunks {
            // Replace existing chunk with same ID.
            docs.retain(|d| d.id != chunk.id);
            docs.push(chunk.clone());
        }
        Ok(())
    }

    async fn search_documents(
        &self,
        _query_embedding: &[f32],
        _query_text: &str,
        _limit: usize,
    ) -> Result<Vec<DocumentChunk>> {
        Ok(Vec::new())
    }

    async fn get_chunks_by_source(&self, source: &str) -> Result<Vec<DocumentChunk>> {
        let docs = self.documents.read().await;
        let mut chunks: Vec<DocumentChunk> = docs
            .iter()
            .filter(|d| d.source == source)
            .cloned()
            .collect();
        chunks.sort_by_key(|c| c.chunk_index);
        Ok(chunks)
    }

    async fn list_sources(&self) -> Result<Vec<String>> {
        let docs = self.documents.read().await;
        let mut sources: Vec<String> = docs
            .iter()
            .map(|d| d.source.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        sources.sort();
        Ok(sources)
    }

    async fn get_permission(&self, tool_id: &str, scope: &str) -> Result<Option<bool>> {
        let perms = self.permissions.read().await;
        Ok(perms.get(&(tool_id.to_string(), scope.to_string())).copied())
    }

    async fn set_permission(&self, tool_id: &str, scope: &str, granted: bool) -> Result<()> {
        self.permissions
            .write()
            .await
            .insert((tool_id.to_string(), scope.to_string()), granted);
        Ok(())
    }
}
