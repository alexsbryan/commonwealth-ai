// SPDX-License-Identifier: AGPL-3.0-or-later
//! `GET /oicp/v1/text/{text_sha256}?start=&end=&context=&corpus=` — a stored
//! text, read by its name (OICP v0.5 §2.2, ADDRESSED_TEXT §5.2).
//!
//! Ranges are half-open code points; with none the reply is the whole text.
//! Only the corpora the caller may read are consulted
//! ([`crate::oicp_evidence::read_scope`]), in `corpus_id` order, so the
//! record served is the lowest `(corpus_id, source.id, ordinal)` the caller
//! may read and a text held only outside that set is `text not held`. The
//! `corpus` hint names where the client found the name; when nothing the
//! caller may read holds it, that corpus's own absence (`texts not stored`)
//! is the reason given. Every refusal is named (§2.5).
//!
//! Mounted in `general` under `client_auth`, like every OICP route.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use corpus_index::index::{CorpusIndex, DocumentRecord};
use kernel_types::Sha256Hash;
use oicp_types::evidence::{is_sha256_hex, reasons, TextSlice, DEFAULT_CONTEXT};
use serde::Deserialize;
use sovereign_contracts::principal::AttachedPrincipal;

use crate::oicp_evidence::{absence_reason, read_scope, wire_document};
use crate::routes_internal::ErrorBody;
use crate::state::AppState;

/// Where `text_endpoint` is mounted; the manifest advertises this path.
pub const TEXT_ENDPOINT: &str = "/oicp/v1/text";

/// The read's query string.
#[derive(Debug, Default, Deserialize)]
pub struct TextQuery {
    /// First code point; `0` when absent.
    pub start: Option<u64>,
    /// One past the last code point; the text's length when absent.
    pub end: Option<u64>,
    /// Code points of `before`/`after` around a range.
    pub context: Option<u32>,
    /// The corpus the client found the name in.
    pub corpus: Option<String>,
}

fn refuse(status: StatusCode, error: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorBody {
            error: error.into(),
        }),
    )
        .into_response()
}

/// `GET {text_endpoint}/{text_sha256}`.
pub async fn text(
    State(state): State<AppState>,
    attached: Option<Extension<AttachedPrincipal>>,
    Path(name): Path<String>,
    Query(query): Query<TextQuery>,
) -> Response {
    if !is_sha256_hex(&name) {
        tracing::debug!(%name, "text read: not a text name");
        return refuse(
            StatusCode::BAD_REQUEST,
            format!("`{name}` is not a text name (64 lowercase hex)"),
        );
    }
    let Some(sha) = Sha256Hash::from_hex(&name) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            format!("`{name}` is not a text name"),
        );
    };
    let Some(engine) = state.inner.node.corpus_engine.clone() else {
        tracing::info!(%name, "text read: no corpus engine on this node");
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            crate::hosted_ingest::NO_INGEST,
        );
    };
    let principal = attached.as_ref().map(|Extension(AttachedPrincipal(p))| p);
    let scope = read_scope(&state, principal);
    let installed = match engine.installed_indexes().await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(error = %e, "text read: installed corpora unread");
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("the installed corpora could not be read: {e}"),
            );
        }
    };
    let mut corpora: Vec<String> = installed
        .into_iter()
        .map(|i| i.corpus_id)
        .filter(|c| scope.admits(c))
        .collect();
    corpora.sort();
    corpora.dedup();

    let found = match find(&engine, &corpora, &sha, query.corpus.as_deref()).await {
        Ok(f) => f,
        Err(refusal) => return refusal,
    };
    let Found {
        corpus_id,
        document,
        text,
    } = found;
    let document = match wire_document(&document) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(%name, corpus = %corpus_id, error = %e, "text read: record not served");
            return refuse(StatusCode::INTERNAL_SERVER_ERROR, e);
        }
    };
    let slice = match slice(&text, &query) {
        Some(s) => s,
        None => {
            tracing::debug!(%name, start = ?query.start, end = ?query.end, "text read: range outside text");
            return refuse(StatusCode::BAD_REQUEST, reasons::RANGE_OUTSIDE_TEXT);
        }
    };
    tracing::debug!(
        %name,
        corpus = %corpus_id,
        source = %document.source.id,
        start = slice.0,
        end = slice.1,
        "text read: served"
    );
    let (start, end, text, before, after) = slice;
    Json(TextSlice {
        document,
        start,
        end,
        text,
        before,
        after,
    })
    .into_response()
}

/// The lowest record the caller may read for a name, and the text's bytes.
struct Found {
    corpus_id: String,
    document: DocumentRecord,
    text: String,
}

