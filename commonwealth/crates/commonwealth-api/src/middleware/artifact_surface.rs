// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ArtifactSurface` — the post-path middleware that notices what
//! the model did this turn and stages a breadcrumb for the next
//! turn's preamble.
//!
//! Between turn N and turn N+1, the agent may have:
//! - written notes (decision/invariant/attempt/uncertainty/…);
//! - completed a milestone (stop_passed flipped true on a run);
//! - had its spec drift auto-flagged (deviation note inserted).
//!
//! All three land in SQLite (corpus-engine) independently of the
//! `/v1/chat/completions` request/response pair. The model doesn't
//! volunteer "I wrote 3 notes and passed milestone 2" — it just
//! calls tools. `ArtifactSurface.post_process` scans the tables
//! for changes since `session.last_seen_at` and stashes an
//! [`ArtifactDelta`] on the session so
//! [`ContextInjector`](super::context_injector) can render it as
//! the "Since last turn" section of the next turn's preamble.
//!
//! Design choice: we read from the DB rather than from the
//! response content. Tool-call results in the response body are an
//! unreliable signal — the model might not mention a note it
//! wrote, or it might summarize incorrectly. The DB is
//! authoritative by construction.

use std::path::Path;

use async_trait::async_trait;
use corpus_engine_notes::{NoteStore, ScopeFilter};

use sovereign_atos::session::{ArtifactDelta, MilestonePassEvent};

use super::{Middleware, MiddlewareError, MiddlewareSession, PipelineContext, ResponseView};
use crate::openai_types::ChatCompletionRequest;

/// Per-kind cap on `recent_note_ids` so the delta stays small even
/// when a turn produces hundreds of notes (unlikely, but bound it).
const RECENT_NOTES_CAP: usize = 5;

pub struct ArtifactSurface;

impl ArtifactSurface {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ArtifactSurface {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for ArtifactSurface {
    fn id(&self) -> &'static str {
        "artifact_surface"
    }

    /// No-op on pre-path. All work happens in `post_process` so
    /// deltas reflect what the turn ACTUALLY did, not what it was
    /// about to do.
    async fn process(
        &self,
        _request: &mut ChatCompletionRequest,
        _session: &mut MiddlewareSession,
        _ctx: &PipelineContext,
    ) -> Result<(), MiddlewareError> {
        Ok(())
    }

    async fn post_process(
        &self,
        _response: &ResponseView<'_>,
        session: &mut MiddlewareSession,
        ctx: &PipelineContext,
    ) -> Result<(), MiddlewareError> {
        let Some(feature_id) = ctx.feature_id.clone() else {
            return Ok(());
        };
        let since = session.last_seen_at;
        let delta = compute_delta(&ctx.repo_root, &feature_id, since).await;
        if is_empty_delta(&delta) {
            // Nothing to surface — don't overwrite a pre-existing
            // delta that ContextInjector hasn't drained yet (e.g.,
            // two post-processes fire in rapid succession).
            return Ok(());
        }
        session.pending_artifact_delta = Some(delta);
        Ok(())
    }
}

fn is_empty_delta(delta: &ArtifactDelta) -> bool {
    delta.notes_by_kind.is_empty() && delta.milestones_passed.is_empty()
}

async fn compute_delta(repo_root: &Path, feature_id: &str, since: i64) -> ArtifactDelta {
    let mut delta = ArtifactDelta::default();

    // ── Notes written since `since` ──────────────────────────────
    let notes_db = repo_root.join(".sovereign").join("notes.db");
    if let Ok(store) = NoteStore::open(&notes_db) {
        let filter = ScopeFilter {
            scopes: vec![
                corpus_engine_notes::NoteScope::Feature,
                corpus_engine_notes::NoteScope::Global,
            ],
            feature_id: Some(feature_id.to_string()),
        };
        // Over-fetch by recency and post-filter on created_at. The
        // notes store doesn't expose a since-timestamp query today;
        // 200 is generous for a single turn.
        if let Ok(rows) = store
            .read_notes_scoped(None, &[], &[], &[], 200, false, &filter)
            .await
        {
            for n in rows {
                let Some(ts) = rfc3339_to_unix(&n.created_at) else {
                    continue;
                };
                if ts <= since {
                    continue;
                }
                *delta.notes_by_kind.entry(n.kind.clone()).or_insert(0) += 1;
                let ids = delta
                    .recent_note_ids
                    .entry(n.kind.clone())
                    .or_insert_with(Vec::new);
                if ids.len() < RECENT_NOTES_CAP {
                    ids.push(n.id);
                }
            }
        }
    }

    // ── Milestones that flipped to stop_passed ───────────────────
    let features_db = repo_root.join(".sovereign").join("features.db");
    if let Ok(store) = corpus_engine_atos::FeatureStore::open(&features_db) {
        if let Ok(runs) = store.list_runs_for_feature(feature_id).await {
            let milestones = store.list_milestones(feature_id).await.unwrap_or_default();
            for run in runs {
                if run.stop_passed != Some(true) {
                    continue;
                }
                if run.mode != "normal" {
                    // Red-team passes don't count as milestone
                    // progress — by design, M3.
                    continue;
                }
                let Some(ended_at) = run.ended_at else {
                    continue;
                };
                if ended_at <= since {
                    continue;
                }
                let Some(m) = milestones.iter().find(|m| m.id == run.milestone_id) else {
                    continue;
                };
                let artifact = format!(
                    ".sovereign/features/{}/milestone-{}.md",
                    feature_id, m.ordinal
                );
                delta.milestones_passed.push(MilestonePassEvent {
                    feature_id: feature_id.to_string(),
                    ordinal: m.ordinal,
                    artifact_path: artifact,
                });
            }
        }
    }

    delta
}

/// Parse an RFC 3339 timestamp to Unix seconds without pulling in
/// chrono. NoteStore's writer produces times like
/// `2026-04-20T08:52:11+00:00`. The parser here only handles that
/// shape + close variants; malformed input returns None.
fn rfc3339_to_unix(s: &str) -> Option<i64> {
    // Cheap parser: year-month-dayThh:mm:ss[.frac][±hh:mm|Z]
    // We reject anything we can't confidently parse.
    let (date, rest) = s.split_once('T')?;
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    let time_end = rest.find(['Z', '+', '-']).unwrap_or(rest.len());
    let time = &rest[..time_end];
    let mut tparts = time.split(':');
    let hh: i64 = tparts.next()?.parse().ok()?;
    let mm: i64 = tparts.next()?.parse().ok()?;
    // Seconds may have fractional part; take integer portion.
    let sec_str = tparts.next()?;
    let ss: i64 = sec_str.split('.').next()?.parse().ok()?;

    // Days-from-civil epoch (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_parses_utc() {
        // 2026-04-20T08:52:11Z. Check via round-trip against the
        // fixed reference value: 1776675131 = seconds since epoch
        // for that datetime in UTC.
        let ts = rfc3339_to_unix("2026-04-20T08:52:11+00:00").unwrap();
        assert_eq!(ts, 1_776_675_131);
    }

