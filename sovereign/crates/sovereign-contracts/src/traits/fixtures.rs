// SPDX-License-Identifier: AGPL-3.0-or-later
//! In-memory [`ConversationStore`] for logic tests — seeding, never SQL
//! (fp-32; the [`crate::notes::fixtures::RecordingNotes`] shape: an
//! off-by-default double beside the trait, enabled per consumer with the
//! `test-fixtures` feature).
//!
//! Its contract mirrors the real store's where logic tests lean on it:
//! `list_conversations` is newest-first (`updated_at` DESC), excludes
//! tombstoned rows, and carries NO message rows (the sqlite listing
//! selects conversation columns only — a caller wanting history
//! re-fetches through `get_conversation`). `save_message` materializes
//! the conversation row on first append and bumps `updated_at` on every
//! later one.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;

use super::ConversationStore;
use crate::error::{Error, Result};
use crate::types::{Conversation, Message};

/// One conversation row — the columns the listing serves.
#[derive(Debug, Clone)]
struct ConvoRow {
    title: Option<String>,
    created_at: i64,
    updated_at: i64,
    skill_id: Option<String>,
    enabled_corpora: Option<Vec<String>>,
    /// Soft-delete tombstone: a deleted row is excluded from listings and
    /// `get_conversation`, mirroring the store's `deleted_at IS NULL` WHERE
    /// clause (the row itself is kept, exactly like the sqlite backend).
    deleted: bool,
}

/// In-memory `ConversationStore` — captures what tests append, derives the
/// listing from it. Not a recording spy: it answers reads back, which is
/// what the recovery-pass logic tests need it for.
#[derive(Debug, Default)]
pub struct RecordingConversations {
    convos: Mutex<BTreeMap<String, ConvoRow>>,
    msgs: Mutex<Vec<Message>>,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl RecordingConversations {
    fn row(&self, id: &str) -> Option<ConvoRow> {
        self.convos
            .lock()
            .unwrap()
            .get(id)
            .filter(|r| !r.deleted)
            .cloned()
    }

    fn convo_shell(&self, id: &str, row: ConvoRow) -> Conversation {
        Conversation {
            id: id.to_string(),
            title: row.title,
            messages: Vec::new(),
            created_at: row.created_at,
            updated_at: row.updated_at,
            version: 0,
            deleted_at: None,
            skill_id: row.skill_id,
            enabled_corpora: row.enabled_corpora,
            searched_sources: None,
        }
    }

    /// The listing rows that survive the tombstone filter, newest-first.
    fn live_rows_newest_first(&self) -> Vec<(String, ConvoRow)> {
        let convos = self.convos.lock().unwrap();
        let mut rows: Vec<(String, ConvoRow)> = convos
            .iter()
            .filter(|(_, r)| !r.deleted)
            .map(|(id, r)| (id.clone(), r.clone()))
            .collect();
        rows.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at));
        rows
    }
}

#[async_trait]
impl ConversationStore for RecordingConversations {
    async fn save_message(&self, msg: &Message) -> Result<()> {
        let mut convos = self.convos.lock().unwrap();
        convos
            .entry(msg.conversation_id.clone())
            .and_modify(|r| {
                r.updated_at = r.updated_at.max(msg.created_at);
            })
            .or_insert_with(|| ConvoRow {
                title: None,
                created_at: msg.created_at,
                updated_at: msg.created_at,
                skill_id: None,
                enabled_corpora: None,
                deleted: false,
            });
        self.msgs.lock().unwrap().push(msg.clone());
        Ok(())
    }

    async fn get_conversation(&self, id: &str) -> Result<Conversation> {
        let row = self
            .row(id)
            .ok_or_else(|| Error::NotFound(format!("conversation {id}")))?;
        let mut msgs: Vec<Message> = self
            .msgs
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.conversation_id == id)
            .cloned()
            .collect();
        // Stable sort: equal `created_at` keeps append order, like the
        // store's rowid ordering.
        msgs.sort_by_key(|m| m.created_at);
        let mut convo = self.convo_shell(id, row);
        convo.messages = msgs;
        Ok(convo)
    }

    async fn list_conversations(&self, limit: usize, offset: usize) -> Result<Vec<Conversation>> {
        Ok(self
            .live_rows_newest_first()
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(id, row)| self.convo_shell(&id, row))
            .collect())
    }

    async fn list_conversations_for_surface(
        &self,
        surface_skill_id: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Conversation>> {
        Ok(self
            .live_rows_newest_first()
            .into_iter()
            .filter(|(_, r)| match (&r.skill_id, surface_skill_id) {
                (None, None) => true,
                (Some(k), Some(s)) => k == s,
                _ => false,
            })
            .skip(offset)
            .take(limit)
            .map(|(id, row)| self.convo_shell(&id, row))
            .collect())
    }

    async fn list_conversations_for_corpus(
        &self,
        corpus_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Conversation>> {
        Ok(self
            .live_rows_newest_first()
            .into_iter()
            .filter(|(_, r)| {
                // Everything-scoped (`enabled_corpora IS NULL`) is excluded —
                // the store's contract, so a notebook shows only threads the
                // user actually had while scoped to it.
                r.enabled_corpora
                    .as_ref()
                    .is_some_and(|v| v.iter().any(|c| c == corpus_id))
            })
            .skip(offset)
            .take(limit)
            .map(|(id, row)| self.convo_shell(&id, row))
            .collect())
    }

    async fn search_messages(&self, query: &str) -> Result<Vec<Message>> {
        Ok(self
            .msgs
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.content.contains(query))
            .cloned()
            .collect())
    }

    async fn delete_conversation(&self, id: &str) -> Result<()> {
        let mut convos = self.convos.lock().unwrap();
        let row = convos
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(format!("conversation {id}")))?;
        row.deleted = true;
        Ok(())
    }

    async fn update_conversation_title(&self, id: &str, title: &str) -> Result<()> {
        let mut convos = self.convos.lock().unwrap();
        let row = convos
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(format!("conversation {id}")))?;
        row.title = Some(title.to_string());
        row.updated_at = now_unix();
        Ok(())
    }

    async fn insert_empty_conversation(
        &self,
        id: &str,
        created_at: i64,
        surface_skill_id: Option<&str>,
    ) -> Result<()> {
        let mut convos = self.convos.lock().unwrap();
        convos.entry(id.to_string()).or_insert_with(|| ConvoRow {
            title: None,
            created_at,
            updated_at: created_at,
            skill_id: surface_skill_id.map(str::to_string),
            enabled_corpora: None,
            deleted: false,
        });
        Ok(())
    }
}
