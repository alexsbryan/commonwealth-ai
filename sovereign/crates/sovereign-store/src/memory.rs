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

struct RoutingLogEntry {
    message_hash: String,
    classified_as: String,
    was_correct: Option<bool>,
    created_at: i64,
}

pub struct InMemoryStateStore {
    conversations: RwLock<HashMap<String, Conversation>>,
    messages: RwLock<Vec<Message>>,
    tasks: RwLock<HashMap<String, Task>>,
    memories: RwLock<Vec<Memory>>,
    documents: RwLock<Vec<DocumentChunk>>,
    permissions: RwLock<HashMap<(String, String), bool>>,
    routing_log: RwLock<Vec<RoutingLogEntry>>,
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
            routing_log: RwLock::new(Vec::new()),
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
        let mut mems = self.memories.write().await;
        mems.retain(|m| m.id != memory.id);
        mems.push(memory.clone());
        Ok(())
    }

    async fn get_relevant_memories(&self, context: &str, limit: usize) -> Result<Vec<Memory>> {
        if context.is_empty() {
            return Ok(Vec::new());
        }
        let mems = self.memories.read().await;
        let context_lower = context.to_lowercase();
        let current_time = now();

        let mut scored: Vec<(f64, Memory)> = mems
            .iter()
            .filter(|m| m.content.to_lowercase().contains(&context_lower))
            .filter_map(|m| {
                let months = (current_time - m.last_used) as f64 / (30.0 * 86400.0);
                let decayed = m.confidence * 0.9_f64.powf(months);
                if decayed >= 0.2 {
                    Some((decayed, m.clone()))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, m)| m).collect())
    }

    async fn get_all_memories(&self) -> Result<Vec<Memory>> {
        Ok(self.memories.read().await.clone())
    }

    async fn delete_memory(&self, id: &str) -> Result<()> {
        self.memories.write().await.retain(|m| m.id != id);
        Ok(())
    }

    async fn update_memory_confidence(&self, id: &str, confidence: f64) -> Result<()> {
        let mut mems = self.memories.write().await;
        if let Some(m) = mems.iter_mut().find(|m| m.id == id) {
            m.confidence = confidence;
        }
        Ok(())
    }

    async fn touch_memory(&self, id: &str, timestamp: i64) -> Result<()> {
        let mut mems = self.memories.write().await;
        if let Some(m) = mems.iter_mut().find(|m| m.id == id) {
            m.last_used = timestamp;
        }
        Ok(())
    }

    async fn log_routing(
        &self,
        message_hash: &str,
        classified_as: &str,
        latency_ms: i64,
    ) -> Result<()> {
        let _ = latency_ms;
        self.routing_log.write().await.push(RoutingLogEntry {
            message_hash: message_hash.to_string(),
            classified_as: classified_as.to_string(),
            was_correct: None,
            created_at: now(),
        });
        Ok(())
    }

    async fn get_routing_corrections(&self, limit: usize) -> Result<Vec<RoutingCorrection>> {
        let log = self.routing_log.read().await;
        let corrections: Vec<RoutingCorrection> = log
            .iter()
            .rev()
            .filter(|e| e.was_correct == Some(false))
            .take(limit)
            .map(|e| RoutingCorrection {
                message_hash: e.message_hash.clone(),
                classified_as: e.classified_as.clone(),
                was_correct: false,
                created_at: e.created_at,
            })
            .collect();
        Ok(corrections)
    }

    async fn mark_routing_correct(&self, message_hash: &str, was_correct: bool) -> Result<()> {
        let mut log = self.routing_log.write().await;
        for entry in log.iter_mut().rev() {
            if entry.message_hash == message_hash {
                entry.was_correct = Some(was_correct);
                break;
            }
        }
        Ok(())
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
