// SPDX-License-Identifier: AGPL-3.0-or-later
//! The node-to-node model-transfer wire: `/internal/v1/models/*`.
//!
//! Moved from `commonwealth_core::model` (phase-b pb-serve-sheds-core) so
//! the serving host speaks it without linking commonwealth-core; that path
//! re-exports every item here.

use serde::{Deserialize, Serialize};

// ── Peer-to-peer model file distribution ─────────────────────────────
//
// The wire vocabulary for `/internal/v1/models/*`, which one peer serves
// (`commonwealth-api::routes_internal::model_files`) and another consumes
// (`sovereign-serving-host::model_fetch`, `::rpc_warm_http`). It lives here because
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
    /// File name, the segment [`model_file_url`] carries.
    pub name: String,
    /// Length in bytes.
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
    /// Every file the peer serves.
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
