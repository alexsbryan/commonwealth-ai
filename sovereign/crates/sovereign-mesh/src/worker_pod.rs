//! Ephemeral worker pods — owner-initiated TLS-pinned worker transport.
//!
//! Spec: `sovereign/docs/EPHEMERAL_WORKER_PODS.md`. This module owns the
//! wire-protocol seam between an owner (a persistent peer — desktop, CLI)
//! and a worker (a Vast/RunPod box rented for a few hours). The seam is:
//!
//! - Owner mints a [`BootstrapBlob`] in-process (no third-party rendezvous).
//!   The blob carries a 32-byte seed, a signed [`WorkerToken`], an upload
//!   manifest (filename → SHA-256), and the owner's Ed25519 verifying key.
//! - The blob is base64'd into the pod's `onstart` env (`SOVEREIGN_BOOTSTRAP`).
//! - Pod derives an Ed25519 keypair from the seed, generates a self-signed
//!   X.509 cert from it, and starts HTTPS on `:9742`.
//! - Owner connects, pinning the cert's public-key thumbprint (SHA-256 of
//!   the raw 32-byte Ed25519 verifying key) — known *before* the pod boots
//!   because the seed determined it. No TOFU window.
//! - Every owner→worker request carries the [`WorkerToken`] in
//!   `Authorization: Bearer <token>`. The pod verifies the Ed25519
//!   signature against the owner verifying key embedded in the blob it
//!   booted with — so this pod's daemon trusts exactly one owner.
//!
//! ## Why thumbprint = SHA-256(raw pubkey), not the full SPKI DER
//!
//! Both forms are stable for an Ed25519 keypair (the SPKI encoding is
//! deterministic). The raw-pubkey form is simpler to compute on both
//! sides — the owner has the verifying key in hand from the seed, and
//! the pod's TLS cert verifier on the owner side extracts the cert's
//! public key and SHA-256s those 32 bytes. No ASN.1 walk required.
//!
//! ## Scope of this module
//!
//! This module is the foundation: types, key derivation, cert generation,
//! blob serialization, token sign/verify. The owner-side controller
//! (`WorkerController`) lives at the bottom of this file as a stub; the
//! pod-side HTTP endpoints live in `worker_http.rs`. Both build on the
//! types here.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::pkcs8::EncodePrivateKey;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Bootstrap-blob protocol version. Bump when a wire-incompatible field
/// is added. Pods reject blobs whose version they don't recognise.
pub const BOOTSTRAP_VERSION: u8 = 1;

/// Default port the worker daemon binds for the owner-only HTTPS surface.
/// Reuses the persistent peer's "internal" port — pods don't run a mesh
/// gossip listener, so 9742 is free for the worker endpoints.
pub const WORKER_PORT: u16 = 9742;

/// SHA-256 digest type as a fixed-size array — easier to pass through
/// `serde` and compare in tests than a `Vec<u8>`.
pub type Sha256Digest = [u8; 32];

#[derive(Debug, Error)]
pub enum WorkerPodError {
    #[error("bootstrap blob version {0} not supported (expected {BOOTSTRAP_VERSION})")]
    UnsupportedVersion(u8),
    #[error("bootstrap blob malformed: {0}")]
    BlobMalformed(String),
    #[error("worker token malformed: {0}")]
    TokenMalformed(String),
    #[error("worker token signature invalid")]
    TokenSignatureInvalid,
    #[error("worker token expired at unix={expires_unix} (now={now_unix})")]
    TokenExpired { expires_unix: u64, now_unix: u64 },
    #[error("worker token bound to a different pod (claim={claim_thumbprint:?})")]
    TokenWrongPod { claim_thumbprint: Sha256Digest },
    #[error("worker token bound to a different job (claim={claim:?}, expected={expected:?})")]
    TokenWrongJob { claim: String, expected: String },
    #[error("cert generation failed: {0}")]
    CertGen(String),
    #[error("key derivation failed: {0}")]
    KeyDerivation(String),
}

