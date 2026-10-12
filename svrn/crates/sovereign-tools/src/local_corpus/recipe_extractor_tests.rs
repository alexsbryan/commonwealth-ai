// SPDX-License-Identifier: AGPL-3.0-or-later
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use corpus_index::ingest_port::{CustomAcquirerFn, CustomExtractorFn, IngestPluginPort};

use super::register_pdf_extractor;
use crate::local_corpus::ocr::{OcrCtx, MIN_TEXT_LAYER_CHARS};

/// A port that keeps what was registered, so the test can call it.
#[derive(Default)]
struct Captured(Mutex<Vec<(String, CustomExtractorFn)>>);

impl IngestPluginPort for Captured {
    fn register_acquirer(&self, _kind: &str, _acquirer: CustomAcquirerFn) {}
    fn register_extractor(&self, kind: &str, extractor: CustomExtractorFn) {
        self.0.lock().unwrap().push((kind.to_string(), extractor));
    }
}

impl Captured {
    fn pdf(&self) -> CustomExtractorFn {
        let registered = self.0.lock().unwrap();
        let kinds: Vec<&str> = registered.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, ["pdf"], "one extractor, of kind \"pdf\"");
        registered[0].1.clone()
    }
}

/// A one-page PDF whose text layer is `text` in Helvetica, with a correct
/// xref so a strict reader takes it.
fn write_pdf(dir: &Path, name: &str, text: &str) -> PathBuf {
    let content = if text.is_empty() {
        "BT ET".to_string()
    } else {
        format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET")
    };
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
    }
    let xref = pdf.len();
    pdf.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    ));
    for offset in offsets {
        pdf.push_str(&format!("{offset:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    let path = dir.join(name);
    std::fs::write(&path, pdf).unwrap();
    path
}

const LAW: &str = "Be it enacted by the Senate and House of Representatives of the United States";

/// An OCR context whose recognizer cannot run, so reaching OCR is visible
/// as OCR's own failure or its unread-page marker.
fn unrunnable_ocr() -> OcrCtx {
    OcrCtx::for_test(
        PathBuf::from("/nonexistent/tesseract"),
        PathBuf::from("/nonexistent/tessdata"),
        "http://127.0.0.1:9".into(),
    )
}

#[test]
fn a_born_digital_pdf_reads_its_text_layer() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write_pdf(dir.path(), "law.pdf", LAW);
    for ocr in [None, Some(unrunnable_ocr())] {
        let port = Captured::default();
        let with_ocr = ocr.is_some();
        register_pdf_extractor(&port, ocr);
        let text = (port.pdf())(&pdf).unwrap();
        assert!(
            text.contains("Be it enacted"),
            "ocr installed: {with_ocr}; read {text:?}"
        );
    }
}

#[test]
fn a_thin_text_layer_without_ocr_is_its_text_layer_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write_pdf(dir.path(), "scan.pdf", "");
    let port = Captured::default();
    register_pdf_extractor(&port, None);
    let text = (port.pdf())(&pdf).unwrap();
    assert!(text.trim().len() < MIN_TEXT_LAYER_CHARS, "read {text:?}");
}

#[test]
fn a_thin_text_layer_with_ocr_installed_is_read_by_ocr() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write_pdf(dir.path(), "scan.pdf", "");
    let port = Captured::default();
    register_pdf_extractor(&port, Some(unrunnable_ocr()));
    match (port.pdf())(&pdf) {
        Err(e) => assert!(e.to_string().contains("pdf ocr"), "{e}"),
        Ok(text) => assert!(
            text.contains("<!-- page"),
            "OCR's output marks a page it could not read; the text layer is \
             empty, so anything else came from somewhere else: {text:?}"
        ),
    }
}
