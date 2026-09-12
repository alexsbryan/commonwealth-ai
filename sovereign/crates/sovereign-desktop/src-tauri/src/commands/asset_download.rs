// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ask the daemon to fetch a model, and watch it.
//!
//! The app used to hold two downloaders of its own — `commands/models.rs` for
//! GGUFs (its own client, its own content-type sniff, its own validator) and
//! `atlas_commands.rs` for the GLiNER export — both writing under
//! `svrnmesh_root()/models`, a root the daemon owns (sv-surface svt-7). Both
//! are now `POST /v1/admin/assets/download` plus this poll.
//!
//! ONE drive, two sinks. The callers differ only in which Tauri event they
//! re-emit; the request, the poll cadence, the terminal states and the three
//! ways a job can end the wait are decided here, so a third asset kind does
//! not arrive with a fourth poll loop (ARCH principle 8).

use std::time::Duration;

use sovereign_contracts::daemon_wire::{
    AssetDownloadProgress, AssetDownloadRequest, AssetDownloadState, IngestJobAck,
};
use sovereign_turn_client::TurnClient;

/// How often the app asks. Fast enough that a progress bar moves, slow enough
/// that a 20 GB download does not cost thousands of round trips — the daemon
/// coalesces its own observations at 250 ms, so polling faster buys nothing.
const POLL_INTERVAL: Duration = Duration::from_millis(400);

/// Start the download and poll to a terminal state, handing every frame to
/// `on_frame`.
///
/// `Ok` carries the terminal frame, whose `path` is where the DAEMON put the
/// file — the app does not re-derive it. Every non-success is an `Err` with
/// the daemon's own sentence: a failed download, and a job the daemon has no
/// record of (it restarted mid-fetch), which is reported as itself rather
/// than as a completed download of nothing (ARCH principle 6).
pub(crate) async fn run_asset_download(
    base_url: String,
    request: &AssetDownloadRequest,
    on_frame: impl Fn(&AssetDownloadProgress),
) -> Result<AssetDownloadProgress, String> {
    let client = TurnClient::new(base_url);
    let ack: IngestJobAck = client
        .asset_download(request)
        .await
        .map_err(|e| format!("the daemon refused the download: {e}"))?;
    tracing::info!(
        job_id = %ack.job_id,
        kind = request.kind.as_str(),
        "asset_download: the daemon accepted the job"
    );

    loop {
        let frame: AssetDownloadProgress = client
            .asset_download_progress(&ack.job_id)
            .await
            .map_err(|e| format!("reading download progress: {e}"))?;
        on_frame(&frame);
        match frame.state {
            AssetDownloadState::Complete => return Ok(frame),
            AssetDownloadState::Error => {
                return Err(frame.error.unwrap_or_else(|| {
                    // The daemon sets `error` on every Error frame; if one
                    // ever arrives without it, say THAT rather than inventing
                    // a cause or reporting success.
                    format!("job {} failed and reported no reason", ack.job_id)
                }));
            }
            AssetDownloadState::Unknown => {
                return Err(format!(
                    "the daemon has no record of job {} — it restarted while the \
                     download was running. Nothing was installed; try again.",
                    ack.job_id
                ))
            }
            AssetDownloadState::Downloading => tokio::time::sleep(POLL_INTERVAL).await,
        }
    }
}