pub type Result<T> = std::result::Result<T, WorkerPodError>;

// ───── Key derivation + thumbprints ─────────────────────────────────

/// Derive a deterministic Ed25519 signing key from a 32-byte seed.
///
/// The seed lives in the bootstrap blob and is the *only* secret shared
/// between owner and pod. Both sides derive the same keypair from it:
/// the pod uses the signing key to mint its TLS cert; the owner uses the
/// verifying key to pin the cert before the pod has even booted.
pub fn derive_signing_key(seed: &[u8; 32]) -> SigningKey {
    SigningKey::from_bytes(seed)
}

/// SHA-256 over the raw 32-byte Ed25519 public key. This is the
/// pin-thumbprint format used throughout this module (see the
/// module-level docs for why we don't use the full SPKI DER).
pub fn pubkey_thumbprint(vk: &VerifyingKey) -> Sha256Digest {
    let mut h = Sha256::new();
    h.update(vk.as_bytes());
    let out = h.finalize();
    let mut a = [0u8; 32];
    a.copy_from_slice(&out);
    a
}

/// Generate a self-signed X.509 cert + DER-encoded private key from a
/// 32-byte seed. The cert's public key is the Ed25519 verifying key
/// derived from the seed, so `pubkey_thumbprint(vk)` is the pin.
///
/// Subject CN is fixed (`sovereign-worker`); we don't need a meaningful
/// DNS name because the owner's pinning verifier ignores hostnames —
/// only the public-key thumbprint matters.
pub fn self_signed_cert(seed: &[u8; 32]) -> Result<(Vec<u8>, Vec<u8>)> {
    let signing_key = derive_signing_key(seed);
    let pkcs8 = signing_key
        .to_pkcs8_der()
        .map_err(|e| WorkerPodError::KeyDerivation(format!("pkcs8 encode: {e}")))?;
    let key_pair = rcgen::KeyPair::try_from(pkcs8.as_bytes())
        .map_err(|e| WorkerPodError::CertGen(format!("rcgen keypair: {e}")))?;

    let mut params = rcgen::CertificateParams::new(vec!["sovereign-worker".to_string()])
        .map_err(|e| WorkerPodError::CertGen(format!("params: {e}")))?;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "sovereign-worker");
    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| WorkerPodError::CertGen(format!("self_signed: {e}")))?;

    let cert_der = cert.der().to_vec();
    let key_der = key_pair.serialize_der();
    Ok((cert_der, key_der))
}

// ───── Bootstrap blob ───────────────────────────────────────────────

/// The bootstrap blob the owner mints in-process and ships to the pod
/// via `SOVEREIGN_BOOTSTRAP=<base64>` in Vast/RunPod's `onstart_cmd`.
///
/// Serialized as compact JSON, base64-url-encoded (no padding). At spec'd
/// shapes (a few-dozen-file upload manifest, no embedded payloads), the
/// encoded form is ~500 bytes — well under any plausible `onstart_cmd`
/// length cap.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootstrapBlob {
    pub version: u8,
    pub job_id: String,
    /// 32-byte seed for the pod's TLS keypair. Treated as a secret;
    /// anyone who reads the env var (which includes the Vast host) can
    /// impersonate the pod. See `EPHEMERAL_WORKER_PODS.md` §"Why
    /// TLS-pinned-from-seed (no TOFU)" for the threat-model carveout.
    #[serde(with = "serde_bytes_32")]
    pub seed: [u8; 32],
    /// Owner's Ed25519 verifying key. The pod uses this to validate
    /// the [`WorkerToken`] on every inbound request.
    #[serde(with = "serde_bytes_32")]
    pub owner_verifying_key: [u8; 32],
    /// Pre-signed bearer token the owner will send back in the
    /// `Authorization` header. Bound to the pod's public-key thumbprint
    /// so a stolen token can't be replayed against a different pod.
    pub worker_token: String,
    /// Filename → SHA-256. The pod's `/internal/worker/upload` endpoint
    /// validates each uploaded file against this manifest. Anything not
    /// listed is rejected.
    pub expected_uploads: BTreeMap<String, Sha256Digest>,
    /// Blob expiry (unix seconds). The pod refuses to start (and the
    /// token is also expired-by-design) past this point.
    pub expires_unix: u64,
}

