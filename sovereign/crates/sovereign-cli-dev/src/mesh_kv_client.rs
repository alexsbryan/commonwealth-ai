// SPDX-License-Identifier: AGPL-3.0-or-later
//! The workbench's [`ReplicatedKv`] client — the dial, not the open
//! (five-programs fp-33; §4 rule 1 / §12 D2: a second process never opens
//! the mesh's store, it dials the process that owns it).
//!
//! Until this module, every work-atlas surface here opened
//! `MeshReplicatedKv` on a repo-local `mesh.db` — a coordination island no
//! other process read. The daemon's `/v1/mesh/kv/*` routes serve the ONE
//! shared instance (`AppState.inner.fabric.mesh_store`) that gossip
//! publishes from, so a record written through this client is visible to
//! the daemon, MCP peers, and the mesh.
//!
//! Absence is reported, never defaulted (principle 6): a daemon that is
//! down surfaces on the first call as `ReplicatedKvError::Backend` naming
//! the URL — a claim that cannot be written says so, it never lands in a
//! local island that only this machine can see.
//!
//! The port trait is sync, so the transport is `reqwest`'s blocking client
//! (the `sovereign-eval` precedent) with the same 2s local-daemon ceiling
//! `sovereign_cli_shared::mcp_client` uses: these are Fast-latency
//! coordination reads, and anything slower IS unreachable for that purpose.
//! The base URL is the audited accessor
//! ([`sovereign_cli_shared::urls::daemon_v1_base`]) — the `SOVEREIGN_DAEMON_URL`
//! knob moves this client with every other reader (the fp-32 precedent).

use bytes::Bytes;
use kernel_types::NodeId;
use sovereign_contracts::peer::{
    KvLookup, KvScanQuery, KvSetBody, ReplicatedKv, ReplicatedKvEntry, ReplicatedKvError,
};

/// A [`ReplicatedKv`] whose backend is the local daemon's `/v1/mesh/kv`
/// routes. Cheap to clone behind an `Arc`; safe to call from many tasks —
/// with the caveat the sync trait implies: a call blocks its thread for up
/// to the client timeout.
pub struct DaemonReplicatedKv {
    http: reqwest::blocking::Client,
    v1: String,
}

impl DaemonReplicatedKv {
    /// Dial the local daemon. Construction resolves the base URL and builds
    /// the HTTP client; daemon PRESENCE is not checked here — a daemon that
    /// is down surfaces on the first call as a named transport error.
    pub fn new() -> Result<Self, ReplicatedKvError> {
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .map_err(|e| ReplicatedKvError::Backend(format!("http client: {e}")))?,
            v1: sovereign_cli_shared::urls::daemon_v1_base(),
        })
    }

    /// ONE implementation of the transport contract, for all four port
    /// methods: send, then map every answer to the port's error with the
    /// URL in the message — transport failure carries what could not be
    /// reached, a non-2xx carries the daemon's own wording, an
    /// unparseable 2xx says the build and the daemon disagree on the
    /// shape (the fp-32 client's error family).
    fn send_json<T: serde::de::DeserializeOwned>(
        &self,
        url: String,
        req: reqwest::blocking::RequestBuilder,
    ) -> Result<T, ReplicatedKvError> {
        let resp = req.send().map_err(|e| {
            ReplicatedKvError::Backend(format!("cannot reach the daemon at {url}: {e}"))
        })?;
        let status = resp.status();
        let body = resp.text().map_err(|e| {
            ReplicatedKvError::Backend(format!("cannot reach the daemon at {url}: {e}"))
        })?;
        if !status.is_success() {
            return Err(ReplicatedKvError::Backend(format!(
                "the daemon refused {url}: {status}: {body}"
            )));
        }
        serde_json::from_str(&body).map_err(|e| {
            ReplicatedKvError::Backend(format!(
                "the daemon's answer at {url} is a shape this build cannot read: {e}"
            ))
        })
    }
}

impl ReplicatedKv for DaemonReplicatedKv {
    fn get(&self, app_id: &str, key: &str) -> Result<Option<ReplicatedKvEntry>, ReplicatedKvError> {
        let url = format!("{}/mesh/kv/entry", self.v1);
        let req = self.http.get(&url).query(&KvLookup {
            app_id: app_id.to_string(),
            key: key.to_string(),
        });
        self.send_json(url, req)
    }

    fn set(
        &self,
        app_id: &str,
        key: &str,
        value: Bytes,
        origin: NodeId,
    ) -> Result<bool, ReplicatedKvError> {
        let url = format!("{}/mesh/kv/entry", self.v1);
        let req = self.http.post(&url).json(&KvSetBody {
            app_id: app_id.to_string(),
            key: key.to_string(),
            value,
            origin,
        });
        self.send_json(url, req)
    }

    fn delete(&self, app_id: &str, key: &str) -> Result<bool, ReplicatedKvError> {
        let url = format!("{}/mesh/kv/entry", self.v1);
        let req = self.http.delete(&url).query(&KvLookup {
            app_id: app_id.to_string(),
            key: key.to_string(),
        });
        self.send_json(url, req)
    }

    fn scan(
        &self,
        app_id: &str,
        prefix: &str,
    ) -> Result<Vec<ReplicatedKvEntry>, ReplicatedKvError> {
        let url = format!("{}/mesh/kv/entries", self.v1);
        let req = self.http.get(&url).query(&KvScanQuery {
            app_id: app_id.to_string(),
            prefix: prefix.to_string(),
        });
        self.send_json(url, req)
    }
}
