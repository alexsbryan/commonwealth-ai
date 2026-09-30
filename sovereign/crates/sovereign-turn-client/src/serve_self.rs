// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where serve listens on this host and what it says about itself: the two
//! reads every client of serve shares (moved from the svrn daemon's
//! `serve_client` at pb-meshapp-rest, so code's editor door dials serve
//! through the same reader svrn does).

/// Read serve's self-report, bounded like every other status read of serve
/// (`crate::reach::PROBE_TIMEOUT`).
pub async fn read_served_self(
    base: &str,
) -> Result<sovereign_contracts::engine_state::ServedSelf, String> {
    let url = format!(
        "{}{}",
        base.trim_end_matches('/'),
        sovereign_contracts::engine_state::SERVED_SELF_PATH
    );
    let read = async {
        let resp = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("serve at {base} is not reachable: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "serve at {base} refused its self-report: HTTP {}",
                resp.status()
            ));
        }
        resp.json::<sovereign_contracts::engine_state::ServedSelf>()
            .await
            .map_err(|e| format!("serve at {base} answered an unreadable self-report: {e}"))
    };
    match tokio::time::timeout(crate::reach::PROBE_TIMEOUT, read).await {
        Ok(r) => r,
        Err(_) => Err(format!(
            "serve at {base} did not answer its self-report within {:?}",
            crate::reach::PROBE_TIMEOUT
        )),
    }
}

/// The base a client dials `serve` at when nothing says otherwise:
/// loopback, on the one port serve listens on by default
/// (`sovereign_contracts::venue::serve_port`, serve's reader too).
pub fn default_serve_base() -> String {
    format!(
        "http://127.0.0.1:{}",
        sovereign_contracts::venue::serve_port()
    )
}
