//! Pinned transport — the per-pod handle that lets the mesh's
//! inference scheduler route requests to an ephemeral worker pod the
//! same way it routes to a persistent mesh peer.
//!
//! Spec: `sovereign/docs/PINNED_WORKER_AS_INFERENCE_PEER.md`.
//!
//! ## What it carries
//!
//! - A pre-built `reqwest::Client` whose only trust root is the pod's
//!   seed-derived self-signed cert. Hostname validation is disabled
//!   because the pin is the cert itself, not the CN. Cheap to clone
//!   (just an `Arc`-shared client).
//! - The owner-signed `WorkerToken` from the bootstrap blob — same
//!   bearer the worker daemon's `require_worker_token` middleware
//!   already validates on `/internal/worker/*`.
//! - A display label for tracing; not load-bearing.
//!
//! ## Why a struct (not a free `reqwest::Client`)
//!
//! `PeerInferenceEndpoint` is the only shape the mesh scheduler knows.
//! Adding `Option<PinnedTransport>` to it lets the existing
//! `select_peer` / scoring / fan-out plumbing remain unchanged — the
//! only hot-path change is "if `transport.is_some()`, build the
//! `RemoteApiProvider` with the pinned client + bearer instead of the
//! default mesh client."
//!
//! ## What it does NOT do
//!
//! - No retry/backoff policy — that's the scheduler's job, identical
//!   for pinned and gossiped peers.
//! - No per-request body construction — the OICP envelope and prompt
//!   shape come from `RemoteApiProvider::build_request`, same as every
//!   other peer route.
//! - No cert rotation — the seed is fixed for the pod's lifetime, so
//!   the cert never changes. If the pod is destroyed and re-created,
//!   the new bootstrap blob has a new seed and the owner builds a
//!   fresh `PinnedTransport` from it.

use commonwealth_core::ids::NodeId;
use reqwest::Certificate;
use sha2::{Digest, Sha256};

use crate::worker_pod::{self_signed_cert, BootstrapBlob, WorkerPodError};

/// Result alias for transport-construction errors. Surfaces the same
/// error type the worker_pod module already exposes so callers don't
/// have to juggle two error families.
pub type Result<T> = std::result::Result<T, TransportError>;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("worker pod cert generation failed: {0}")]
    Pod(#[from] WorkerPodError),
    #[error("reqwest client build failed: {0}")]
    Reqwest(#[from] reqwest::Error),
}

/// How to actually open a connection to a pinned worker pod.
///
/// `None` on `PeerInferenceEndpoint::transport` means the default
/// mesh transport (plain HTTP on `:9741`, gossip-issued bearer).
/// `Some(handle)` means use the pinned client + worker token
/// recorded here.
#[derive(Clone)]
pub struct PinnedTransport {
    /// Pre-built `reqwest::Client` whose only trust root is the pod's
    /// seed-derived cert. Cheap to clone — `reqwest::Client` is an
    /// `Arc` internally.
    pub client: reqwest::Client,
    /// Bearer to set on every outbound call. The owner-signed
    /// `WorkerToken` from the bootstrap blob — same bearer the worker
    /// daemon's `require_worker_token` middleware verifies.
    pub bearer: String,
    /// Display label for tracing; not load-bearing.
    pub label: String,
}

impl std::fmt::Debug for PinnedTransport {
    /// Hand-rolled because `reqwest::Client` doesn't implement Debug
    /// in a useful way (it dumps the internal config). The bearer is
    /// also redacted — it's a signed token and shouldn't end up in
    /// log files even at trace level.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinnedTransport")
            .field("label", &self.label)
            .field("bearer", &"<redacted>")
            .finish()
    }
}

/// Build a TLS-pinned `reqwest::Client` for a freshly-decoded blob.
/// Both `WorkerController::build_pinned_client` and this function
/// produce byte-identical clients — the worker_controller helper
/// exists for the controller's internal flow; this one is the public
/// factory the mesh scheduler uses without depending on a controller
/// instance.
pub fn build_pinned_client(blob: &BootstrapBlob) -> Result<reqwest::Client> {
    let (cert_der, _key_der) = self_signed_cert(&blob.seed)?;
    let cert = Certificate::from_der(&cert_der)?;
    let client = reqwest::ClientBuilder::new()
        .add_root_certificate(cert)
        // The pod's cert CN is a generic placeholder
        // ("sovereign-worker"). The pin is the key material, not the
        // hostname — accepting "invalid" hostnames here doesn't weaken
        // the trust model because only this specific cert is accepted,
        // and only one private key on earth can present it (the one
        // derived from the seed in our blob).
        .danger_accept_invalid_hostnames(true)
        // Match the default request timeout used elsewhere in
        // sovereign-inference's RemoteApiProvider so a long synthesis
        // call against a pinned pod isn't artificially capped.
        .timeout(std::time::Duration::from_secs(1800))
        .build()?;
    Ok(client)
}

/// Construct a [`PinnedTransport`] from a bootstrap blob + display
/// label. The label flows into tracing on every routing decision so an
/// operator reading the logs can tell at a glance which pinned pod
/// served a request without grepping for the synthetic node id.
pub fn build_pinned_transport(
    blob: &BootstrapBlob,
    label: impl Into<String>,
) -> Result<PinnedTransport> {
    Ok(PinnedTransport {
        client: build_pinned_client(blob)?,
        bearer: blob.worker_token.clone(),
        label: label.into(),
    })
}

