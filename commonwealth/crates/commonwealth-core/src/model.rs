// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ids::{ModelId, NodeId};
use crate::oicp::CapabilityProfile;

/// Information about a model known to the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: ModelId,
    pub name: String,
    pub repo: String,
    pub file: String,
    pub size_bytes: u64,
    pub total_layers: u32,
    pub architecture: ModelArchitecture,
    /// **Dead, and a trap. Leave it empty.** See
    /// `available_on_is_unusable_because_node_id_is_not_a_json_key`.
    ///
    /// Populating it makes `serde_json` fail — `NodeId` serialises as a
    /// 16-byte array and JSON object keys must be strings — so the entry
    /// stops round-tripping through `MeshStore` and silently disappears
    /// from `/v1/models`. Every construction site in the workspace passes
    /// `HashMap::new()` for exactly this reason.
    ///
    /// Nothing reads it. "Which nodes hold this model" is answered by the
    /// OICP manifests (`/oicp/v1/capabilities`), which is what both name
    /// resolution and `/v1/models` consult. The field survives only because
    /// removing it is a wire change — it carries no `#[serde(default)]`, so
    /// a peer on an older build fails to deserialise a payload that omits
    /// it — and that is not a change to make while mesh membership is
    /// mid-migration. Delete it, with the default, once the fleet is past
    /// that; do not "fix" it by filling it in.
    pub available_on: HashMap<NodeId, ModelAvailability>,
    pub oicp_capabilities: CapabilityProfile,
    pub quantization: String,

    // ── Deployment constraints (used by the adaptive mesh scheduler) ──
    /// Minimum unified/VRAM memory required to load this model at all.
    /// Nodes below this threshold are never assigned this model.
    #[serde(default)]
    pub min_memory_gb: u32,

    /// Preferred memory for comfortable operation (model + KV cache headroom).
    #[serde(default)]
    pub preferred_memory_gb: u32,

    /// Whether this model can run as multiple independent instances.
    /// Dense models: true. MoE with expert routing that requires shared
    /// memory: false.
    #[serde(default)]
    pub supports_parallel_instances: bool,

    /// Whether this model can be sharded across nodes via pipeline parallelism.
    #[serde(default)]
    pub supports_pipeline_shard: bool,
}

/// Model architecture family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelArchitecture {
    Llama,
    Qwen,
    Mistral,
    Phi,
    Gemma,
    Other,
}

/// Whether a model is available on a specific node and in what state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailability {
    /// Model file is on disk, ready to load.
    Available,
    /// Model is currently loaded in memory and serving.
    Loaded,
    /// Model is being downloaded.
    Downloading,
}

// ── Peer-to-peer model file distribution ─────────────────────────────
//
// The wire vocabulary for `/internal/v1/models/*`, which one peer serves
// (`commonwealth-api::routes_internal::model_files`) and another consumes
// (`sovereign-mesh::model_fetch`, `::rpc_warm_http`). It lives here because
// BOTH ends must agree on these bytes for a fetch to work, and this is the
// lowest crate both already depend on.
//
// It was declared twice — byte-identical, including derive order — from
// 2026-05-11 (`dfbf7c3d3`) until 2026-09-04. Neither declaration ever drifted.
// What drifted was the CONTRACT AROUND them: `637448086` (2026-06-05) taught
// the server HTTP Range / 206 / `Content-Range`, and the client half was not
// touched and still cannot ask for a byte range. A third consumer
// (`rpc_warm_http`) hand-builds range URLs instead. That is why the PATH lives
// here too — it was spelled five times, and the spelling is the half that
// actually rotted.

/// One model file a peer is willing to serve.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelFileInfo {
    pub name: String,
    pub size_bytes: u64,
    /// Hex BLAKE-free SHA-256 of the file, so a consumer can verify what it
    /// got rather than trusting the length.
    pub sha256: String,
}

/// The body of `GET /internal/v1/models/list`.
///
/// Named for what it is. It was `ListResponse` on both sides, and
/// `routes_internal/mod.rs` already had to alias it on import
/// (`ListResponse as ModelFileListResponse`) — the repo had noticed the name
/// was too generic to travel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelFileListing {
    pub files: Vec<ModelFileInfo>,
}

/// Path of the listing endpoint. One spelling, used to register the route AND
/// to build the client URL.
pub const MODELS_LIST_PATH: &str = "/internal/v1/models/list";

/// Axum route pattern for the file endpoint. Kept beside [`model_file_url`]
/// because the two must agree; they differ only in how the segment is spelled
/// (`{name}` for the router, the value itself for a client).
pub const MODEL_FILE_ROUTE: &str = "/internal/v1/models/file/{name}";

/// `GET` URL for a peer's model listing. `base` is a scheme+authority with no
/// trailing slash, e.g. `http://10.0.0.4:9742`.
pub fn models_list_url(base: &str) -> String {
    format!("{}{MODELS_LIST_PATH}", base.trim_end_matches('/'))
}

