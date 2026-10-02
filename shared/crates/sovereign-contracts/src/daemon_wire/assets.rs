// SPDX-License-Identifier: AGPL-3.0-or-later
//! The weights and models a daemon holds, and the one job that fetches them.
//!
//! Everything under `<data.dir>/models` belongs to the daemon that serves
//! from it — it decides where a GGUF lands, which GLiNER export is installed,
//! and when either is being fetched. A client asks; it does not write there
//! and it does not run its own downloader (sv-surface svt-7).
//!
//! Before this, three separate downloaders wrote into that root: the CLI
//! wizard's, the desktop's `setup_flow`, and a third in the desktop's
//! `commands/models.rs` with its own content-type sniff and its own
//! validation. Two of them ran in a process that owns none of those files.

use serde::{Deserialize, Serialize};

/// Which kind of artifact `POST /v1/admin/assets/download` should fetch.
///
/// A closed set, so an enum (ARCH principle 9). The two land in different
/// roots under the same data dir and are validated differently — a GGUF
/// against its advertised size and magic bytes, a GLiNER export against the
/// file layout its generation declares — which is why the kind is a field on
/// the request rather than two routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    /// A chat / embedding / editing model, into `<data.dir>/models/`.
    Gguf,
    /// A GLiNER ONNX export + tokenizer, into the GLiNER models root.
    Gliner,
}

impl AssetKind {
    /// The wire spelling, for tracing and job ids.
    pub const fn as_str(&self) -> &'static str {
        match self {
            AssetKind::Gguf => "gguf",
            AssetKind::Gliner => "gliner",
        }
    }
}

/// Body of `POST /v1/admin/assets/download`.
///
/// The fields a kind does not use are absent rather than ignored: a `gguf`
/// request with no `url` is refused by name, and so is a `gliner` request
/// carrying one. Accepting either and quietly doing something else is how a
/// caller ends up with a file it did not ask for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetDownloadRequest {
    /// Which root this lands in, and which validator runs.
    pub kind: AssetKind,
    /// `gguf`: the direct download URL (`setup_planner::hf_download_url`, or
    /// a BYOM link the caller resolved). Required for `gguf`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// `gguf`: the filename to write under `<data.dir>/models/`. Required for
    /// `gguf`, and a path separator in it is refused — a client naming a
    /// destination outside the models root is the one thing this route must
    /// not honour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// `gliner`: the model id (e.g. `gliner_small-v2.1`). Defaults to the
    /// daemon's configured id when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// `gguf`: the artifact's advertised size, which the validator uses as a
    /// floor. Absent means only the 1 MB sentinel and the GGUF magic apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_gb: Option<f64>,
}

/// Where one asset download has got to.
///
/// `Downloading` with `total: None` is a server that sent no
/// `Content-Length` — reported as absent, never as zero, because a renderer
/// dividing by it shows a finished bar over a running download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetDownloadState {
    /// Accepted, bytes moving.
    Downloading,
    /// The file is on disk and validated.
    Complete,
    /// It failed. `error` carries the daemon's sentence.
    Error,
    /// No job with that id in this daemon's lifetime.
    Unknown,
}

/// Answer of `GET /v1/admin/assets/download/{job}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetDownloadProgress {
    /// The job this describes.
    pub job_id: String,
    /// What it is fetching. `None` with [`AssetDownloadState::Unknown`] —
    /// a daemon with no record of the job does not know what it was for, and
    /// naming a kind there would be a fabricated field on a frame whose whole
    /// content is "I have never seen this" (ARCH principle 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<AssetKind>,
    /// Which artifact is moving right now. A GLiNER download fetches two
    /// files under one job, so this changes during the run.
    pub state: AssetDownloadState,
    /// The file currently being written, when one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Bytes written so far for `file`.
    pub downloaded: u64,
    /// Bytes expected for `file`, when the server said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Where it landed, on [`AssetDownloadState::Complete`] — the GGUF's
    /// full path, or the GLiNER model's directory.
    ///
    /// The DAEMON answers this because the daemon chose it. A client that
    /// re-derived `<its own data root>/models/<file>` would be right only
    /// while the two processes resolve the same root, and the slot path it
    /// then writes into config is what the daemon has to open (ARCH
    /// principle 8: one accessor per path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Why it failed. Populated only with [`AssetDownloadState::Error`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Answer of `GET /internal/ner/model` — is the entity extractor's model
/// installed on the machine that would run it.
///
/// The daemon answers because the daemon is where the extractor loads and
/// `models_root()` is under the data root it owns. The desktop probed its own
/// filesystem for this, which is the same answer only while the two processes
/// share a host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NerModelStatus {
    /// Both files resolve where the generation's layout expects them.
    pub installed: bool,
    /// The id this daemon would load (`SOVEREIGN_GLINER_MODEL_ID`, else the
    /// default) — not a constant the client hardcodes.
    pub model_id: String,
    /// Where the files go, for the "install" affordance to name.
    pub expected_path: String,
    /// Rough download size, so a client can warn before a 600 MB fetch.
    pub size_estimate_mb: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kind's wire spelling and its `as_str` are one string. Watched
    /// fail: change either arm alone.
    #[test]
    fn asset_kind_spells_itself_the_same_way_twice() {
        for k in [AssetKind::Gguf, AssetKind::Gliner] {
            assert_eq!(
                serde_json::to_string(&k).expect("serialize"),
                format!("\"{}\"", k.as_str())
            );
        }
    }

    /// An unsent `Content-Length` stays absent on the wire rather than
    /// arriving as `0`. Watched fail: drop the `skip_serializing_if` and
    /// `total` renders as `null`, which `total ?? 0` turns into a complete
    /// bar over a running download.
    #[test]
    fn an_unknown_total_is_absent_not_zero() {
        let p = AssetDownloadProgress {
            job_id: "j1".into(),
            kind: Some(AssetKind::Gguf),
            state: AssetDownloadState::Downloading,
            file: Some("m.gguf".into()),
            downloaded: 42,
            total: None,
            path: None,
            error: None,
        };
        let json = serde_json::to_string(&p).expect("serialize");
        assert!(!json.contains("total"), "{json}");
        assert!(!json.contains("error"), "{json}");
        assert!(json.contains(r#""downloaded":42"#), "{json}");
    }
}