    #[test]
    fn rfc3339_parses_z_suffix() {
        let ts = rfc3339_to_unix("2026-04-20T00:00:00Z").unwrap();
        // Not doing manual verification of the unix value — just
        // making sure it parses without panicking and is a
        // reasonable > 0 value.
        assert!(ts > 0);
    }

    #[test]
    fn rfc3339_malformed_returns_none() {
        assert!(rfc3339_to_unix("not a timestamp").is_none());
        assert!(rfc3339_to_unix("2026-04-20").is_none());
    }

    #[tokio::test]
    async fn no_feature_id_is_noop() {
        let surface = ArtifactSurface::new();
        let mut session = MiddlewareSession::default();
        let ctx = PipelineContext {
            pipeline_name: "test".into(),
            model_id: "m".into(),
            context_config: Default::default(),
            feature_id: None,
            session_id: Some("s".into()),
            repo_root: std::env::temp_dir(),
        };
        let view = ResponseView {
            content: "",
            finish_reason: Some("stop"),
            tool_calls_emitted: 0,
        };
        surface
            .post_process(&view, &mut session, &ctx)
            .await
            .unwrap();
        assert!(session.pending_artifact_delta.is_none());
    }

    #[tokio::test]
    async fn delta_surfaces_notes_written_since_last_turn() {
        // Build a scratch .sovereign/ with a notes.db seeded with
        // two notes — one before the cutoff, one after.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        std::fs::create_dir_all(repo.join(".sovereign")).unwrap();
        let notes_db = repo.join(".sovereign").join("notes.db");
        let store = NoteStore::open(&notes_db).unwrap();

        // Seed a note FIRST with current timestamp, then set
        // `since = now - 1` so the seed counts as "after cutoff".
        let id = store
            .write_note_scoped(
                "uncertainty",
                "post-cutoff note",
                vec![],
                vec![],
                "test",
                corpus_engine_notes::NoteScope::Feature,
                Some("fx"),
            )
            .await
            .unwrap();

        // Wait a second so last_seen_at is reliably before the note.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let since = now - 10;

        let surface = ArtifactSurface::new();
        let mut session = MiddlewareSession {
            last_seen_at: since,
            ..Default::default()
        };
        let ctx = PipelineContext {
            pipeline_name: "test".into(),
            model_id: "m".into(),
            context_config: Default::default(),
            feature_id: Some("fx".into()),
            session_id: Some("s".into()),
            repo_root: repo.to_path_buf(),
        };
        let view = ResponseView {
            content: "",
            finish_reason: Some("stop"),
            tool_calls_emitted: 0,
        };
        surface
            .post_process(&view, &mut session, &ctx)
            .await
            .unwrap();

        let delta = session.pending_artifact_delta.expect("delta staged");
        assert_eq!(delta.notes_by_kind.get("uncertainty"), Some(&1));
        assert_eq!(delta.recent_note_ids.get("uncertainty").unwrap()[0], id);
    }
}
