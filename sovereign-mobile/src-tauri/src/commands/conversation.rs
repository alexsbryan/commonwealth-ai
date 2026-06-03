//! Conversation CRUD — cache-first, then reconcile against the host.
//!
//! Reads return the local cache immediately so the app is usable
//! offline and instant on relaunch (§8, §9); when the host is
//! reachable we refetch and reconcile. Mirrors the desktop `api.ts`
//! command names so the shared UI is unchanged.

use tauri::State;

use crate::cache::store as cache;
use crate::error::Result;
use crate::remote::dto::ConversationDto;
use crate::state::AppState;

#[tauri::command]
pub async fn create_conversation(state: State<'_, AppState>) -> Result<String> {
    let client = state.active_client()?;
    client.create_conversation().await
}

#[tauri::command]
pub async fn list_conversations(state: State<'_, AppState>) -> Result<Vec<ConversationDto>> {
    let host = state.active_host()?;

    // Try the host; reconcile cache on success.
    if let Ok(client) = state.active_client() {
        if let Ok(remote) = client.list_conversations().await {
            if let Ok(conn) = state.db.lock() {
                for c in &remote {
                    let _ = cache::upsert_conversation(&conn, &host.id, c);
                }
            }
            return Ok(remote);
        }
    }
    // Offline / host down → serve cache.
    let conn = state.db.lock().map_err(|_| crate::error::Error::Other("db poisoned".into()))?;
    cache::read_conversations(&conn, &host.id)
}

#[tauri::command]
pub async fn get_conversation(
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<Option<ConversationDto>> {
    let host = state.active_host()?;

    if let Ok(client) = state.active_client() {
        if let Ok(remote) = client.get_conversation(&conversation_id).await {
            if let Ok(mut conn) = state.db.lock() {
                let _ = cache::upsert_conversation(&conn, &host.id, &remote);
                for m in &remote.messages {
                    let _ = cache::upsert_message_full(
                        &mut conn,
                        m,
                        m.provenance.as_ref(),
                        &m.citations,
                    );
                }
            }
            return Ok(Some(remote));
        }
    }
    let conn = state.db.lock().map_err(|_| crate::error::Error::Other("db poisoned".into()))?;
    cache::read_conversation(&conn, &conversation_id)
}

#[tauri::command]
pub async fn delete_conversation(state: State<'_, AppState>, conversation_id: String) -> Result<()> {
    if let Ok(client) = state.active_client() {
        let _ = client.delete_conversation(&conversation_id).await;
    }
    let conn = state.db.lock().map_err(|_| crate::error::Error::Other("db poisoned".into()))?;
    conn.execute(
        "DELETE FROM conversation WHERE id = ?1",
        rusqlite::params![conversation_id],
    )?;
    Ok(())
}
