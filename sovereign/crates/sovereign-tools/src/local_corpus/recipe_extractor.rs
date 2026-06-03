//! Wire `local_corpus`'s existing PDF text-extraction helper into the
//! `CorpusEngine` so recipes declaring
//! `extract = { type = "custom", kind = "pdf" }` can ingest PDFs
//! downloaded by `http_api + follow` (or any other acquirer).
//!
//! Why this lives in `sovereign-tools` and not in `corpus-engine`:
//! `pdf-extract` is a ~30 MB dep and corpus-engine deliberately stays
//! lean so cloud-build deployments that never see a PDF don't pay the
//! weight. The same `safe_extract_pdf_text` already powers the
//! folder-drop watched-folder flow; this module reuses it.
use std::sync::Arc;

use corpus_engine::{CorpusEngine, CustomExtractorFn, Error, Result};

use super::extract_stage::{safe_extract_pdf_text, SafeExtractError};

/// Register the `"pdf"` per-file extractor on `engine`. Idempotent:
/// re-registering overwrites. Call once at Runtime startup before
/// any ingest of a PDF-bearing recipe (`olc-opinions`,
/// `scotus-opinions`, etc.). Bare-CLI flows that bypass this
/// registration will fail loudly at install time with a clear
/// "register before install" panic.
pub fn register_pdf_extractor(engine: &CorpusEngine) {
    let extractor: CustomExtractorFn = Arc::new(|path| {
        safe_extract_pdf_text(path).map_err(|e| match e {
            SafeExtractError::Encrypted => Error::Extraction(format!(
                "pdf is encrypted/password-protected: {}",
                path.display()
            )),
            SafeExtractError::Parse(msg) => {
                Error::Extraction(format!("pdf parse error in {}: {msg}", path.display()))
            }
            SafeExtractError::Panic(msg) => {
                Error::Extraction(format!("pdf-extract panicked on {}: {msg}", path.display()))
            }
            SafeExtractError::Other(msg) => {
                Error::Extraction(format!("pdf extract error on {}: {msg}", path.display()))
            }
        }) as Result<String>
    });
    engine.register_extractor("pdf", extractor);
}
