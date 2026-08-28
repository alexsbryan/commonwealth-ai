// SPDX-License-Identifier: AGPL-3.0-or-later
//! Headless OCR install — hand the daemon's `LocalCorpusManager` an `OcrCtx`.
//!
//! # Why this file exists
//!
//! Everything needed to OCR a scanned PDF on a headless box was already in
//! the tree; the one missing link was a caller. `OcrCtx` is plain data,
//! `LocalCorpusManager::set_ocr_ctx` is a per-instance setter, and the
//! daemon already builds that manager (`bootstrap::setup_watched_folders`).
//! Only the desktop ever called the setter, and it resolved its assets from
//! three `AppHandle` bundle probes that do not exist off-desktop. This
//! module is the same install with the bundle probes replaced by
//! env-then-`data_dir` resolution.
//!
//! Without it, `svrn corpus watch --ocr` on a server registers the corpus
//! with `with_ocr: true` and then every scanned PDF lands in
//! `WatchedFolderState.failed_files` with the reason "OCR is enabled but the
//! daemon's OcrCtx isn't installed" (`watched/worker.rs`). Reported, not
//! silent — but for a litigation corpus, "reported" still means the
//! discovery never got indexed.
//!
//! # The seam is an env var, deliberately
//!
//! `PaddleEngine::from_ctx` resolves its models through
//! `paddle::models_root()`, which reads `SOVEREIGN_PADDLE_OCR_MODEL_DIR` and
//! otherwise falls back to a hardcoded `~/.svrnmesh/models/paddle-ocr` —
//! **not** rebrand-aware, so a `~/.svrnmesh` install misses it. `OcrCtx`
//! carries no model-path field, so pointing the engine at a staged asset
//! directory means setting that variable. We set it (as the desktop does)
//! rather than introducing a second resolution path for the same thing.
//!
//! # Glassbox
//!
//! Every exit from [`install_ocr_ctx`] logs, including the two silent ones:
//! the feature being compiled out, and the models not resolving. An operator
//! who turned OCR on in `corpus watch` and sees nothing happen must be able
//! to find out why from the daemon log alone.

use std::path::Path;
// Only the `ocr` arm and its path helpers build owned paths. Without the
// gate this is an unused-import warning in every default build, which is
// the kind of noise that trains a reader to ignore warnings.
#[cfg(feature = "ocr")]
use std::path::PathBuf;

use sovereign_tools::local_corpus::LocalCorpusManager;

