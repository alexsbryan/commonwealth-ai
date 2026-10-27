// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where serve listens on this host and what it says about itself: the two
//! reads every client of serve shares (moved from the svrn daemon's
//! `serve_client` at pb-meshapp-rest, so code's editor door dials serve
//! through the same reader svrn does).

/// Read serve's self-report, bounded like every other status read of serve
/// (`crate::reach::PROBE_TIMEOUT`). The two absences fold into one message
/// here; [`served_self_read`] keeps them apart.
pub async fn read_served_self(
    base: &str,
) -> Result<sovereign_contracts::engine_state::ServedSelf, String> {
    match served_self_read(base).await {
        EngineStateRead::Answered(served) => Ok(served),
        EngineStateRead::Unreachable(why) => Err(why),
        EngineStateRead::DidNotAnswerInTime => Err(format!(
            "serve at {base} did not answer its self-report within {:?}",
            crate::reach::PROBE_TIMEOUT
        )),
    }
}

/// Read serve's self-report, bounded: unreachable and did-not-answer-in-time
/// stay apart (svrn's follower, whose last read `/status` names,
/// pc-split-deploy-honesty-serve-reach).
pub async fn served_self_read(
    base: &str,
) -> EngineStateRead<sovereign_contracts::engine_state::ServedSelf> {
    read_bounded(
        base,
        sovereign_contracts::engine_state::SERVED_SELF_PATH,
        "self-report",
    )
    .await
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

/// What reading serve found. The absences stay apart (principle 6): a serve
/// that is not there is never the same answer as one that did not answer in
/// time. Moved from the svrn daemon's `serve_client` (pb-mesh-exit-transport).
/// `T` is what was read: serve's self-report ([`served_self_read`]), or `()`
/// where only the outcome is kept (svrn's `ServeReach`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineStateRead<T> {
    /// serve answered.
    Answered(T),
    /// Nothing answered at the base, or what answered refused or was unreadable.
    Unreachable(String),
    /// serve did not answer within the bound.
    DidNotAnswerInTime,
}

/// GET `path` from serve at `base` within `crate::reach::PROBE_TIMEOUT`;
/// `what` names the read in each absence's message.
async fn read_bounded<T: serde::de::DeserializeOwned + std::fmt::Debug>(
    base: &str,
    path: &str,
    what: &str,
) -> EngineStateRead<T> {
    let bound = crate::reach::PROBE_TIMEOUT;
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let read = async {
        let resp = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("serve at {base} is not reachable: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "serve at {base} refused its {what}: HTTP {}",
                resp.status()
            ));
        }
        resp.json::<T>()
            .await
            .map_err(|e| format!("serve at {base} answered an unreadable {what}: {e}"))
    };
    let outcome = match tokio::time::timeout(bound, read).await {
        Ok(Ok(state)) => EngineStateRead::Answered(state),
        Ok(Err(why)) => EngineStateRead::Unreachable(why),
        Err(_) => EngineStateRead::DidNotAnswerInTime,
    };
    tracing::debug!(serve_base = base, path, bound_ms = bound.as_millis() as u64, outcome = ?outcome, "{what} read from serve");
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stub serve on a free loopback port whose self-report route waits
    /// `hold` before it answers the default report.
    async fn stub_serve(hold: std::time::Duration) -> String {
        use axum::routing::get;
        let app = axum::Router::new().route(
            sovereign_contracts::engine_state::SERVED_SELF_PATH,
            get(move || async move {
                tokio::time::sleep(hold).await;
                axum::Json(sovereign_contracts::engine_state::ServedSelf::default())
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
        let read = served_self_read(&base).await;
        let took = started.elapsed();
        assert!(
            matches!(read, EngineStateRead::DidNotAnswerInTime),
            "got {read:?}"
        );
        assert!(
            took < crate::reach::PROBE_TIMEOUT + std::time::Duration::from_secs(1),
            "the read waited {took:?}, past the bound plus 1 s"
        );
    }

    #[tokio::test]
    async fn a_serve_that_answers_is_read() {
        let base = stub_serve(std::time::Duration::ZERO).await;
        match served_self_read(&base).await {
            EngineStateRead::Answered(served) => assert!(served.resident_slots.is_empty()),
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_serve_at_the_base_is_unreachable_not_empty() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        assert!(matches!(
            served_self_read(&base).await,
            EngineStateRead::Unreachable(_)
        ));
    }
}
