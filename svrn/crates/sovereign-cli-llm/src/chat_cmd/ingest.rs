// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ingest's engine, as a distribution composes it into this process
//! (pb-cli-llm-ingest-move-compose; FIVE_PROGRAMS §2c). The daemon's pattern
//! since pb-ingest-dial-daemon: the stock distribution's `sovereign-cli-llm-stock`
//! hands [`bin_main_with`](crate::bin_main_with) a `process::HostedIngest`, and
//! every session builds its engine through it. The bare `sovereign-cli-llm`
//! installs none, and a lane that reads a corpus names the absence
//! ([`NO_INGEST`]) instead of reading nothing.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use corpus_engine_atlas_reader::ports::AtlasPort;
use sovereign_daemon::hosted_ingest::{HostedIngest, IngestHost, IngestMount};
use sovereign_store::sqlite::SqliteStateStore;

/// Why a lane that reads corpora refuses in the bare binary.
pub const NO_INGEST: &str = "no ingest program is composed in this process: the bare \
                             `sovereign-cli-llm` reads no corpus. Run the verb through `svrn`, \
                             which execs the stock distribution's `sovereign-cli-llm-stock`";

/// The partition suffix this CLI's engine has always had: it never set a
/// node id, so it ran on the engine's default, and the daemon's
/// `resolve_self_node_id` stays the daemon's.
const CLI_NODE_ID: &str = "local";

static HOSTED: OnceLock<HostedIngest> = OnceLock::new();

/// Called once, by the entry, before any verb runs.
pub(crate) fn install(ingest: Option<HostedIngest>) {
    match ingest {
        Some(ingest) => {
            tracing::debug!(target: "sovereign_cli_llm::ingest", "ingest composed in this process");
            if HOSTED.set(ingest).is_err() {
                tracing::warn!(target: "sovereign_cli_llm::ingest", "ingest already installed; the second composition is ignored");
            }
        }
        None => {
            tracing::debug!(target: "sovereign_cli_llm::ingest", "no ingest program composed; corpus lanes name the absence");
        }
    }
}

/// The engine over `provider`, rooted at `data_dir` (its `indexes/` and
/// `recipes/`); `None` when no ingest program is composed.
pub(crate) fn compose(
    data_dir: PathBuf,
    provider: Arc<dyn sovereign_core::traits::InferenceProvider>,
    embed_model: &str,
    store: Arc<SqliteStateStore>,
) -> Option<IngestMount> {
    let Some(hosted) = HOSTED.get() else {
        tracing::info!(target: "sovereign_cli_llm::ingest", data_dir = %data_dir.display(), "ingest absent: no engine composed");
        return None;
    };
    tracing::info!(target: "sovereign_cli_llm::ingest", data_dir = %data_dir.display(), embed_model, "composing ingest's engine");
    Some(hosted.compose(IngestHost {
        data_dir,
        provider,
        embed_model: embed_model.to_string(),
        node_id: CLI_NODE_ID.to_string(),
        chunk_entity_store: store,
        // The CLI's engine had no tiered provider and no chunk NER; a session
        // reads, and the vault build wires its own extractor.
        ner: None,
        conv_tiered: None,
        folder_tiered: None,
    }))
}

/// Ingest's atlas and enrichment-config ports, when composed.
pub(crate) fn ports() -> Option<&'static HostedIngest> {
    HOSTED.get()
}

/// Ingest's recipe-authoring seams, or [`NO_INGEST`] when none is composed.
pub(crate) fn recipe_author(
) -> Result<sovereign_contracts::recipe::testing::RecipeAuthorSeams, &'static str> {
    match HOSTED.get() {
        Some(hosted) => Ok(hosted.recipe_author()),
        None => {
            tracing::debug!(target: "sovereign_cli_llm::ingest", "recipe-author seams absent: no ingest program composed");
            Err(NO_INGEST)
        }
    }
}

/// Ingest's atlas port, or [`NO_INGEST`] when none is composed: the one
/// accessor the svrn verbs that write or read an atlas take it through
/// (pb-cli-llm-ingest-move-remainder).
pub(crate) fn atlas() -> Result<Arc<dyn AtlasPort>, &'static str> {
    match HOSTED.get() {
        Some(hosted) => Ok(hosted.atlas()),
        None => {
            tracing::debug!(target: "sovereign_cli_llm::ingest", "atlas port absent: no ingest program composed");
            Err(NO_INGEST)
        }
    }
}

/// Ingest's engine-free calls (the daemon chat client, the folder tiered run,
/// the NER chunk adapter), or [`NO_INGEST`] when none is composed.
pub(crate) fn calls() -> Result<&'static sovereign_daemon::hosted_ingest::IngestCalls, &'static str>
{
    match HOSTED.get() {
        Some(hosted) => Ok(hosted.calls()),
        None => {
            tracing::debug!(target: "sovereign_cli_llm::ingest", "ingest calls absent: no ingest program composed");
            Err(NO_INGEST)
        }
    }
}
