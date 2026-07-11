// SPDX-License-Identifier: AGPL-3.0-or-later
//! Provider manifest schema (v0.3 §4, v0.4 §2): model advertisement,
//! embed-model compatibility, knowledge plane, federation, and the
//! feature-string vocabulary.

use serde::{Deserialize, Serialize};

use crate::capability::CapabilityClaim;
use crate::ingest::IngestEndpoints;
use crate::version::OICP_VERSION;

// -----------------------------------------------------------------
// v0.4 §2.1 — Feature advertisement vocabulary
// -----------------------------------------------------------------

/// Registered feature strings for [`ProviderManifest::features`] (v0.4
/// §2.1). A host advertises the request-level capabilities it honours
/// so a decoupled client can negotiate (§3) instead of guessing.
/// Extension features carry the `x:` prefix; unknown features are
/// preserved verbatim and treated as absent by clients.
pub mod features {
    /// `response_format: {type: "json_schema"}` is grammar-enforced —
    /// output is guaranteed to validate against the supplied schema.
    pub const CONSTRAINT_JSON_SCHEMA: &str = "constraint:json_schema";
    /// `response_format: {type: "json_object"}` guarantees syntactically
    /// valid JSON (no schema-conformance guarantee).
    pub const CONSTRAINT_JSON_OBJECT: &str = "constraint:json_object";
    /// The `lark_grammar` body field is honoured; output is guaranteed
    /// to be in the grammar's language. More expressive than JSON Schema.
    pub const CONSTRAINT_LARK: &str = "constraint:lark";
    /// The `url_allowlist` sampler constraint is honoured.
    pub const CONSTRAINT_ALLOWLIST_URL: &str = "constraint:allowlist:url";
    /// The `evidence_id_allowlist` sampler constraint is honoured.
    pub const CONSTRAINT_ALLOWLIST_EVIDENCE_ID: &str = "constraint:allowlist:evidence_id";
    /// The `cmd_prefix` / `assistant_prefix` sampler constraints are honoured.
    pub const CONSTRAINT_ALLOWLIST_CMD_PREFIX: &str = "constraint:allowlist:cmd_prefix";
    /// The `think_budget` body field (a reasoning-token cap) is honoured.
    pub const THINK_BUDGET: &str = "think_budget";
    /// The `oicp` request envelope ([`InferenceRequirements`]) is
    /// consumed for routing.
    pub const OICP_REQUEST_PROPERTIES: &str = "oicp:request_properties";
    /// The §5 ingest extension (install + progress) is mounted; MUST
    /// co-occur with a populated `knowledge.ingest`.
    pub const INGEST_V1: &str = "ingest:v1";
    /// The §5.4 recipe-test endpoint is mounted; MUST co-occur with
    /// `knowledge.ingest.test_endpoint`.
    pub const INGEST_RECIPE_TEST: &str = "ingest:recipe_test";
    /// §6 fingerprints are populated on manifest models and echoed in
    /// response metadata.
    pub const MODEL_FINGERPRINT: &str = "model_fingerprint";

    /// Extension (§4.3): a request may carry the forced-choice sentinel
    /// (`structured_output: {"x_forced_choice": true, "enum": [...]}`) and
    /// the host returns a calibrated next-token distribution over the
    /// candidate labels in ONE forward pass instead of K sampling draws.
    /// Deliberately an `x:` extension, NOT a `REGISTERED` string — it is a
    /// Commonwealth-local capability pending a spec revision, so
    /// `is_valid` admits it via the `x:` prefix without touching the
    /// conformance registry. Advertised in `EMBEDDED_FEATURES`; the mesh
    /// scheduler excludes peers that don't advertise it from forced-choice
    /// dispatch (SLOT_POLICY §6).
    pub const X_FORCED_CHOICE: &str = "x:forced_choice";

    /// Extension-feature prefix (§2.1). A host MAY advertise
    /// `x:`-prefixed features not registered in this crate build.
    pub const EXTENSION_PREFIX: &str = "x:";

    /// Every feature this crate build knows how to name. Grows by spec
    /// revision; a host MAY advertise `x:`-prefixed features not listed.
    pub const REGISTERED: &[&str] = &[
        CONSTRAINT_JSON_SCHEMA,
        CONSTRAINT_JSON_OBJECT,
        CONSTRAINT_LARK,
        CONSTRAINT_ALLOWLIST_URL,
        CONSTRAINT_ALLOWLIST_EVIDENCE_ID,
        CONSTRAINT_ALLOWLIST_CMD_PREFIX,
        THINK_BUDGET,
        OICP_REQUEST_PROPERTIES,
        INGEST_V1,
        INGEST_RECIPE_TEST,
        MODEL_FINGERPRINT,
    ];