impl BootstrapBlob {
    /// Convenience: the pod's pinned public-key thumbprint derived from
    /// the blob's seed. Owner and pod compute the same value.
    pub fn pod_pubkey_thumbprint(&self) -> Sha256Digest {
        let sk = derive_signing_key(&self.seed);
        pubkey_thumbprint(&sk.verifying_key())
    }

    pub fn owner_pubkey_thumbprint(&self) -> Sha256Digest {
        let mut h = Sha256::new();
        h.update(self.owner_verifying_key);
        let out = h.finalize();
        let mut a = [0u8; 32];
        a.copy_from_slice(&out);
        a
    }
}

/// Encode a bootstrap blob to its on-the-wire `SOVEREIGN_BOOTSTRAP` form.
pub fn encode_bootstrap(blob: &BootstrapBlob) -> Result<String> {
    let json = serde_json::to_vec(blob)
        .map_err(|e| WorkerPodError::BlobMalformed(format!("serialize: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

/// Decode a bootstrap blob from its `SOVEREIGN_BOOTSTRAP` form.
pub fn decode_bootstrap(encoded: &str) -> Result<BootstrapBlob> {
    let trimmed = encoded.trim();
    let bytes = URL_SAFE_NO_PAD
        .decode(trimmed)
        .map_err(|e| WorkerPodError::BlobMalformed(format!("base64: {e}")))?;
    let blob: BootstrapBlob = serde_json::from_slice(&bytes)
        .map_err(|e| WorkerPodError::BlobMalformed(format!("json: {e}")))?;
    if blob.version != BOOTSTRAP_VERSION {
        return Err(WorkerPodError::UnsupportedVersion(blob.version));
    }
    Ok(blob)
}

// ───── Worker token ─────────────────────────────────────────────────

/// Claims encoded inside a [`WorkerToken`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenClaims {
    pub job_id: String,
    /// SHA-256 of the owner's Ed25519 verifying key — redundant with
    /// the verifying key in the bootstrap blob, but cheap to include
    /// and makes audit logging simpler.
    #[serde(with = "serde_bytes_32")]
    pub owner_pubkey_thumbprint: Sha256Digest,
    /// SHA-256 of the *pod's* Ed25519 verifying key (derived from the
    /// blob's seed). Binds this token to this pod — a token minted for
    /// pod A can't be replayed against pod B.
    #[serde(with = "serde_bytes_32")]
    pub pod_pubkey_thumbprint: Sha256Digest,
    pub expires_unix: u64,
}

/// Two-segment compact token (claims_b64.sig_b64), JWT-shaped without
/// the algorithm-header indirection (we always use Ed25519). The pod
/// re-derives the algorithm from `version` in the bootstrap blob — no
/// alg-confusion attack surface.
pub fn sign_worker_token(owner_signing: &SigningKey, claims: &TokenClaims) -> Result<String> {
    let claims_json = serde_json::to_vec(claims)
        .map_err(|e| WorkerPodError::TokenMalformed(format!("serialize claims: {e}")))?;
    let claims_b64 = URL_SAFE_NO_PAD.encode(&claims_json);
    let sig: Signature = owner_signing.sign(claims_b64.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(sig.to_bytes());
    Ok(format!("{claims_b64}.{sig_b64}"))
}

/// Verify a token against the owner verifying key and bind-check the
/// pod thumbprint + (optionally) the job id.
pub fn verify_worker_token(
    token: &str,
    owner_verifying: &VerifyingKey,
    expected_pod_thumbprint: &Sha256Digest,
    expected_job_id: Option<&str>,
    now_unix: u64,
) -> Result<TokenClaims> {
    let (claims_b64, sig_b64) = token
        .split_once('.')
        .ok_or_else(|| WorkerPodError::TokenMalformed("missing '.' segment".into()))?;

    let sig_bytes = URL_SAFE_NO_PAD
        .decode(sig_b64)
        .map_err(|e| WorkerPodError::TokenMalformed(format!("sig base64: {e}")))?;
    let sig = Signature::from_slice(&sig_bytes)
        .map_err(|e| WorkerPodError::TokenMalformed(format!("sig bytes: {e}")))?;

    owner_verifying
        .verify(claims_b64.as_bytes(), &sig)
        .map_err(|_| WorkerPodError::TokenSignatureInvalid)?;

    let claims_json = URL_SAFE_NO_PAD
        .decode(claims_b64)
        .map_err(|e| WorkerPodError::TokenMalformed(format!("claims base64: {e}")))?;
    let claims: TokenClaims = serde_json::from_slice(&claims_json)
        .map_err(|e| WorkerPodError::TokenMalformed(format!("claims json: {e}")))?;

    if claims.expires_unix <= now_unix {
        return Err(WorkerPodError::TokenExpired {
            expires_unix: claims.expires_unix,
            now_unix,
        });
    }
    if &claims.pod_pubkey_thumbprint != expected_pod_thumbprint {
        return Err(WorkerPodError::TokenWrongPod {
            claim_thumbprint: claims.pod_pubkey_thumbprint,
        });
    }
    if let Some(expected_job) = expected_job_id {
        if claims.job_id != expected_job {
            return Err(WorkerPodError::TokenWrongJob {
                claim: claims.job_id.clone(),
                expected: expected_job.to_string(),
            });
        }
    }
    Ok(claims)
}

// ───── Minting a fresh bootstrap (owner-side) ──────────────────────

/// Inputs the owner gathers before calling [`mint_bootstrap`].
#[derive(Debug, Clone)]
pub struct BootstrapInputs<'a> {
    pub job_id: String,
    pub owner_signing: &'a SigningKey,
    pub expected_uploads: BTreeMap<String, Sha256Digest>,
    /// Lifetime of the blob *and* the token, in seconds from now. Tokens
    /// outlive the pod's expected runtime so polling keeps working
    /// across owner-side restarts.
    pub ttl_seconds: u64,
    /// Optional RNG override for tests. Production callers pass `None`
    /// and we use [`rand::rngs::OsRng`].
    pub seed_override: Option<[u8; 32]>,
}

/// Mint a fresh bootstrap blob with a freshly-rolled (or test-supplied)
/// seed. Returns both the blob and the pod's pinned thumbprint — the
/// owner stores the thumbprint alongside the blob so the
/// [`WorkerHandle`] can verify the cert at connect time.
pub fn mint_bootstrap(inputs: BootstrapInputs<'_>) -> Result<(BootstrapBlob, Sha256Digest)> {
    let seed = match inputs.seed_override {
        Some(s) => s,
        None => {
            // rand 0.9 moved OS-backed entropy under the fallible
            // `TryRngCore` trait — `try_fill_bytes` is the equivalent
            // of the old `RngCore::fill_bytes`. Failure is essentially
            // a kernel bug; treat it as fatal.
            use rand::TryRngCore;
            let mut s = [0u8; 32];
            let mut rng = rand::rngs::OsRng;
            rng.try_fill_bytes(&mut s)
                .map_err(|e| WorkerPodError::KeyDerivation(format!("OS rng: {e}")))?;
            s
        }
    };
    let pod_signing = derive_signing_key(&seed);
    let pod_thumbprint = pubkey_thumbprint(&pod_signing.verifying_key());

    let now = now_unix();
    let expires_unix = now.saturating_add(inputs.ttl_seconds);

    let owner_vk = inputs.owner_signing.verifying_key();
    let owner_pubkey_thumbprint = {
        let mut h = Sha256::new();
        h.update(owner_vk.as_bytes());
        let out = h.finalize();
        let mut a = [0u8; 32];
        a.copy_from_slice(&out);
        a
    };

    let claims = TokenClaims {
        job_id: inputs.job_id.clone(),
        owner_pubkey_thumbprint,
        pod_pubkey_thumbprint: pod_thumbprint,
        expires_unix,
    };
    let token = sign_worker_token(inputs.owner_signing, &claims)?;

    let blob = BootstrapBlob {
        version: BOOTSTRAP_VERSION,
        job_id: inputs.job_id,
        seed,
        owner_verifying_key: owner_vk.to_bytes(),
        worker_token: token,
        expected_uploads: inputs.expected_uploads,
        expires_unix,
    };
    Ok((blob, pod_thumbprint))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ───── WorkerHandle ─────────────────────────────────────────────────

/// Cheaply-cloneable handle to a single ephemeral worker. The shape is
/// designed so a multi-pod fan-out is just `Vec<WorkerHandle>` plus a
/// `tokio::select!` over independent `poll_completed` calls — no shared
/// cross-pod state, no consensus. See `EPHEMERAL_WORKER_PODS.md`
/// §"Multi-pod jobs (planned fast-follow)" for context.
#[derive(Clone)]
pub struct WorkerHandle {
    inner: Arc<WorkerHandleInner>,
}

struct WorkerHandleInner {
    pub host: String,
    pub port: u16,
    pub pod_pubkey_thumbprint: Sha256Digest,
    pub worker_token: String,
    pub job_id: String,
    /// Owner's signing key — kept on the handle so the controller can
    /// re-mint a fresh token if the original expires before the job
    /// finishes (future work; the MVP relies on TTL being long enough).
    pub owner_signing: SigningKey,
    /// Last cursor returned by `/internal/worker/completed`. Owners
    /// resume polling from this value after a restart (we persist
    /// handles to disk in the controller layer).
    pub last_cursor: std::sync::atomic::AtomicU64,
}

impl WorkerHandle {
    pub fn new(
        host: impl Into<String>,
        port: u16,
        pod_pubkey_thumbprint: Sha256Digest,
        worker_token: impl Into<String>,
        job_id: impl Into<String>,
        owner_signing: SigningKey,
    ) -> Self {
        Self {
            inner: Arc::new(WorkerHandleInner {
                host: host.into(),
                port,
                pod_pubkey_thumbprint,
                worker_token: worker_token.into(),
                job_id: job_id.into(),
                owner_signing,
                last_cursor: std::sync::atomic::AtomicU64::new(0),
            }),
        }
    }

    pub fn host(&self) -> &str {
        &self.inner.host
    }
    pub fn port(&self) -> u16 {
        self.inner.port
    }
    pub fn pod_pubkey_thumbprint(&self) -> &Sha256Digest {
        &self.inner.pod_pubkey_thumbprint
    }
    pub fn worker_token(&self) -> &str {
        &self.inner.worker_token
    }
    pub fn job_id(&self) -> &str {
        &self.inner.job_id
    }
    pub fn owner_verifying_key(&self) -> VerifyingKey {
        self.inner.owner_signing.verifying_key()
    }
    pub fn cursor(&self) -> u64 {
        self.inner
            .last_cursor
            .load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn advance_cursor(&self, to: u64) {
        // Monotonic — never go backwards on poll-batch ack.
        let mut cur = self.cursor();
        while to > cur {
            match self.inner.last_cursor.compare_exchange_weak(
                cur,
                to,
                std::sync::atomic::Ordering::Release,
                std::sync::atomic::Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => cur = observed,
            }
        }
    }

    /// Base URL the owner connects to. `https://` because the pod is
    /// always serving its self-signed cert on the worker port.
    pub fn base_url(&self) -> String {
        format!("https://{}:{}", self.inner.host, self.inner.port)
    }
}

impl std::fmt::Debug for WorkerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerHandle")
            .field("host", &self.inner.host)
            .field("port", &self.inner.port)
            .field("job_id", &self.inner.job_id)
            .field(
                "pod_pubkey_thumbprint",
                &hex::encode(self.inner.pod_pubkey_thumbprint),
            )
            .field("cursor", &self.cursor())
            .finish_non_exhaustive()
    }
}

// ───── serde glue for fixed-size byte arrays ────────────────────────
//
// `[u8; 32]` doesn't serde-derive directly in JSON without a hint;
// without this module, the default encoding is a 32-element array, which
// is correct but bloated (each byte becomes its own JSON number).
// Encoding as hex keeps the blob compact and human-debuggable.
mod serde_bytes_32 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        s.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D>(d: D) -> Result<[u8; 32], D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        let v = hex::decode(&s).map_err(D::Error::custom)?;
        if v.len() != 32 {
            return Err(D::Error::custom(format!(
                "expected 32 bytes, got {}",
                v.len()
            )));
        }
        let mut a = [0u8; 32];
        a.copy_from_slice(&v);
        Ok(a)
    }
}

// ───── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_owner_key() -> SigningKey {
        // Deterministic for test stability; never use in production.
        SigningKey::from_bytes(&[7u8; 32])
    }

    #[test]
    fn seed_to_keypair_is_deterministic() {
        let seed = [42u8; 32];
        let k1 = derive_signing_key(&seed);
        let k2 = derive_signing_key(&seed);
        assert_eq!(k1.verifying_key().to_bytes(), k2.verifying_key().to_bytes());
        let tp1 = pubkey_thumbprint(&k1.verifying_key());
        let tp2 = pubkey_thumbprint(&k2.verifying_key());
        assert_eq!(tp1, tp2);
    }

    #[test]
    fn self_signed_cert_succeeds_for_seed() {
        let seed = [9u8; 32];
        let (cert_der, key_der) = self_signed_cert(&seed).unwrap();
        assert!(!cert_der.is_empty());
        assert!(!key_der.is_empty());
        // Sanity: cert DER should start with the ASN.1 SEQUENCE tag.
        assert_eq!(cert_der[0], 0x30);
    }

    #[test]
    fn bootstrap_round_trip() {
        let owner = fixed_owner_key();
        let mut manifest = BTreeMap::new();
        manifest.insert("primary.gguf".to_string(), [1u8; 32]);
        manifest.insert("embed.gguf".to_string(), [2u8; 32]);

        let inputs = BootstrapInputs {
            job_id: "sep-2026-05-15".into(),
            owner_signing: &owner,
            expected_uploads: manifest,
            ttl_seconds: 3600,
            seed_override: Some([3u8; 32]),
        };
        let (blob, expected_thumbprint) = mint_bootstrap(inputs).unwrap();
        assert_eq!(blob.pod_pubkey_thumbprint(), expected_thumbprint);

        let encoded = encode_bootstrap(&blob).unwrap();
        let decoded = decode_bootstrap(&encoded).unwrap();
        assert_eq!(blob, decoded);
    }

    #[test]
    fn token_sign_verify_round_trip() {
        let owner = fixed_owner_key();
        let pod_seed = [5u8; 32];
        let pod_thumb = pubkey_thumbprint(&derive_signing_key(&pod_seed).verifying_key());

        let claims = TokenClaims {
            job_id: "j1".into(),
            owner_pubkey_thumbprint: pubkey_thumbprint(&owner.verifying_key()),
            pod_pubkey_thumbprint: pod_thumb,
            expires_unix: now_unix() + 600,
        };
        let token = sign_worker_token(&owner, &claims).unwrap();
        let verified = verify_worker_token(
            &token,
            &owner.verifying_key(),
            &pod_thumb,
            Some("j1"),
            now_unix(),
        )
        .unwrap();
        assert_eq!(verified, claims);
    }

    #[test]
    fn token_rejected_for_wrong_pod_thumbprint() {
        let owner = fixed_owner_key();
        let pod_a = pubkey_thumbprint(&derive_signing_key(&[1u8; 32]).verifying_key());
        let pod_b = pubkey_thumbprint(&derive_signing_key(&[2u8; 32]).verifying_key());

        let claims = TokenClaims {
            job_id: "j1".into(),
            owner_pubkey_thumbprint: pubkey_thumbprint(&owner.verifying_key()),
            pod_pubkey_thumbprint: pod_a,
            expires_unix: now_unix() + 600,
        };
        let token = sign_worker_token(&owner, &claims).unwrap();
        let err = verify_worker_token(&token, &owner.verifying_key(), &pod_b, None, now_unix())
            .unwrap_err();
        assert!(matches!(err, WorkerPodError::TokenWrongPod { .. }));
    }

    #[test]
    fn token_rejected_when_expired() {
        let owner = fixed_owner_key();
        let pod_thumb = pubkey_thumbprint(&derive_signing_key(&[6u8; 32]).verifying_key());
        let claims = TokenClaims {
            job_id: "j1".into(),
            owner_pubkey_thumbprint: pubkey_thumbprint(&owner.verifying_key()),
            pod_pubkey_thumbprint: pod_thumb,
            expires_unix: 100, // long past
        };
        let token = sign_worker_token(&owner, &claims).unwrap();
        let err =
            verify_worker_token(&token, &owner.verifying_key(), &pod_thumb, None, 200).unwrap_err();
        assert!(matches!(err, WorkerPodError::TokenExpired { .. }));
    }

    #[test]
    fn token_rejected_when_claims_tampered() {
        let owner = fixed_owner_key();
        let pod_thumb = pubkey_thumbprint(&derive_signing_key(&[6u8; 32]).verifying_key());
        let claims = TokenClaims {
            job_id: "j1".into(),
            owner_pubkey_thumbprint: pubkey_thumbprint(&owner.verifying_key()),
            pod_pubkey_thumbprint: pod_thumb,
            expires_unix: now_unix() + 600,
        };
        let token = sign_worker_token(&owner, &claims).unwrap();
        // Flip a character in the claims segment — signature should fail.
        let mut chars: Vec<char> = token.chars().collect();
        chars[0] = if chars[0] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        let err = verify_worker_token(
            &tampered,
            &owner.verifying_key(),
            &pod_thumb,
            None,
            now_unix(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            WorkerPodError::TokenSignatureInvalid | WorkerPodError::TokenMalformed(_)
        ));
    }

    #[test]
    fn decode_rejects_wrong_version() {
        // Build a blob, hand-edit version, re-encode, and confirm reject.
        let owner = fixed_owner_key();
        let inputs = BootstrapInputs {
            job_id: "j".into(),
            owner_signing: &owner,
            expected_uploads: BTreeMap::new(),
            ttl_seconds: 60,
            seed_override: Some([0u8; 32]),
        };
        let (mut blob, _) = mint_bootstrap(inputs).unwrap();
        blob.version = 99;
        let encoded = encode_bootstrap(&blob).unwrap();
        let err = decode_bootstrap(&encoded).unwrap_err();
        assert!(matches!(err, WorkerPodError::UnsupportedVersion(99)));
    }

    #[test]
    fn worker_handle_cursor_is_monotonic() {
        let owner = fixed_owner_key();
        let pod_thumb = pubkey_thumbprint(&derive_signing_key(&[8u8; 32]).verifying_key());
        let handle = WorkerHandle::new("pod.example", 9742, pod_thumb, "tok", "job-x", owner);
        assert_eq!(handle.cursor(), 0);
        handle.advance_cursor(10);
        assert_eq!(handle.cursor(), 10);
        // Backwards updates ignored.
        handle.advance_cursor(5);
        assert_eq!(handle.cursor(), 10);
        handle.advance_cursor(11);
        assert_eq!(handle.cursor(), 11);
    }
}
