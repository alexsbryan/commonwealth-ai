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

/// What reading serve's engine state found. The absences stay apart
/// (principle 6): "not observed yet" is an [`EngineStateRead::Answered`]
/// whose `device_memory` is `None`, and it is never the same answer as a
/// serve that is not there or one that did not answer in time. Moved from
/// the svrn daemon's `serve_client` when its `/v1/mesh/status` went
/// (pb-mesh-exit-transport); `svrn mesh plan|bench` read it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineStateRead {
    /// serve answered its cached view.
    Answered(sovereign_contracts::engine_state::EngineState),
    /// Nothing answered at the base, or what answered refused or was unreadable.
    Unreachable(String),
    /// serve did not answer within the bound.
    DidNotAnswerInTime,
}

/// Read serve's engine state, bounded like every other status read of serve
/// (`crate::reach::PROBE_TIMEOUT`, 2 s): a cached view on serve's side does
/// not bound the dial to it (the rule minted on 2026-07-30).
pub async fn read_engine_state(base: &str) -> EngineStateRead {
    let bound = crate::reach::PROBE_TIMEOUT;
    let url = format!(
        "{}{}",
        base.trim_end_matches('/'),
        sovereign_contracts::engine_state::ENGINE_STATE_PATH
    );
    let read = async {
        let resp = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("serve at {base} is not reachable: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "serve at {base} refused its engine state: HTTP {}",
                resp.status()
            ));
        }
        resp.json::<sovereign_contracts::engine_state::EngineState>()
            .await
            .map_err(|e| format!("serve at {base} answered an unreadable engine state: {e}"))
    };
    let outcome = match tokio::time::timeout(bound, read).await {
        Ok(Ok(state)) => EngineStateRead::Answered(state),
        Ok(Err(why)) => EngineStateRead::Unreachable(why),
        Err(_) => EngineStateRead::DidNotAnswerInTime,
    };
    tracing::debug!(serve_base = base, bound_ms = bound.as_millis() as u64, outcome = ?outcome, "engine state read from serve");
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stub serve on a free loopback port whose engine-state route waits
    /// `hold` before it answers the empty view.
    async fn stub_serve(hold: std::time::Duration) -> String {
        use axum::routing::get;
        let app = axum::Router::new().route(
            sovereign_contracts::engine_state::ENGINE_STATE_PATH,
            get(move || async move {
                tokio::time::sleep(hold).await;
                axum::Json(sovereign_contracts::engine_state::EngineState::default())
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        base
    }

    #[tokio::test]
    async fn a_serve_that_holds_the_route_past_the_bound_is_named_not_waited_on() {
        let base = stub_serve(std::time::Duration::from_secs(30)).await;
        let started = std::time::Instant::now();
        let read = read_engine_state(&base).await;
        let took = started.elapsed();
        assert_eq!(read, EngineStateRead::DidNotAnswerInTime);
        assert!(
            took < crate::reach::PROBE_TIMEOUT + std::time::Duration::from_secs(1),
            "the read waited {took:?}, past the bound plus 1 s"
        );
    }

    #[tokio::test]
    async fn a_serve_that_answers_is_read_and_not_observed_yet_stays_none() {
        let base = stub_serve(std::time::Duration::ZERO).await;
        match read_engine_state(&base).await {
            EngineStateRead::Answered(state) => assert_eq!(state.device_memory, None),
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_serve_at_the_base_is_unreachable_not_empty() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        assert!(matches!(
            read_engine_state(&base).await,
            EngineStateRead::Unreachable(_)
        ));
    }
}