/// Install an `OcrCtx` on `manager`, if this build has the `ocr` feature and
/// the PaddleOCR assets can be found.
///
/// `cleanup_model` must be a GGUF **file stem** the daemon's
/// `/v1/chat/completions` route can resolve (slots are registered under
/// their file stem) — never a slot alias like `"fast"`, which 503s. A wrong
/// value does not fail: the page keeps its raw un-polished OCR text with a
/// `<!-- raw OCR (cleanup unavailable) -->` marker, which is exactly the
/// kind of quality loss nobody reports. Hence the explicit warn below when
/// it arrives empty.
///
/// Never fatal. A daemon with no OCR is a supported posture; a daemon that
/// refuses to boot because an optional asset is missing is not.
pub(super) async fn install_ocr_ctx(
    manager: &LocalCorpusManager,
    data_dir: &Path,
    daemon_base_url: String,
    cleanup_model: String,
) {
    #[cfg(not(feature = "ocr"))]
    {
        // Bind the unused params so the no-op arm compiles identically to
        // the real one — a signature that drifts between cfgs is a build
        // break waiting for whoever first turns the feature on.
        let _ = (manager, data_dir, daemon_base_url, cleanup_model);
        tracing::info!(
            "ocr:unavailable reason=feature_not_compiled — this daemon was built \
             without `--features ocr`, so scanned PDFs in a `corpus watch --ocr` \
             folder will be reported as scanned_no_text rather than indexed"
        );
    }

    #[cfg(feature = "ocr")]
    {
        use sovereign_tools::local_corpus::ocr::paddle::DEFAULT_MODEL_ID;
        use sovereign_tools::local_corpus::ocr::{OcrCtx, OcrEngineKind};

        let roots = paddle_model_roots(
            data_dir,
            sovereign_tools::local_corpus::ocr::paddle::model_root_override(),
        );
        let Some(model_root) = roots
            .iter()
            .find(|root| model_set_complete(root, DEFAULT_MODEL_ID))
            .cloned()
        else {
            tracing::warn!(
                probed = ?roots,
                model_set = DEFAULT_MODEL_ID,
                "ocr:unavailable reason=models_not_found — none of the probed roots \
                 holds {DEFAULT_MODEL_ID}/{{det.onnx,rec.onnx,dict.txt}}. Stage them \
                 or set SOVEREIGN_PADDLE_OCR_MODEL_DIR; scanned PDFs stay unindexed \
                 until then"
            );
            return;
        };

        // The engine reads this on construction. Setting it here (rather
        // than teaching `models_root()` about `data_dir`) keeps one
        // resolution path for both the desktop and the daemon.
        std::env::set_var("SOVEREIGN_PADDLE_OCR_MODEL_DIR", &model_root);

        let pdfium_probes = pdfium_probes(data_dir, std::env::var("SOVEREIGN_PDFIUM_LIB").ok());
        let pdfium_lib_path = pdfium_probes.iter().find(|p| p.exists()).cloned();
        if pdfium_lib_path.is_none() {
            tracing::warn!(
                probed = ?pdfium_probes,
                "ocr:pdfium_not_staged — falling back to pdfium-render's bundled/system \
                 search. On an air-gapped box there is usually no system libpdfium, and \
                 without it no PDF can be rasterized, so OCR will produce nothing. Stage \
                 the library or set SOVEREIGN_PDFIUM_LIB"
            );
        }

        if cleanup_model.is_empty() {
            tracing::warn!(
                "ocr:cleanup_model_unset — no chat model is configured, so the OCR \
                 cleanup pass cannot run. Pages will carry raw recognizer output with a \
                 `<!-- raw OCR (cleanup unavailable) -->` marker"
            );
        }

        let ctx = OcrCtx {
            // Inert under `engine: Paddle`; `OcrCtx` has no Option for them.
            tesseract_bin: PathBuf::from("tesseract"),
            tessdata_dir: PathBuf::new(),
            pdfium_lib_path,
            daemon_base_url,
            cleanup_model,
            dpi: 300,
            tesseract_timeout_secs: 30,
            cleanup_timeout_secs: 30,
            engine: OcrEngineKind::Paddle,
        };
        tracing::info!(
            engine = "paddleocr",
            paddle_model_root = %model_root.display(),
            pdfium = ?ctx.pdfium_lib_path,
            cleanup_model = %ctx.cleanup_model,
            daemon_base_url = %ctx.daemon_base_url,
            "ocr:installed — `corpus watch --ocr` folders will read scanned PDFs"
        );
        manager.set_ocr_ctx(ctx).await;
    }
}

/// Candidate PaddleOCR model **roots**, in precedence order. A root holds
/// `<model_id>/` — not the model files themselves.
///
/// `data_dir` before `~/.svrnmesh` is the load-bearing ordering: a
/// rebranded install (`~/.svrnmesh`) has a `data_dir` the hardcoded
/// `paddle::models_root()` fallback would never look at.
///
/// The env override arrives as a parameter rather than being read here so
/// the precedence is a pure function of its inputs — testable without
/// mutating process-global state, which under a parallel test runner is not
/// a test but a race.
#[cfg(feature = "ocr")]
fn paddle_model_roots(data_dir: &Path, env_override: Option<PathBuf>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(env_path) = env_override {
        roots.push(env_path);
    }
    roots.push(data_dir.join("models").join("paddle-ocr"));
    roots.push(
        sovereign_contracts::rebrand::svrnmesh_root()
            .join("models")
            .join("paddle-ocr"),
    );
    roots
}

/// True only when all three files of a model set are present, so a `Some`
/// from the caller means "Paddle can run" rather than "a directory exists".
/// Discovering a missing `rec.onnx` per-document instead is the failure this
/// avoids.
#[cfg(feature = "ocr")]
fn model_set_complete(root: &Path, model_id: &str) -> bool {
    let set = root.join(model_id);
    set.join("det.onnx").is_file()
        && set.join("rec.onnx").is_file()
        && set.join("dict.txt").is_file()
}

