// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one place a [`RemoteApiProvider`] waits out backpressure, moved out of
//! lib.rs for its arch-gate ceiling. Two faces on one loop: the formatted one
//! every caller used, and the raw one that keeps a refusal's status for a
//! caller that acts on it (the structured-output fallback in `chat_wire`).

use crate::{error_excerpt, shed_retry_after, RemoteApiProvider};
use crate::{SHED_MAX_ATTEMPTS, SHED_TOTAL_WAIT_CAP};
use sovereign_contracts::error::{Error, Result};

/// A host's non-success answer, typed, so a caller can act on the status.
pub(crate) struct Refusal {
    pub(crate) status: reqwest::StatusCode,
    pub(crate) body: String,
    /// `(attempts, waited)` when the host shed until the budget ran out.
    shed_spent: Option<(u32, std::time::Duration)>,
}

impl Refusal {
    /// The refusal as the error callers have always seen. A shed is reported
    /// AS a shed: the caller needs to know this was "busy", not "broken", to
    /// decide whether to route elsewhere (§18.3).
    pub(crate) fn into_error(self, what: &str) -> Error {
        match self.shed_spent {
            None => Error::Inference(format!(
                "{what} returned {}: {}",
                self.status,
                error_excerpt(&self.body)
            )),
            Some((attempt, waited)) => Error::Inference(format!(
                "{what} shed by the host after {attempt} attempt(s), {}s waited: {}",
                waited.as_secs(),
                error_excerpt(&self.body)
            )),
        }
    }
}

impl RemoteApiProvider {
    /// Send, and come back when the host asks us to.
    ///
    /// THE ONE place this client waits out backpressure (ARCH §10.6). `build`
    /// re-creates the request per attempt rather than cloning, so a body
    /// stream cannot be consumed by a failed try.
    ///
    /// Returns the FIRST success, or the last refusal. A refusal that is not a
    /// shed returns immediately and untouched — see [`shed_retry_after`].
    pub(crate) async fn send_honouring_shed<F>(
        &self,
        build: F,
        what: &'static str,
    ) -> Result<reqwest::Response>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        self.send_honouring_shed_raw(build, what)
            .await?
            .map_err(|refusal| refusal.into_error(what))
    }

    /// [`Self::send_honouring_shed`] with the refusal left typed. `Err` is a
    /// transport failure; `Ok(Err(_))` is the host saying no.
    pub(crate) async fn send_honouring_shed_raw<F>(
        &self,
        build: F,
        what: &'static str,
    ) -> Result<std::result::Result<reqwest::Response, Refusal>>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut waited = std::time::Duration::ZERO;
        let mut attempt = 0u32;
        loop {
            let response = build()
                .send()
                .await
                .map_err(|e| Error::Inference(format!("{what} failed: {e}")))?;
            if response.status().is_success() {
                return Ok(Ok(response));
            }
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            attempt += 1;

            let shed = if self.wait_out_sheds {
                shed_retry_after(status, &body)
            } else {
                // Not our shed to wait out: report it so the caller can route
                // elsewhere. Peers depend on this — see `wait_out_sheds`.
                None
            };
            let Some(delay) = shed else {
                // Not backpressure — a real failure. Surface it as it arrived.
                let shed_spent = None;
                return Ok(Err(Refusal {
                    status,
                    body,
                    shed_spent,
                }));
            };
            if attempt >= SHED_MAX_ATTEMPTS || waited + delay > SHED_TOTAL_WAIT_CAP {
                let shed_spent = Some((attempt, waited));
                return Ok(Err(Refusal {
                    status,
                    body,
                    shed_spent,
                }));
            }
            tracing::info!(
                target: "oicp_client",
                what,
                attempt,
                delay_ms = delay.as_millis() as u64,
                waited_ms = waited.as_millis() as u64,
                "shed — honouring the host's Retry-After and coming back"
            );
            tokio::time::sleep(delay).await;
            waited += delay;
        }
    }
}