    /// True iff `f` is a registered feature string or a well-formed
    /// `x:`-prefixed extension feature (non-empty tag). This is the
    /// validity predicate the conformance suite's `manifest.features`
    /// check applies.
    pub fn is_valid(f: &str) -> bool {
        REGISTERED.contains(&f)
            || f.strip_prefix(EXTENSION_PREFIX)
                .is_some_and(|tag| !tag.is_empty())
    }
}

// -----------------------------------------------------------------
// Section 4 — Provider Manifest Schema
// -----------------------------------------------------------------

/// Provider manifest served at `GET /oicp/v1/capabilities` (§4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderManifest {
    pub oicp_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderInfo>,
    pub models: Vec<ProviderModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge: Option<KnowledgeManifest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub federation: Option<FederationManifest>,
    /// v0.4 §2: request-level capabilities this host honours. Empty
    /// (the serde default and the absence-on-the-wire shape) means
    /// "v0.3 host" — the client assumes only baseline OpenAI-compat.
    /// See the [`features`] module for registered strings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

impl ProviderManifest {
    pub fn new(models: Vec<ProviderModel>) -> Self {
        Self {
            oicp_version: OICP_VERSION.to_string(),
            provider: None,
            models,
            knowledge: None,
            federation: None,
            features: Vec::new(),
        }
    }

    /// True iff this manifest advertises feature `f` (§2).
    pub fn has_feature(&self, f: &str) -> bool {
        self.features.iter().any(|x| x == f)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub provider_type: Option<ProviderType>,
}

/// Provider type hint (§4.1). Informational only — clients MUST NOT
/// make routing decisions based on this field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    Local,
    Mesh,
    Cloud,
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    pub context_tokens: u32,
    pub status: ModelStatus,
    /// Approximate on-disk weight size in gigabytes. Used as a
    /// tiebreaker during OICP backend selection: when two models
    /// score equally against a request, prefer the smaller one
    /// (smaller ≈ faster TTFT, lighter memory footprint, less
    /// energy). Optional because providers may not know or want to
    /// publish this; absent values sort after any known value so an
    /// unknown-size model never spuriously wins a tie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_gb: Option<f32>,
    /// §4: capability claims advertised for this model. Each claim
    /// describes a (capability hint × latency class × context ×
    /// output × affinity) combination the model serves well.
    /// Multiple claims per model are expected when a single model
    /// handles more than one latency class (e.g., a 9B general
    /// model serving both fast short-context and normal long-context
    /// work).
    pub claims: Vec<CapabilityClaim>,
    /// v0.4 §6: opaque fingerprint that MUST change when the served
    /// weights, quantization, or chat template change. Lets a client
    /// key model-dependent caches on `(id, fingerprint)`. Gated by the
    /// `model_fingerprint` feature; absent on v0.3 hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatus {
    pub available: bool,
    pub loaded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_tokens_per_sec: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_ttft_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_load_time_sec: Option<u32>,
}

// -----------------------------------------------------------------
// Embed model compatibility (used by collaborative ingestion)
// -----------------------------------------------------------------

/// How token embeddings are pooled into a single sequence embedding.
/// Matches the values used by sovereign-core's EmbedQuirks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolingStrategy {
    /// Last non-padding token hidden state (Qwen3-Embedding).
    Last,
    /// Average all non-padding hidden states (mxbai, BERT-style embedders).
    Mean,
    /// [CLS] token hidden state (BERT-style models).
    Cls,
}

/// Whether L2 normalisation is performed by the server or the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizationStrategy {
    /// llama-server normalises via --embd-normalize.
    Server,
    /// Application must L2-normalise the raw vector before use.
    Application,
}