/// Candidate `libpdfium` paths, in precedence order. All three platform
/// library names are probed at each root so a kit staged on one OS and
/// inspected on another still resolves.
#[cfg(feature = "ocr")]
fn pdfium_probes(data_dir: &Path, env_override: Option<String>) -> Vec<PathBuf> {
    const LIB_NAMES: [&str; 3] = ["libpdfium.so", "libpdfium.dylib", "pdfium.dll"];
    let mut probes: Vec<PathBuf> = Vec::new();
    if let Some(env_path) = env_override {
        probes.push(PathBuf::from(env_path));
    }
    let mut roots = vec![data_dir.join("lib"), data_dir.to_path_buf()];
    roots.push(sovereign_contracts::rebrand::svrnmesh_root().join("lib"));
    for root in &roots {
        for lib in LIB_NAMES {
            probes.push(root.join("pdfium").join(lib));
            probes.push(root.join(lib));
        }
    }
    probes
}

#[cfg(all(test, feature = "ocr"))]
mod tests {
    use super::*;

    /// The rebrand trap: `paddle::models_root()`'s own fallback is a
    /// hardcoded `~/.svrnmesh/models/paddle-ocr`, so a `~/.svrnmesh` install
    /// only finds staged models because `data_dir` is probed FIRST. Assert
    /// the ordering, not merely the membership.
    #[test]
    fn data_dir_root_outranks_the_hardcoded_home_fallback() {
        let data_dir = Path::new("/srv/svrnmesh");
        let roots = paddle_model_roots(data_dir, None);
        let staged = roots
            .iter()
            .position(|p| p == &data_dir.join("models").join("paddle-ocr"))
            .expect("data_dir root must be probed");
        // svrnmesh_root() is infallible and is now pushed unconditionally;
        // it must still come after the staged root.
        let fallback = sovereign_contracts::rebrand::svrnmesh_root()
            .join("models")
            .join("paddle-ocr");
        if let Some(at) = roots.iter().position(|p| p == &fallback) {
            assert!(
                staged < at,
                "data_dir root must outrank the hardcoded ~/.svrnmesh fallback"
            );
        }
    }

    /// A directory with two of three files must NOT resolve — the whole
    /// point of validating the set is that "OCR is available" is decided
    /// once at boot rather than per document.
    #[test]
    fn a_partial_model_set_does_not_resolve() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("models").join("paddle-ocr");
        let set = root.join("ppocr-en-v4v5");
        std::fs::create_dir_all(&set).expect("mkdir");
        std::fs::write(set.join("det.onnx"), b"x").expect("write det");
        std::fs::write(set.join("dict.txt"), b"x").expect("write dict");
        assert!(
            !model_set_complete(&root, "ppocr-en-v4v5"),
            "a set missing rec.onnx must not count as resolvable"
        );
        std::fs::write(set.join("rec.onnx"), b"x").expect("write rec");
        assert!(
            model_set_complete(&root, "ppocr-en-v4v5"),
            "a complete set must resolve"
        );
    }

    /// `SOVEREIGN_PDFIUM_LIB` is the documented override in the on-prem
    /// runbook; if it ever stopped being probed first, a staged-but-wrong
    /// library in `data_dir` would silently win.
    #[test]
    fn pdfium_env_override_is_probed_first() {
        let probes = pdfium_probes(
            Path::new("/srv/svrnmesh"),
            Some("/opt/libpdfium.so".to_string()),
        );
        assert_eq!(
            probes.first().map(PathBuf::as_path),
            Some(Path::new("/opt/libpdfium.so"))
        );
    }

    /// With no override, `data_dir` must still be probed ahead of `$HOME` —
    /// the same rebrand trap as the model root, for the library.
    #[test]
    fn pdfium_probes_data_dir_before_home() {
        let data_dir = Path::new("/srv/svrnmesh");
        let probes = pdfium_probes(data_dir, None);
        let staged = probes
            .iter()
            .position(|p| p == &data_dir.join("lib").join("libpdfium.so"))
            .expect("data_dir/lib must be probed");
        let fallback = sovereign_contracts::rebrand::svrnmesh_root()
            .join("lib")
            .join("libpdfium.so");
        if let Some(at) = probes.iter().position(|p| p == &fallback) {
            assert!(staged < at, "data_dir/lib must outrank ~/.svrnmesh/lib");
        }
    }
}
