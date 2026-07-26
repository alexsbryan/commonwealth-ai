// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ConversationStore` impl — messages, conversations, titles,
//! skills, enabled corpora, searched sources.

use super::*;

#[async_trait]
impl ConversationStore for SqliteStateStore {
    async fn save_message(&self, msg: &Message) -> Result<()> {
        {
            let conn = self.conn.lock().await;

            // Upsert conversation.
            conn.execute(
                "INSERT INTO conversations (id, title, created_at, updated_at)
                 VALUES (?1, NULL, ?2, ?2)
                 ON CONFLICT(id) DO UPDATE SET updated_at = ?2",
                rusqlite::params![msg.conversation_id, now()],
            )
            .map_err(map_db)?;

            let metadata_json = msg
                .metadata
                .as_ref()
                .map(|m| serde_json::to_string(m).unwrap_or_default());

            let role_str = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
            };

            conn.execute(
                "INSERT OR REPLACE INTO messages (id, conversation_id, role, content, created_at, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    msg.id,
                    msg.conversation_id,
                    role_str,
                    msg.content,
                    msg.created_at,
                    metadata_json,
                ],
            )
            .map_err(map_db)?;
        }
        // Post-commit observer notification. Lock dropped above so the
        // observer cannot deadlock on a store read from inside its handler.
        self.fire_observer(|o| o.on_message_written(&msg.conversation_id));
        Ok(())
    }

    async fn get_conversation(&self, id: &str) -> Result<Conversation> {
        let conn = self.conn.lock().await;

        let (title, created_at, updated_at, skill_id, enabled_corpora_json, searched_sources_json) = conn
            .query_row(
                "SELECT title, created_at, updated_at, skill_id, enabled_corpora, searched_sources FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::NotFound(format!("Conversation {id}"))
                }
                other => map_db(other),
            })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, conversation_id, role, content, created_at, metadata
                 FROM messages WHERE conversation_id = ?1 ORDER BY created_at ASC",
            )
            .map_err(map_db)?;

        let messages: Vec<Message> = stmt
            .query_map(rusqlite::params![id], |row| {
                let role_str: String = row.get(2)?;
                let metadata_str: Option<String> = row.get(5)?;

                Ok(Message {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    role: match role_str.as_str() {
                        "assistant" => Role::Assistant,
                        "system" => Role::System,
                        _ => Role::User,
                    },
                    content: row.get(3)?,
                    created_at: row.get(4)?,
                    metadata: metadata_str.and_then(|s| serde_json::from_str(&s).ok()),
                    version: 0,
                })
            })
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(Conversation {
            id: id.to_string(),
            title,
            messages,
            created_at,
            updated_at,
            version: 0,
            deleted_at: None,
            skill_id,
            enabled_corpora: enabled_corpora_json
                .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok()),
            searched_sources: searched_sources_json.and_then(|s| {
                serde_json::from_str::<Vec<sovereign_core::types::SearchedSourceEntry>>(&s).ok()
            }),
        })
    }

    async fn list_conversations(&self, limit: usize, offset: usize) -> Result<Vec<Conversation>> {
        let conn = self.conn.lock().await;

        let mut stmt = conn
            .prepare(
                "SELECT id, title, created_at, updated_at, skill_id, enabled_corpora, searched_sources
                 FROM conversations
                 WHERE deleted_at IS NULL
                   AND (skill_id IS NULL OR skill_id != 'inner-work')
                 ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2",
            )
            .map_err(map_db)?;

        let convos: Vec<Conversation> = stmt
            .query_map(rusqlite::params![limit as i64, offset as i64], |row| {
                let enabled_corpora_json: Option<String> = row.get(5)?;
                let searched_sources_json: Option<String> = row.get(6)?;
                Ok(Conversation {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    messages: Vec::new(), // Not loading messages for list view.
                    created_at: row.get(2)?,
                    updated_at: row.get(3)?,
                    version: 0,
                    deleted_at: None,
                    skill_id: row.get(4)?,
                    enabled_corpora: enabled_corpora_json
                        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok()),
                    searched_sources: searched_sources_json.and_then(|s| {
                        serde_json::from_str::<Vec<sovereign_core::types::SearchedSourceEntry>>(&s)
                            .ok()
                    }),
                })
            })
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(convos)
    }

    async fn search_messages(&self, query: &str) -> Result<Vec<Message>> {
        let conn = self.conn.lock().await;

        // Sanitize the natural-language query into an FTS5-safe expression. The
        // raw query was bound straight into `MATCH ?1`, so operators and column
        // syntax in ordinary user text produced hard errors — "corpus-engine"
        // parsed as `corpus NOT engine`, "research AND methodology" as a bare
        // AND, a trailing word read as a column ("no such column: reference").
        // Reuse the SAME sanitizer the memory + document searches already use
        // (strip punctuation/stopwords, OR the remaining terms). Empty after
        // sanitisation → nothing searchable, so skip the MATCH (an empty FTS5
        // query is itself a syntax error).
        let fts_query = sanitize_fts5_query(query);
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }

        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.conversation_id, m.role, m.content, m.created_at, m.metadata
                 FROM messages m
                 JOIN messages_fts fts ON m.rowid = fts.rowid
                 WHERE messages_fts MATCH ?1
                 ORDER BY m.created_at DESC
                 LIMIT 50",
            )
            .map_err(map_db)?;

        let messages: Vec<Message> = stmt
            .query_map(rusqlite::params![fts_query], |row| {
                let role_str: String = row.get(2)?;
                let metadata_str: Option<String> = row.get(5)?;

                Ok(Message {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    role: match role_str.as_str() {
                        "assistant" => Role::Assistant,
                        "system" => Role::System,
                        _ => Role::User,
                    },
                    content: row.get(3)?,
                    created_at: row.get(4)?,
                    metadata: metadata_str.and_then(|s| serde_json::from_str(&s).ok()),
                    version: 0,
                })
            })
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db)?;

        Ok(messages)
    }

    async fn delete_conversation(&self, id: &str) -> Result<()> {
        {
            let conn = self.conn.lock().await;
            let ts = now();
            conn.execute(
                "UPDATE conversations SET deleted_at = ?2, version = ?2 WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![id, ts],
            )
            .map_err(map_db)?;
        }
        // Post-commit notification so the KnowledgeView conversational
        // acquirer can drop this conversation's chunks from its index.
        self.fire_observer(|o| o.on_conversation_deleted(id));
        Ok(())
    }

    async fn update_conversation_title(&self, id: &str, title: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        let ts = now();
        let rows = conn
            .execute(
                "UPDATE conversations SET title = ?2, updated_at = ?3, version = ?3 \
                 WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![id, title, ts],
            )
            .map_err(map_db)?;
        if rows == 0 {
            return Err(Error::NotFound(format!("conversation {id}")));
        }
        Ok(())
    }

    async fn set_conversation_skill_if_unset(
        &self,
        conversation_id: &str,
        skill_id: &str,
    ) -> Result<()> {
        // `WHERE skill_id IS NULL` guarantees idempotence: a later
        // skill activation cannot overwrite the first-message tag.
        // Silently a no-op when the conversation doesn't exist or
        // was already tagged — the Runtime calls this on every
        // message write for simplicity, relying on the constraint
        // to keep the first-writer-wins semantic.
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE conversations SET skill_id = ?2 \
             WHERE id = ?1 AND skill_id IS NULL",
            rusqlite::params![conversation_id, skill_id],
        )
        .map_err(map_db)?;
        Ok(())
    }

    async fn get_conversation_frame(&self, conversation_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        // `query_row` + OptionalExtension: a missing conversation and a
        // NULL frame are the same answer here ("no frame yet"), and a
        // compaction path must never fail a turn over it.
        let frame: Option<Option<String>> = conn
            .query_row(
                "SELECT frame FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![conversation_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_db)?;
        Ok(frame.flatten())
    }

    async fn set_conversation_frame(&self, conversation_id: &str, frame: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE conversations SET frame = ?2 \
             WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![conversation_id, frame],
        )
        .map_err(map_db)?;
        // Deliberately NOT an error on 0 rows, unlike
        // `set_conversation_enabled_corpora`: that setter serves a user
        // action that should fail loudly, while this one is background
        // housekeeping on a conversation that may have been deleted
        // mid-turn. Losing a frame write is recoverable (one cold fold);
        // failing the user's turn over it is not.
        Ok(())
    }

    async fn set_conversation_enabled_corpora(
        &self,
        conversation_id: &str,
        enabled_corpora: Option<Vec<String>>,
    ) -> Result<()> {
        let encoded = match enabled_corpora {
            Some(ref ids) => Some(
                serde_json::to_string(ids)
                    .map_err(|e| Error::Storage(format!("encode enabled_corpora: {e}")))?,
            ),
            None => None,
        };
        let conn = self.conn.lock().await;
        let rows = conn
            .execute(
                "UPDATE conversations SET enabled_corpora = ?2 \
                 WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![conversation_id, encoded],
            )
            .map_err(map_db)?;
        if rows == 0 {
            return Err(Error::NotFound(format!("conversation {conversation_id}")));
        }
        Ok(())
    }

    async fn set_conversation_searched_sources(
        &self,
        conversation_id: &str,
        entries: Option<Vec<sovereign_core::types::SearchedSourceEntry>>,
    ) -> Result<()> {
        let encoded = match entries {
            Some(ref es) => Some(
                serde_json::to_string(es)
                    .map_err(|e| Error::Storage(format!("encode searched_sources: {e}")))?,
            ),
            None => None,
        };
        let conn = self.conn.lock().await;
        let rows = conn
            .execute(
                "UPDATE conversations SET searched_sources = ?2 \
                 WHERE id = ?1 AND deleted_at IS NULL",
                rusqlite::params![conversation_id, encoded],
            )
            .map_err(map_db)?;
        if rows == 0 {
            return Err(Error::NotFound(format!("conversation {conversation_id}")));
        }
        Ok(())
    }

    async fn insert_empty_conversation(
        &self,
        id: &str,
        created_at: i64,
        surface_skill_id: Option<&str>,
    ) -> Result<()> {
        // Delegate to the inherent method so the INSERT OR IGNORE SQL
        // has a single source of truth. Explicit path avoids resolving
        // back to this trait method.
        SqliteStateStore::insert_empty_conversation(self, id, created_at, surface_skill_id).await
    }
}
