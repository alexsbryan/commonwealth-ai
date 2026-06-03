//! Corpus commands — list installed CORPUS_REFs and resolve a citation
//! to its snippet (the `(corpus_id, chunk_id)` handle, §4).

use tauri::State;

use crate::cache::store as cache;
use crate::error::Result;
use crate::remote::dto::CorpusRefDto;
use crate::state::AppState;

#[tauri::command]
pub async fn list_corpora(state: State<'_, AppState>) -> Result<Vec<CorpusRefDto>> {
    let host = state.active_host()?;
    if let Ok(client) = state.active_client() {
        if let Ok(refs) = client.list_corpora().await {
            if let Ok(mut conn) = state.db.lock() {
                let _ = cache::replace_corpus_refs(&mut conn, &host.id, &refs);
            }
            return Ok(refs);
        }
    }
    // Offline → cached corpus refs.
    let conn = state.db.lock().map_err(|_| crate::error::Error::Other("db poisoned".into()))?;
    let mut stmt = conn.prepare(
        "SELECT corpus_id, display_name, category, icon, chunk_count, scope, mesh_shared
         FROM corpus_ref WHERE host_connection_id = ?1 ORDER BY display_name ASC",
    )?;
    let refs = stmt
        .query_map(rusqlite::params![host.id], |r| {
            Ok(CorpusRefDto {
                corpus_id: r.get(0)?,
                display_name: r.get(1)?,
                category: r.get(2)?,
                icon: r.get(3)?,
                chunk_count: r.get(4)?,
                scope: r.get(5)?,
                mesh_shared: r.get::<_, i64>(6)? != 0,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(refs)
}

/// Resolve a tapped citation to its snippet, from cache. The cached
/// `corpus_id` matching a `corpus_ref` for the active host is what
/// proves the answer was grounded in an installed corpus.
#[tauri::command]
pub async fn resolve_citation(
    state: State<'_, AppState>,
    corpus_id: String,
    chunk_id: String,
) -> Result<Option<String>> {
    let conn = state.db.lock().map_err(|_| crate::error::Error::Other("db poisoned".into()))?;
    cache::citation_snippet(&conn, &corpus_id, &chunk_id)
}
