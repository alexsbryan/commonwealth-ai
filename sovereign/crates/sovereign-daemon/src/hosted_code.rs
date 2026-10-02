// SPDX-License-Identifier: AGPL-3.0-or-later
//! The code program, composed into this process by a distribution (phase-b
//! pb-code-daemon-exit; F2 (a), phase-b-30; phase-b-33). svrn names no type
//! of code's: the distribution builds code through code's face and hands
//! back what svrn mounts — code's tools on the one `/mcp`, code's routes on
//! the client surface, and a handle that keeps code's runtime alive. svrn
//! alone (no [`HostedCode`]) serves no code tool; its `/mcp` points a caller
//! at `svrn code mcp` and `/v1/projects/*` answers the same absence.

use std::any::Any;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use crate::posture::Posture;

/// What svrn hands code's composition: its data root (code opens its own
/// `notes.db` there, pb-notes-memory), the workspace it was told to watch,
/// the chunk index svrn holds for that root, and the inputs of code's notes
/// rail.
pub struct CodeHost {
    /// svrn's data root; the distribution places code's data under it.
    pub data_dir: PathBuf,
    /// The workspace svrn resolved (`SOVEREIGN_WORKSPACE_DIR`, then
    /// `~/.svrnmesh/workspace`), `None` when there is none.
    pub workspace: Option<PathBuf>,
    /// The root's chunk indexes, read through the engine svrn holds.
    pub index: Arc<dyn corpus_index::source::IndexSource>,
    /// What svrn alone has for code's note store (pb-notes-memory): its embed
    /// slot (T1) and GLiNER session (T2), this node's id and mesh roster,
    /// and the convergence recorder `/status` reads.
    pub notes_embed: corpus_index::types::EmbedFn,
    /// `None` when no GLiNER session loaded.
    pub notes_gliner: Option<corpus_index::types::GlinerFn>,
    /// This node's id, stamped on outbound notes.
    pub node_id: kernel_types::NodeId,
    /// `None` on a solo node or an unreadable `mesh.json`.
    pub roster: Option<corpus_index::types::NodeRoster>,
    /// Stamped by the notes rail's publish and ingest.
    pub convergence: Arc<dyn sovereign_contracts::peer::Convergence>,
}

/// Code, composed, as svrn mounts it.
pub struct CodeMount {
    /// Code's tools, listed and called on svrn's `/mcp` beside svrn's own.
    pub tools: Arc<dyn host_kit::mcp::McpMountedTools>,
    /// Code's client routes (`/v1/projects/*`, and `/v1/solve/jobs*` since
    /// pb-meshapp-solve).
    pub routes: axum::Router,
    /// Code's editor door (`/v1/edit_predictions` and its outcome route),
    /// mounted on every surface that serves the general client routes.
    pub edit_routes: axum::Router,
    /// Hands code's lint/test watchers svrn's foreground signal.
    pub yield_to: Box<dyn Fn(Arc<dyn corpus_engine_yield::YieldHook>) + Send + Sync>,
    /// Keeps code's runtime (Reindexer, watchers, atlas GC) alive for the
    /// process's life.
    pub hold: Box<dyn Any + Send + Sync>,
}

type Compose = Box<
    dyn FnOnce(CodeHost) -> Pin<Box<dyn Future<Output = Result<CodeMount, String>> + Send>> + Send,
>;

/// The distribution's composition of code, run once at boot.
pub struct HostedCode {
    compose: Compose,
}

impl HostedCode {
    /// `compose` builds code for `CodeHost`; an `Err` names why, and refuses
    /// boot.
    pub fn new<F, Fut>(compose: F) -> Self
    where
        F: FnOnce(CodeHost) -> Fut + Send + 'static,
        Fut: Future<Output = Result<CodeMount, String>> + Send + 'static,
    {
        Self {
            compose: Box::new(move |host| Box::pin(compose(host))),
        }
    }

    /// Run the composition.
    pub async fn compose(self, host: CodeHost) -> Result<CodeMount, String> {
        (self.compose)(host).await
    }
}

/// Where svrn alone points a caller that asked for code: the one server
/// that serves code's tools and `/v1/projects/*`.
pub const CODE_SERVER: &str = "svrn code mcp";

/// A route of code's on svrn alone: a 503 naming the code program, never a
/// 404 that reads as "no such route" (FIVE_PROGRAMS §4 rule 3), and pointing
/// where `posture` says: `svrn code mcp`, or what a sealed box does instead.
fn absent(posture: Posture) -> axum::routing::MethodRouter {
    axum::routing::any(move |uri: axum::http::Uri| async move {
        use axum::response::IntoResponse;
        tracing::debug!(path = %uri.path(), ?posture, "code routes: no code program in this process");
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({
                "error": format!(
                    "{} is served by the code program, which this svrn does not host: {}",
                    uri.path(),
                    posture.code_pointer()
                ),
            })),
        )
            .into_response()
    })
}

/// `/v1/projects/*` on svrn alone.
pub fn projects_absent_router(posture: Posture) -> axum::Router {
    axum::Router::new()
        .route("/v1/projects", absent(posture))
        .route("/v1/projects/{*rest}", absent(posture))
}

/// The solver's job routes on svrn alone (pb-meshapp-solve):
/// `/v1/solve/jobs` and everything under it.
pub fn solve_absent_router(posture: Posture) -> axum::Router {
    axum::Router::new()
        .route("/v1/solve/jobs", absent(posture))
        .route("/v1/solve/jobs/{*rest}", absent(posture))
}

/// Code's editor door on svrn alone (pb-meshapp-rest): `/v1/edit_predictions`
/// and its outcome route.
pub fn edit_door_absent_router(posture: Posture) -> axum::Router {
    axum::Router::new()
        .route("/v1/edit_predictions", absent(posture))
        .route("/v1/edit_predictions/outcome", absent(posture))
}
