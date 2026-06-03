//! CommissiveQuery dispatch — persists a commitment/todo note
//! anchored to the working-memory current_goal (or honestly anchorless
//! when none is loaded). Fast-slot acknowledgement only; no synthesis.



use crate::error::Result;

use super::super::*;

impl Runtime {
    /// Handle CommissiveQuery: persist a user commitment to the notes
    /// store anchored to the situated `working_memory.current_goal`
    /// (or honestly anchorless when no goal is loaded). The reply
    /// cites the situated anchor so the user knows where the
    /// commitment will surface.
    pub(crate) async fn handle_commissive_query(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
    ) -> Result<Response> {
        // Extract commitment phrase: text after the marker.
        let phrase = extract_commitment_phrase(message)
            .unwrap_or_else(|| message.trim().to_string());

        // Resolve situated anchor — current_goal is the strongest
        // signal; topic_context.topic is fallback; otherwise None.
        let related_entity: Option<String> = context
            .working_memory
            .as_ref()
            .and_then(|wm| wm.current_goal.clone())
            .or_else(|| {
                context
                    .topic_context
                    .as_ref()
                    .and_then(|tc| tc.topic.clone())
            });

        let lower = message.to_lowercase();
        let kind = if lower.contains("remind me") {
            "todo"
        } else {
            "commitment"
        };

        // No notes store wired — degrade honestly, do not silently drop.
        let Some(note_store) = self.note_store.as_ref() else {
            let reply = format!(
                "I'd save this commitment, but my notes store isn't wired in this build. \
                 The commitment was: \"{phrase}\". Run via the desktop or daemon to enable \
                 persistence."
            );
            let response_msg = Message {
                id: uuid::Uuid::new_v4().to_string(),
                conversation_id: conversation_id.to_string(),
                role: Role::Assistant,
                content: reply,
                created_at: now(),
                metadata: Some(serde_json::json!({
                    "intent": "CommissiveQuery",
                    "kind": kind,
                    "phrase": phrase,
                    "result_quality": "no_note_store",
                })),
                version: 0,
            };
            return Ok(Response { message: response_msg, task: None, metrics: None });
        };

        // Persist via existing NoteStore API. Defaults to
        // `NoteSource::Agent` — the agent is recording what the user
        // said about a future intention, which matches the agent-
        // observation semantic.
        let note_id = match note_store
            .write_note_with_relation(
                kind,
                &phrase,
                Vec::new(),
                Vec::new(),
                conversation_id,
                corpus_engine_notes::NoteScope::Session,
                None,
                related_entity.as_deref(),
            )
            .await
        {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(error = %e, "CommissiveQuery: note write failed");
                let reply = format!(
                    "I tried to save this commitment but the note store returned an error. \
                     Phrase: \"{phrase}\". Error: {e}"
                );
                let response_msg = Message {
                    id: uuid::Uuid::new_v4().to_string(),
                    conversation_id: conversation_id.to_string(),
                    role: Role::Assistant,
                    content: reply,
                    created_at: now(),
                    metadata: Some(serde_json::json!({
                        "intent": "CommissiveQuery",
                        "kind": kind,
                        "phrase": phrase,
                        "result_quality": "write_failed",
                    })),
                    version: 0,
                };
                return Ok(Response { message: response_msg, task: None, metrics: None });
            }
        };

        let anchor_phrase = related_entity
            .as_deref()
            .map(|s| format!("under {s}"))
            .unwrap_or_else(|| "to this conversation".to_string());
        let reply = format!(
            "Saved as a {kind} {anchor_phrase}. I'll surface it next time we touch that work.\n\n\
             (Note id: {note_id})"
        );
        let response_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: reply,
            created_at: now(),
            metadata: Some(serde_json::json!({
                "intent": "CommissiveQuery",
                "kind": kind,
                "phrase": phrase,
                "note_id": note_id,
                "related_entity": related_entity,
            })),
            version: 0,
        };
        Ok(Response { message: response_msg, task: None, metrics: None })
    }
}