/// `GET` URL for one model file on a peer.
///
/// `name` is interpolated verbatim — **the caller percent-encodes it.** This
/// crate takes no URL-encoding dependency (it is a 55-crate closure and the
/// package boundary is the point), and a builder that silently did not encode
/// would be worse than one that says so.
pub fn model_file_url(base: &str, name: &str) -> String {
    format!(
        "{}/internal/v1/models/file/{name}",
        base.trim_end_matches('/')
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ModelId;

    #[test]
    fn model_info_serde_roundtrip() {
        let model = ModelInfo {
            id: ModelId::from_u128(1),
            name: "Qwen3-Coder-30B".into(),
            repo: "Qwen/Qwen3-Coder-30B-GGUF".into(),
            file: "qwen3-coder-30b-q4_k_m.gguf".into(),
            size_bytes: 17_000_000_000,
            total_layers: 64,
            architecture: ModelArchitecture::Qwen,
            available_on: HashMap::new(),
            oicp_capabilities: CapabilityProfile::default(),
            quantization: "Q4_K_M".into(),
            min_memory_gb: 32,
            preferred_memory_gb: 48,
            supports_parallel_instances: true,
            supports_pipeline_shard: true,
        };
        let json = serde_json::to_string(&model).unwrap();
        let back: ModelInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "Qwen3-Coder-30B");
        assert_eq!(back.total_layers, 64);
        assert_eq!(back.architecture, ModelArchitecture::Qwen);
    }

    /// The trap `available_on`'s doc comment names, made falsifiable.
    ///
    /// This is not a test of serde. It is the guard on a specific repair
    /// someone will reach for: `/v1/models` cannot say which nodes hold a
    /// model, there is a field on `ModelInfo` shaped exactly like the
    /// answer, and filling it in is the obvious move. It does not work, and
    /// it does not fail loudly — `set_model_info` drops the serialise error
    /// on the floor, so the entry is never written and the model vanishes
    /// from the very endpoint the change was meant to improve.
    ///
    /// If this test ever fails, `NodeId`'s serialisation changed and the
    /// field may finally be usable. Until then, holders come from the OICP
    /// manifests — see `routes_inference::manifest_rows`.
    #[test]
    fn available_on_is_unusable_because_node_id_is_not_a_json_key() {
        let mut model = ModelInfo {
            id: ModelId::from_u128(1),
            name: "Qwen3-Coder-30B".into(),
            repo: String::new(),
            file: "m.gguf".into(),
            size_bytes: 1,
            total_layers: 0,
            architecture: ModelArchitecture::Qwen,
            available_on: HashMap::new(),
            oicp_capabilities: CapabilityProfile::default(),
            quantization: String::new(),
            min_memory_gb: 0,
            preferred_memory_gb: 0,
            supports_parallel_instances: false,
            supports_pipeline_shard: false,
        };
        // Empty is the only shape that works, and it is what every
        // construction site in the workspace passes.
        assert!(serde_json::to_vec(&model).is_ok());

        model
            .available_on
            .insert(NodeId::from_u128(2), ModelAvailability::Loaded);
        assert!(
            serde_json::to_vec(&model).is_err(),
            "populating `available_on` must still be a hard serialise \
             failure. If it now succeeds, the silent-disappearance hazard \
             this guards is gone and the field can be wired up properly — \
             but check the READ path round-trips before trusting it."
        );
    }

    #[test]
    fn model_availability_serde_roundtrip() {
        for avail in [
            ModelAvailability::Available,
            ModelAvailability::Loaded,
            ModelAvailability::Downloading,
        ] {
            let json = serde_json::to_string(&avail).unwrap();
            let back: ModelAvailability = serde_json::from_str(&json).unwrap();
            assert_eq!(avail, back);
        }
    }

    /// The path is a wire contract: a peer built from an older checkout calls
    /// this exact string. Pinned as a literal rather than rebuilt from the
    /// constants, because a test that rebuilds agrees with any change.
    #[test]
    fn the_model_file_paths_are_pinned() {
        assert_eq!(MODELS_LIST_PATH, "/internal/v1/models/list");
        assert_eq!(MODEL_FILE_ROUTE, "/internal/v1/models/file/{name}");
        assert_eq!(
            models_list_url("http://h:9742"),
            "http://h:9742/internal/v1/models/list"
        );
        assert_eq!(
            model_file_url("http://h:9742", "m.gguf"),
            "http://h:9742/internal/v1/models/file/m.gguf"
        );
        // A trailing slash on the base must not double up.
        assert_eq!(
            models_list_url("http://h:9742/"),
            "http://h:9742/internal/v1/models/list"
        );
    }

    #[test]
    fn the_listing_wire_shape_is_pinned() {
        let l = ModelFileListing {
            files: vec![ModelFileInfo {
                name: "m.gguf".into(),
                size_bytes: 7,
                sha256: "ab".into(),
            }],
        };
        assert_eq!(
            serde_json::to_string(&l).unwrap(),
            r#"{"files":[{"name":"m.gguf","size_bytes":7,"sha256":"ab"}]}"#
        );
    }
}