/// Consult `corpora` (sorted, all readable by the caller) for `name`: the
/// first that holds it has the lowest record, and its text comes from the
/// first holder that stored it.
async fn find(
    engine: &std::sync::Arc<dyn corpus_index::ingest_port::daemon::IngestPort>,
    corpora: &[String],
    name: &Sha256Hash,
    hint: Option<&str>,
) -> Result<Found, Response> {
    let mut chosen: Option<(String, DocumentRecord)> = None;
    let mut hinted_absence = None;
    let mut unread: Vec<String> = Vec::new();
    for corpus_id in corpora {
        let index = match engine.open_index_for_corpus(corpus_id).await {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(corpus = %corpus_id, error = %e, "text read: corpus did not open");
                unread.push(corpus_id.clone());
                continue;
            }
        };
        let records = match index.documents_for(&[*name]).await {
            Ok(Ok(mut by_name)) => by_name.remove(name).unwrap_or_default(),
            Ok(Err(absence)) => {
                tracing::debug!(corpus = %corpus_id, reason = absence_reason(absence), "text read: corpus cannot answer");
                if Some(corpus_id.as_str()) == hint {
                    hinted_absence = Some(absence);
                }
                continue;
            }
            Err(e) => {
                tracing::warn!(corpus = %corpus_id, error = %e, "text read: records unread");
                unread.push(corpus_id.clone());
                continue;
            }
        };
        if chosen.is_none() {
            if let Some(lowest) = records
                .iter()
                .min_by(|a, b| (&a.source_id, a.ordinal).cmp(&(&b.source_id, b.ordinal)))
            {
                chosen = Some((corpus_id.clone(), lowest.clone()));
            }
        }
        if !records.iter().any(|r| r.text_stored) {
            continue;
        }
        if let Some((corpus_id, document)) = chosen.take() {
            if !unread.is_empty() {
                tracing::warn!(
                    ?unread,
                    "text read: served, though corpora ordered before the holder did not answer"
                );
            }
            return read_text(&index, name, corpus_id, document).await;
        }
    }
    if chosen.is_some() {
        tracing::debug!(%name, "text read: held, its text not stored");
        return Err(refuse(StatusCode::NOT_FOUND, reasons::TEXT_NOT_STORED));
    }
    if !unread.is_empty() {
        // "Did not answer" is not "not held" (ARCH 6).
        return Err(refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "not found in what answered; these corpora did not: {}",
                unread.join(", ")
            ),
        ));
    }
    let reason = hinted_absence.map_or(reasons::TEXT_NOT_HELD, absence_reason);
    tracing::debug!(%name, consulted = corpora.len(), reason, "text read: refused");
    Err(refuse(StatusCode::NOT_FOUND, reason))
}

async fn read_text(
    index: &CorpusIndex,
    name: &Sha256Hash,
    corpus_id: String,
    document: DocumentRecord,
) -> Result<Found, Response> {
    match index.text(name).await {
        Ok(Ok(stored)) => Ok(Found {
            corpus_id,
            document,
            text: stored.text,
        }),
        Ok(Err(absence)) => Err(refuse(StatusCode::NOT_FOUND, absence_reason(absence))),
        Err(e) => {
            tracing::error!(%name, error = %e, "text read: the stored text did not read back");
            Err(refuse(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        }
    }
}

/// `(start, end, text, before, after)` for `query` over `text`, in code
/// points, or `None` when the range falls outside it. No range is the whole
/// text with no context.
fn slice(text: &str, query: &TextQuery) -> Option<(u64, u64, String, String, String)> {
    let len = text.chars().count() as u64;
    let ranged = query.start.is_some() || query.end.is_some();
    let (start, end) = (query.start.unwrap_or(0), query.end.unwrap_or(len));
    if start > end || end > len {
        return None;
    }
    let cp = |a: u64, b: u64| -> Option<String> {
        let (a, b) = (usize::try_from(a).ok()?, usize::try_from(b).ok()?);
        quote_align::code_point_slice(text, a..b).map(str::to_string)
    };
    let (before, after) = if ranged {
        let c = u64::from(query.context.unwrap_or(DEFAULT_CONTEXT));
        (
            cp(start.saturating_sub(c), start)?,
            cp(end, end.saturating_add(c).min(len))?,
        )
    } else {
        (String::new(), String::new())
    };
    Some((start, end, cp(start, end)?, before, after))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(start: Option<u64>, end: Option<u64>, context: Option<u32>) -> TextQuery {
        TextQuery {
            start,
            end,
            context,
            corpus: None,
        }
    }

    #[test]
    fn ranges_are_code_points_with_context_either_side() {
        let t = "café — naïve text";
        let (s, e, x, b, a) = slice(t, &q(Some(5), Some(6), Some(3))).unwrap();
        assert_eq!((s, e, x.as_str()), (5, 6, "—"));
        assert_eq!((b.as_str(), a.as_str()), ("fé ", " na"));
        let (s, e, x, b, a) = slice(t, &q(None, None, Some(3))).unwrap();
        assert_eq!((s, e), (0, t.chars().count() as u64));
        assert_eq!(
            (x.as_str(), b.as_str(), a.as_str()),
            (t, "", ""),
            "whole, no context"
        );
        let (_, _, x, b, _) = slice(t, &q(Some(2), None, None)).unwrap();
        assert_eq!((x.as_str(), b.as_str()), ("fé — naïve text", "ca"));
    }

    #[test]
    fn a_range_past_the_end_or_reversed_is_outside() {
        let t = "abc";
        assert!(slice(t, &q(Some(0), Some(4), None)).is_none());
        assert!(slice(t, &q(Some(2), Some(1), None)).is_none());
        assert!(
            slice(t, &q(Some(3), Some(3), None)).is_some(),
            "empty at the end"
        );
    }
}