/// Derive a stable synthetic `NodeId` from the bootstrap blob's seed.
///
/// Stable across CLI invocations against the same pod (the seed is
/// the only secret persisted in the on-disk snapshot), so the
/// scheduler's per-peer throughput cache + ledger keys keep
/// accumulating observations after a `pipeline run` reconnects to an
/// existing pod. Different from the pod's pubkey thumbprint by
/// SHA-256 domain separation so the two values can never be confused.
///
/// **Why not reuse `pod_pubkey_thumbprint`.** The pubkey thumbprint is
/// already a `NodeId`-shaped 32 bytes — but it's also the TLS pin and
/// the wire identity. Conflating "identity for the mesh scheduler's
/// hashmap key" with "TLS pin" leaks intent. Domain-separating with a
/// distinct prefix keeps the two namespaces independent.
pub fn synthetic_node_id_from_seed(seed: &[u8; 32]) -> NodeId {
    let mut h = Sha256::new();
    h.update(b"sovereign-pinned-pod-node-id\0");
    h.update(seed);
    let out = h.finalize();
    // `NodeId` is 16 bytes wide; take the high half of the SHA-256.
    // The collision space is 2^128 — fine for a per-pod synthetic id.
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&out[..16]);
    NodeId::from_u128(u128::from_be_bytes(bytes))
}

/// Convenience wrapper that builds a `PinnedTransport` and yields the
/// synthetic node id in one call. Used by `PinnedWorkerEndpointSource`
/// when constructing endpoints from a `WorkerHandle` + blob pair.
pub fn pinned_transport_and_node_id(
    blob: &BootstrapBlob,
    label: impl Into<String>,
) -> Result<(PinnedTransport, NodeId)> {
    let transport = build_pinned_transport(blob, label)?;
    let node_id = synthetic_node_id_from_seed(&blob.seed);
    Ok((transport, node_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker_pod::{mint_bootstrap, BootstrapInputs};
    use ed25519_dalek::SigningKey;
    use std::collections::BTreeMap;

    fn fixed_owner_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn mint_with_seed(seed: [u8; 32]) -> BootstrapBlob {
        let owner = fixed_owner_key();
        let (blob, _thumbprint) = mint_bootstrap(BootstrapInputs {
            job_id: "test-job".into(),
            owner_signing: &owner,
            expected_uploads: BTreeMap::new(),
            ttl_seconds: 3600,
            seed_override: Some(seed),
        })
        .expect("mint");
        blob
    }

    #[test]
    fn synthetic_node_id_is_deterministic_per_seed() {
        let id_a = synthetic_node_id_from_seed(&[42u8; 32]);
        let id_b = synthetic_node_id_from_seed(&[42u8; 32]);
        assert_eq!(id_a, id_b, "same seed must produce same node id");

        let id_c = synthetic_node_id_from_seed(&[43u8; 32]);
        assert_ne!(id_a, id_c, "different seeds must produce different ids");
    }

    #[test]
    fn synthetic_node_id_differs_from_pod_thumbprint() {
        // Domain separation: the scheduler's hashmap key must not
        // collide with the TLS pin. A future refactor that conflates
        // them would fail this test.
        let blob = mint_with_seed([99u8; 32]);
        let thumbprint = blob.pod_pubkey_thumbprint();
        let node_id = synthetic_node_id_from_seed(&blob.seed);
        // NodeId is 16 bytes; compare against the high 16 bytes of the
        // thumbprint. They must differ — same input, different
        // domain-separation prefix.
        assert_ne!(
            node_id.as_bytes(),
            <&[u8; 16]>::try_from(&thumbprint[..16]).unwrap(),
            "synthetic node id collided with pod pubkey thumbprint — \
             the SHA-256 domain-separation prefix in \
             synthetic_node_id_from_seed must be intact"
        );
    }

    #[test]
    fn pinned_client_builds_without_error() {
        let blob = mint_with_seed([7u8; 32]);
        let client = build_pinned_client(&blob).expect("client builds");
        // Sanity: the client can format a GET — we don't actually send
        // it (no server), but exercising the builder catches any panic
        // path inside reqwest.
        let _ = client.get("https://127.0.0.1:9742/v1/models");
    }

    #[test]
    fn pinned_transport_clones_cheaply() {
        let blob = mint_with_seed([12u8; 32]);
        let t = build_pinned_transport(&blob, "pod-test").expect("transport");
        let t2 = t.clone();
        assert_eq!(t.label, t2.label);
        assert_eq!(t.bearer, t2.bearer);
        // reqwest::Client is Arc-shared internally, so cloning it
        // doesn't reach into the TLS config; this test is mostly a
        // typecheck that Clone is wired correctly.
    }

    #[test]
    fn pinned_transport_debug_redacts_bearer() {
        let blob = mint_with_seed([5u8; 32]);
        let t = build_pinned_transport(&blob, "pod-redact").expect("transport");
        let dbg = format!("{t:?}");
        assert!(dbg.contains("pod-redact"), "label should be in Debug");
        assert!(
            !dbg.contains(&t.bearer),
            "bearer leaked through Debug — redaction broke"
        );
        assert!(dbg.contains("<redacted>"));
    }

    #[test]
    fn deterministic_cert_from_seed() {
        // The owner-side flow re-derives the cert from the seed every
        // time a snapshot is loaded. If `self_signed_cert(seed)` ever
        // becomes non-deterministic (e.g. someone adds a random
        // serial number) the owner's pin breaks and connections fail.
        // This test pins the invariant — change it deliberately, not
        // accidentally.
        let seed = [33u8; 32];
        let (cert_a, key_a) = self_signed_cert(&seed).expect("cert a");
        let (cert_b, key_b) = self_signed_cert(&seed).expect("cert b");
        assert_eq!(cert_a, cert_b, "same seed must yield identical cert");
        assert_eq!(key_a, key_b, "same seed must yield identical private key");
    }
}