/// Embedding model identity and output shape.
/// Two nodes are compatible for collaborative ingestion iff their
/// `EmbedModelInfo` values are equal (exact match required — cosine
/// similarity across different embedding spaces is meaningless). The
/// v0.4 `query_instruction_prefix` is part of that equality: it changes
/// the query-side embedding space, so two nodes with different prefixes
/// are incompatible even when the other four fields match.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EmbedModelInfo {
    /// Model identifier, e.g. `"qwen3-embedding-0.6b"`.
    pub model_id: String,
    /// Output vector dimensionality.
    pub dimensions: usize,
    pub pooling: PoolingStrategy,
    pub normalization: NormalizationStrategy,
    /// v0.4 §4: instruction prefix prepended to *query* text (not
    /// document text) before embedding. Empty string = no prefix (also
    /// the v0.3-on-the-wire shape via serde default). A client
    /// reconstructing a query embedding for federated search MUST
    /// prepend this or it produces a vector in a different space.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub query_instruction_prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeManifest {
    pub corpora: Vec<CorpusDescriptor>,
    pub search_endpoint: String,
    /// Embed model in use on this node. `None` means the node has
    /// not advertised its embed configuration — exclude from
    /// collaborative ingestion until this is populated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_model: Option<EmbedModelInfo>,
    /// v0.4 §5: corpus-ingest endpoints this host exposes. `None` means
    /// the host does not offer an OICP ingest surface. When present,
    /// the manifest MUST also advertise the `ingest:v1` feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingest: Option<IngestEndpoints>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusDescriptor {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub total_chunks: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shards: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<u32>,
    pub fully_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationManifest {
    pub peers: Vec<PeerDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerDescriptor {
    pub name: String,
    pub capabilities_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_level: Option<String>,
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{CapabilityHint, LatencyClass};

    // ───── v0.4 back-compat + new-surface round-trips ─────────

    #[test]
    fn v03_manifest_json_deserialises_into_v04_with_defaults() {
        // A manifest emitted by a v0.3 host carries none of the v0.4
        // fields. It MUST deserialise cleanly with empty/None defaults
        // (§8) — this is the whole back-compat contract.
        let v03 = r#"{
            "oicp_version": "0.3.0",
            "models": [{
                "id": "qwen3-9b",
                "context_tokens": 16384,
                "status": {"available": true, "loaded": true},
                "claims": []
            }],
            "knowledge": {
                "corpora": [],
                "search_endpoint": "/v1/knowledge/search",
                "embed_model": {
                    "model_id": "qwen3-embedding-0.6b",
                    "dimensions": 1024,
                    "pooling": "last",
                    "normalization": "server"
                }
            }
        }"#;
        let m: ProviderManifest = serde_json::from_str(v03).expect("deserialise v0.3");
        assert!(m.features.is_empty(), "no features on a v0.3 manifest");
        assert!(m.models[0].fingerprint.is_none());
        let k = m.knowledge.as_ref().unwrap();
        assert!(k.ingest.is_none(), "no ingest surface on a v0.3 host");
        assert_eq!(
            k.embed_model.as_ref().unwrap().query_instruction_prefix,
            "",
            "absent prefix defaults to empty"
        );
    }

    #[test]
    fn empty_v04_manifest_serialises_to_v03_shape() {
        // An empty v0.4 manifest must serialise byte-identically to a
        // v0.3 manifest: none of the new fields appear on the wire when
        // empty (skip_serializing_if). This is what keeps v0.3 clients
        // from ever seeing v0.4 fields.
        let m = ProviderManifest::new(vec![]);
        let v = serde_json::to_value(&m).unwrap();
        assert!(v.get("features").is_none(), "empty features omitted");
        let obj = v.as_object().unwrap();
        // Exactly the v0.3 always-present keys (oicp_version + models);
        // provider/knowledge/federation are None → omitted.
        assert_eq!(obj.len(), 2, "only oicp_version + models on the wire");
    }

    #[test]
    fn embed_model_equality_distinguishes_query_prefix() {
        // The prefix is part of the bit-compat equality (§4): two nodes
        // that differ only in the query prefix are NOT compatible.
        let base = EmbedModelInfo {
            model_id: "qwen3-embedding-0.6b".into(),
            dimensions: 1024,
            pooling: PoolingStrategy::Last,
            normalization: NormalizationStrategy::Server,
            query_instruction_prefix: String::new(),
        };
        let prefixed = EmbedModelInfo {
            query_instruction_prefix: "Represent this query: ".into(),
            ..base.clone()
        };
        assert_ne!(base, prefixed, "prefix difference breaks compatibility");
        assert_eq!(base, base.clone());
    }

    #[test]
    fn features_validity_predicate() {
        assert!(features::is_valid(features::CONSTRAINT_JSON_SCHEMA));
        assert!(features::is_valid(features::INGEST_V1));
        assert!(features::is_valid("x:prose"), "well-formed extension");
        assert!(!features::is_valid("x:"), "empty extension tag is invalid");
        assert!(!features::is_valid("bogus"), "unregistered bare feature");
    }

    // ───── ProviderManifest round-trip ──────────────────────

    #[test]
    fn provider_manifest_round_trip_with_claims() {
        let model = ProviderModel {
            id: "qwen3-9b".into(),
            base_model: None,
            quantization: Some("Q4_K_M".into()),
            context_tokens: 16_384,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: Some(42.0),
                estimated_ttft_ms: Some(900),
                estimated_load_time_sec: None,
            },
            size_gb: Some(5.2),
            claims: vec![
                CapabilityClaim::new(
                    CapabilityHint::general(),
                    LatencyClass::Fast,
                    4_000,
                    500,
                    0.75,
                ),
                CapabilityClaim::new(
                    CapabilityHint::general(),
                    LatencyClass::Normal,
                    16_000,
                    2_000,
                    0.6,
                ),
            ],
            fingerprint: None,
        };
        let manifest = ProviderManifest::new(vec![model]);
        let json = serde_json::to_string(&manifest).unwrap();
        let back: ProviderManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.models.len(), 1);
        assert_eq!(back.models[0].claims.len(), 2);
        assert_eq!(back.models[0].claims[0].latency_class, LatencyClass::Fast);
    }
}
