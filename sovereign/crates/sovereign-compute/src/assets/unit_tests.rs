// SPDX-License-Identifier: AGPL-3.0-or-later
//! The download deciders' unit tests — see `assets.rs`.

use super::*;

/// A `gguf` request missing either half is refused by NAME, so the caller
/// learns which field. Watched fail: replace the `ok_or` with
/// `unwrap_or_default()` and a request with no url silently downloads
/// from the empty string.
#[test]
fn a_gguf_request_names_the_field_it_is_missing() {
    let mut req = AssetDownloadRequest {
        kind: AssetKind::Gguf,
        url: None,
        file: Some("m.gguf".into()),
        model_id: None,
        expected_gb: None,
    };
    assert!(plan(&req).unwrap_err().contains("`url`"));
    req.url = Some("https://x/m.gguf".into());
    req.file = None;
    assert!(plan(&req).unwrap_err().contains("`file`"));
}

/// The destination cannot leave the models root. Watched fail: drop the
/// separator check and `file: "../../.ssh/authorized_keys"` writes there.
#[test]
fn a_gguf_destination_cannot_escape_the_models_root() {
    for bad in ["../m.gguf", "a/b.gguf", "a\\b.gguf", ""] {
        let req = AssetDownloadRequest {
            kind: AssetKind::Gguf,
            url: Some("https://x/m.gguf".into()),
            file: Some(bad.into()),
            model_id: None,
            expected_gb: None,
        };
        assert!(
            plan(&req).is_err(),
            "`{bad}` must be refused as a destination"
        );
    }
}

/// A `gliner` request carrying gguf fields is REFUSED, not silently
/// half-honoured. The two kinds resolve their URL differently and a
/// request that mixes them is asking for something this route cannot do.
#[test]
fn a_gliner_request_refuses_gguf_fields() {
    let req = AssetDownloadRequest {
        kind: AssetKind::Gliner,
        url: Some("https://x/m.onnx".into()),
        file: None,
        model_id: Some("gliner_small-v2.1".into()),
        expected_gb: None,
    };
    assert!(plan(&req).unwrap_err().contains("model_id"));

    let ok = AssetDownloadRequest {
        kind: AssetKind::Gliner,
        url: None,
        file: None,
        model_id: Some("gliner_small-v2.1".into()),
        expected_gb: None,
    };
    match plan(&ok).expect("a model_id-only request is honoured") {
        Plan::Gliner { model_id } => assert_eq!(model_id, "gliner_small-v2.1"),
        Plan::Gguf { .. } => panic!("kind gliner must not plan a gguf fetch"),
    }
}

/// A server that sent no `Content-Length` is reported as ABSENT, and a
/// real total survives the sentinel encoding.
///
/// Watched fail: store `0` for the unknown case and the progress route
/// reports a 0-byte artifact, which renders as a finished download.
#[test]
fn an_absent_content_length_is_reported_absent() {
    let job = Arc::new(AssetDownload {
        kind: AssetKind::Gguf,
        dest: std::path::PathBuf::from("/models/m.gguf"),
        file: Mutex::new(None),
        downloaded: AtomicU64::new(0),
        total: AtomicU64::new(NO_TOTAL),
        outcome: Mutex::new(None),
    });
    job.observe("m.gguf", 10, None);
    let p = job.progress("j");
    assert_eq!(p.total, None);
    assert_eq!(p.downloaded, 10);
    assert_eq!(p.file.as_deref(), Some("m.gguf"));
    assert_eq!(p.state, AssetDownloadState::Downloading);

    job.observe("m.gguf", 20, Some(100));
    assert_eq!(job.progress("j").total, Some(100));
}

/// A failed download reports Error WITH the daemon's sentence, never a
/// Complete with an empty file. Watched fail: map the outcome's `Err` to
/// `Complete` and this goes red.
#[test]
fn a_failed_download_reports_its_reason() {
    let job = Arc::new(AssetDownload {
        kind: AssetKind::Gliner,
        dest: std::path::PathBuf::from("/models/gliner/x"),
        file: Mutex::new(Some("model.onnx".into())),
        downloaded: AtomicU64::new(1),
        total: AtomicU64::new(NO_TOTAL),
        outcome: Mutex::new(Some(Err("fetch https://x: HTTP 404".into()))),
    });
    let p = job.progress("j2");
    assert_eq!(p.state, AssetDownloadState::Error);
    assert_eq!(p.error.as_deref(), Some("fetch https://x: HTTP 404"));
}
