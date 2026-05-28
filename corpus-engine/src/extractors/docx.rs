//! DOCX sub-extractor for the described-asset dispatcher (AD-3).
//!
//! Reads Office Open XML word documents (`.docx`, `.docm`) without
//! pulling in a heavy dep. DOCX is a ZIP archive whose `word/document.xml`
//! holds the body. We unzip in-memory, stream the XML through
//! `quick-xml`, and concatenate text runs into a paragraph-separated
//! prose body — sufficient for the atlas pipeline's prose-shaped
//! consumption.
//!
//! No parsed-form parquet; DOCX is a prose document — its parsed
//! representation IS the prose body that ends up in
//! `ExtractedDoc.content`. The parsed_form path on the asset store
//! ledger stays `None`.

use std::io::Cursor;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;
use zip::ZipArchive;

use super::described_asset::{AssetExtraction, AssetSubExtractor, ExtractionTier};
use crate::asset_store::AssetStore;
use crate::error::{Error, Result};

pub struct DocxSubExtractor;

impl AssetSubExtractor for DocxSubExtractor {
    fn detect(&self, path: &Path, head_bytes: &[u8]) -> bool {
        if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|e| matches!(e.to_ascii_lowercase().as_str(), "docx" | "docm"))
            .unwrap_or(false)
        {
            return true;
        }
        // ZIP magic + we'll let `extract` confirm by looking for
        // `word/document.xml`. Without an extension hint, defer the
        // disambiguation to extract().
        head_bytes.starts_with(b"PK\x03\x04")
            && path
                .extension()
                .and_then(|s| s.to_str())
                .is_none()
    }

    fn extract(
        &self,
        path: &Path,
        bytes: &[u8],
        _sha256: &str,
        _store: &dyn AssetStore,
    ) -> Result<AssetExtraction> {
        let mut zip = ZipArchive::new(Cursor::new(bytes)).map_err(|e| {
            Error::Extraction(format!(
                "docx: cannot open zip in {}: {e}",
                path.display()
            ))
        })?;
        // The word body is always at `word/document.xml`; alternative
        // payloads (`word/document2.xml`) exist on extreme edge cases
        // we ignore for Phase 1.
        let mut entry = zip.by_name("word/document.xml").map_err(|e| {
            Error::Extraction(format!(
                "docx: word/document.xml not in {}: {e}",
                path.display()
            ))
        })?;
        let mut xml = Vec::with_capacity(entry.size() as usize);
        std::io::copy(&mut entry, &mut xml).map_err(Error::Io)?;
        drop(entry);

        let mut text = String::with_capacity(xml.len() / 2);
        let mut reader = XmlReader::from_reader(xml.as_slice());
        reader.config_mut().trim_text(false);
        let mut in_run = false;
        let mut paragraph_buf = String::new();
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let name = e.name();
                    let local = std::str::from_utf8(name.local_name().as_ref())
                        .unwrap_or("")
                        .to_string();
                    if local == "t" {
                        in_run = true;
                    }
                }
                Ok(Event::End(e)) => {
                    let name = e.name();
                    let local = std::str::from_utf8(name.local_name().as_ref())
                        .unwrap_or("")
                        .to_string();
                    if local == "t" {
                        in_run = false;
                    } else if local == "p" {
                        if !paragraph_buf.trim().is_empty() {
                            text.push_str(paragraph_buf.trim());
                            text.push_str("\n\n");
                        }
                        paragraph_buf.clear();
                    } else if local == "br" {
                        paragraph_buf.push('\n');
                    }
                }
                Ok(Event::Empty(e)) => {
                    let name = e.name();
                    let local = std::str::from_utf8(name.local_name().as_ref())
                        .unwrap_or("")
                        .to_string();
                    if local == "br" {
                        paragraph_buf.push('\n');
                    } else if local == "tab" {
                        paragraph_buf.push('\t');
                    }
                }
                Ok(Event::Text(e)) if in_run => {
                    let txt = e.unescape().unwrap_or_default();
                    paragraph_buf.push_str(&txt);
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    return Err(Error::Extraction(format!(
                        "docx: xml parse error in {}: {e}",
                        path.display()
                    )));
                }
                _ => {}
            }
            buf.clear();
        }
        if !paragraph_buf.trim().is_empty() {
            text.push_str(paragraph_buf.trim());
            text.push('\n');
        }

        // Trim trailing whitespace + collapse runs of blank lines.
        let body = text.trim().to_string();
        let description = if body.is_empty() {
            format!("DOCX: empty document ({} bytes raw)", bytes.len())
        } else {
            body
        };

        Ok(AssetExtraction {
            description,
            asset_kind: "docx".into(),
            tier: ExtractionTier::Prose,
            mime: Some(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                    .into(),
            ),
            parsed_form: None,
        })
    }

    fn name(&self) -> &'static str {
        "docx"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn build_test_docx(paragraphs: &[&str]) -> Vec<u8> {
        // Hand-roll the minimum valid DOCX shape calamine wouldn't
        // touch — one document.xml under `word/`, namespaces stripped
        // to local names since our parser walks local names.
        let buf: Vec<u8> = Vec::new();
        let cursor = Cursor::new(buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let opts: SimpleFileOptions = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("word/document.xml", opts).unwrap();
        let mut body = String::new();
        body.push_str(r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#);
        for p in paragraphs {
            body.push_str(&format!(
                "<w:p><w:r><w:t>{}</w:t></w:r></w:p>",
                p.replace('<', "&lt;").replace('>', "&gt;")
            ));
        }
        body.push_str("</w:body></w:document>");
        zip.write_all(body.as_bytes()).unwrap();
        let cursor = zip.finish().unwrap();
        cursor.into_inner()
    }

    struct NullStore;
    impl AssetStore for NullStore {
        fn put_raw(
            &self,
            _: &[u8],
            _: Option<&str>,
            _: Option<&str>,
            _: &str,
        ) -> Result<crate::asset_store::AssetReceipt> {
            unreachable!("not used in docx sub-extractor")
        }
        fn put_parsed(
            &self,
            _: &str,
            _: &str,
            _: &[u8],
        ) -> Result<std::path::PathBuf> {
            unreachable!()
        }
        fn record_parsed_form(&self, _: &str, _: &Path) -> Result<()> {
            unreachable!()
        }
        fn lookup(
            &self,
            _: &str,
        ) -> Result<Option<crate::asset_store::LedgerEntry>> {
            Ok(None)
        }
        fn entries(&self) -> Result<Vec<crate::asset_store::LedgerEntry>> {
            Ok(Vec::new())
        }
        fn raw_path(&self, _: &str) -> std::path::PathBuf {
            std::path::PathBuf::new()
        }
        fn root(&self) -> &Path {
            Path::new("")
        }
    }

    #[test]
    fn extract_recovers_body_text() {
        let docx = build_test_docx(&["Hello world.", "Second paragraph."]);
        let store = NullStore;
        let r = DocxSubExtractor
            .extract(Path::new("test.docx"), &docx, "sha", &store)
            .unwrap();
        assert!(r.description.contains("Hello world."));
        assert!(r.description.contains("Second paragraph."));
        assert_eq!(r.asset_kind, "docx");
        assert!(matches!(r.tier, ExtractionTier::Prose));
        assert!(r.parsed_form.is_none());
    }

    #[test]
    fn empty_document_has_safe_fallback_description() {
        let docx = build_test_docx(&[]);
        let store = NullStore;
        let r = DocxSubExtractor
            .extract(Path::new("empty.docx"), &docx, "sha", &store)
            .unwrap();
        assert!(r.description.contains("empty document"));
    }

    #[test]
    fn detect_by_extension() {
        assert!(DocxSubExtractor.detect(Path::new("a.docx"), &[]));
        assert!(!DocxSubExtractor.detect(Path::new("a.txt"), &[]));
    }
}
