// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon's synchronous [`ReplicatedKv`], over `cw-rails`' `/v1/mesh/kv/*`
//! doors (five-programs fp-110, decision five-programs-56; the doors are
//! `commonwealth-rails/src/kv.rs`). With [`super::ledger::RailsLedger`] it
//! covers all six of `StoreSeed`'s ports.
//!
//! Nothing here is wired into `AppState` yet — fp-88 flips the backing.
//!
//! Nothing is retained: every call reads through to the serving process. A
//! cached row would turn a read-modify-write (the grants handoff) into a lost
//! peer update (-56). Every failure is `ReplicatedKvError::Backend` naming the
//! URL, never `Ok(None)` or an empty scan (principle 6).
//!
//! The port is sync and its callers run on every kind of thread, so the dial
//! rides ONE dedicated thread that owns its own runtime; the caller waits on
//! a channel — inside `block_in_place` on a multi-thread runtime worker,
//! plainly otherwise. `reqwest::blocking` panics inside an async context and
//! `Handle::block_on` panics on a runtime thread, so neither is used.

use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::Duration;

use bytes::Bytes;
use futures::future::BoxFuture;
use kernel_types::NodeId;
use sovereign_contracts::peer::{
    KvLookup, KvScanQuery, KvSetBody, ReplicatedKv, ReplicatedKvEntry, ReplicatedKvError,
};

/// The ceiling `DaemonReplicatedKv` (sovereign-cli-dev) uses for the same
/// four doors: these are coordination reads, and slower IS unreachable.
const KV_TIMEOUT: Duration = Duration::from_secs(2);

type Job = BoxFuture<'static, ()>;

/// The one dial thread: a current-thread runtime that spawns each job, so
/// concurrent callers do not queue behind one another's round trip.
fn dial_thread() -> &'static tokio::sync::mpsc::UnboundedSender<Job> {
    static TX: OnceLock<tokio::sync::mpsc::UnboundedSender<Job>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Job>();
        std::thread::Builder::new()
            .name("rails-kv-dial".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        // Every later call reports the dead thread by name.
                        tracing::error!(error = %e, "rails kv: the dial thread has no runtime");
                        return;
                    }
                };
                runtime.block_on(async move {
                    while let Some(job) = rx.recv().await {
                        tokio::spawn(job);
                    }
                });
            })
            .expect("spawning the rails kv dial thread");
        tx
    })
}

/// Carry `fut` on the dial thread and wait for its answer.
fn on_dial_thread<T: Send + 'static>(
    url: &str,
    fut: impl std::future::Future<Output = Result<T, ReplicatedKvError>> + Send + 'static,
) -> Result<T, ReplicatedKvError> {
    let (reply, answer) = mpsc::sync_channel(1);
    let job: Job = Box::pin(async move {
        // Fails only when the caller is gone; nobody is left to tell.
        let _ = reply.send(fut.await);
    });
    if dial_thread().send(job).is_err() {
        tracing::warn!(url, "rails kv: the dial thread is gone");
        return Err(ReplicatedKvError::Backend(format!(
            "cannot dial the mesh's serving process at {url}: the dial thread is gone"
        )));
    }
    let on_worker = tokio::runtime::Handle::try_current()
        .map(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
        .unwrap_or(false);
    let received = if on_worker {
        tokio::task::block_in_place(|| answer.recv())
    } else {
        answer.recv()
    };
    received.unwrap_or_else(|_| {
        tracing::warn!(url, "rails kv: the dial thread dropped the call");
        Err(ReplicatedKvError::Backend(format!(
            "cannot dial the mesh's serving process at {url}: the dial thread dropped the call"
        )))
    })
}

/// One door's answer: transport failure, a non-2xx with the door's own
/// wording, and an unreadable 2xx are each a traced `Backend` naming `url`.
async fn answer<T: serde::de::DeserializeOwned>(
    url: String,
    req: reqwest::RequestBuilder,
) -> Result<T, ReplicatedKvError> {
    let fail = |what: String| {
        tracing::warn!(url = %url, error = %what, "rails kv: dial failed");
        ReplicatedKvError::Backend(what)
    };
    let resp = req.timeout(KV_TIMEOUT).send().await.map_err(|e| {
        fail(format!(
            "cannot reach the mesh's serving process at {url}: {e}"
        ))
    })?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| {
        fail(format!(
            "the mesh's serving process's answer at {url} is unreadable: {e}"
        ))
    })?;
    if !status.is_success() {
        return Err(fail(format!(
            "the mesh's serving process refused {url}: {status}: {body}"
        )));
    }
    let read = serde_json::from_str(&body).map_err(|e| {
        fail(format!(
            "the mesh's serving process's answer at {url} is a shape this build cannot read: {e}"
        ))
    })?;
    tracing::debug!(url = %url, "rails kv: answered");
    Ok(read)
}

/// [`ReplicatedKv`] dialed to `cw-rails`. Construction checks no presence: a
/// serving process that is down surfaces on the first call, by URL.
#[derive(Clone)]
pub struct RailsKv {
    base: String,
}

impl RailsKv {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

impl ReplicatedKv for RailsKv {
    fn get(&self, app_id: &str, key: &str) -> Result<Option<ReplicatedKvEntry>, ReplicatedKvError> {
        let url = self.url("/v1/mesh/kv/entry");
        let req = super::client().get(&url).query(&KvLookup {
            app_id: app_id.to_string(),
            key: key.to_string(),
        });
        on_dial_thread(&url.clone(), answer(url, req))
    }

    fn set(
        &self,
        app_id: &str,
        key: &str,
        value: Bytes,
        origin: NodeId,
    ) -> Result<bool, ReplicatedKvError> {
        let url = self.url("/v1/mesh/kv/entry");
        let req = super::client().post(&url).json(&KvSetBody {
            app_id: app_id.to_string(),
            key: key.to_string(),
            value,
            origin,
        });
        on_dial_thread(&url.clone(), answer(url, req))
    }

    fn delete(&self, app_id: &str, key: &str) -> Result<bool, ReplicatedKvError> {
        let url = self.url("/v1/mesh/kv/entry");
        let req = super::client().delete(&url).query(&KvLookup {
            app_id: app_id.to_string(),
            key: key.to_string(),
        });
        on_dial_thread(&url.clone(), answer(url, req))
    }

    fn scan(
        &self,
        app_id: &str,
        prefix: &str,
    ) -> Result<Vec<ReplicatedKvEntry>, ReplicatedKvError> {
        let url = self.url("/v1/mesh/kv/entries");
        let req = super::client().get(&url).query(&KvScanQuery {
            app_id: app_id.to_string(),
            prefix: prefix.to_string(),
        });
        on_dial_thread(&url.clone(), answer(url, req))
    }
}

#[cfg(test)]
#[path = "kv/tests.rs"]
mod tests;
