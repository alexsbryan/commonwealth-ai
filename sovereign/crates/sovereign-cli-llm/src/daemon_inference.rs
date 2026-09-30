// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ingest verbs' daemon inference: the parsed CLI globals handed to the
//! one decider, `oicp_client::daemon_inference::build_inference` (probe, model
//! ids, HTTP provider). Field plumbing only; svrn's chat surface has its own
//! (`chat_cmd::bootstrap::build_inference`) over the same decider, because
//! these verbs leave for ingest's CLI and may not name svrn's session
//! bootstrap (pb-cli-llm-ingest-move).

use std::sync::Arc;

use sovereign_cli_base::chat_globals::ChatGlobals;
use sovereign_contracts::error::Result;
use sovereign_contracts::traits::InferenceProvider;

/// The daemon-backed provider, the daemon base and the resolved embed model id.
pub async fn build_inference(
    globals: &ChatGlobals,
) -> Result<(Arc<dyn InferenceProvider>, String, String)> {
    oicp_client::daemon_inference::build_inference(
        &globals.daemon_base,
        globals.bearer.as_deref(),
        globals.chat_model.as_deref(),
        globals.embed_model.as_deref(),
        globals.guest_link_active,
        globals.guest_lender_url.as_deref(),
    )
    .await
}
