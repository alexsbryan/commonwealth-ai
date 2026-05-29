//! RFC-5322 / MIME email extractor (Phase 2 of the architecture-over-
//! Enron push).
//!
//! The first driver that exercises Phase 1's described-asset substrate
//! end-to-end. Walks a folder of email files (maildir / mbox / .eml),
//! parses each through `mailparse`, and emits one
//! [`ExtractedDoc`](super::ExtractedDoc) per message — `content` is the
//! best-available text body, `metadata` carries the parsed headers + a
//! `thread_id` derived from the In-Reply-To / References chain (falling
//! back to Message-ID for thread roots).
//!
//! Attachments dispatch to the described-asset path. When an
//! [`crate::asset_store::AssetStore`] + an
//! [`crate::extractors::described_asset::AssetSubExtractorRegistry`]
//! are installed on the extractor, each attachment becomes (1) raw
//! bytes in the asset store, (2) an [`Asset`](crate::enrichment::atlas::atoms::Asset)
//! atom written to the per-corpus sidecar, and (3) an
//! [`EdgeType::Attaches`](crate::enrichment::atlas::edges::EdgeType)
//! edge from the email's synthetic message-atom id to the asset atom
//! id, written to the per-corpus edges sidecar.
//!
//! The "message atom id" is derived deterministically from the
//! Message-ID header so a future atlas-enrichment pass that re-extracts
//! the same body can produce the same atom id and have the Attaches
//! edge resolve cleanly.

use std::collections::VecDeque;
use std::fs;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mailparse::{parse_mail, MailHeaderMap, ParsedMail};

use super::{slug, ExtractedDoc, Extractor};
use crate::asset_store::AssetStoreHandle;
use crate::enrichment::atlas::atoms::{Asset, AtomEnvelope, AtomId};
use crate::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
use crate::extractors::described_asset::{
    build_asset_atom, AssetSubExtractor, AssetSubExtractorRegistry, OpaqueFallback,
};
use crate::error::{Error, Result};

/// Configuration knob shared between the recipe schema and the
/// extractor implementation. See
/// [`crate::recipe::ExtractorConfig::Email`].
#[derive(Debug, Clone)]
pub struct EmailExtractorConfig {
    /// Skip messages whose body (after MIME decoding) exceeds this
    /// many bytes. Keeps a stray 200MB HTML newsletter from
    /// dominating a single ExtractedDoc; the asset goes through the
    /// described-asset path if attachment-dispatch is wired.
    pub max_body_bytes: usize,
    /// Maximum bytes the attachment dispatcher will load into RAM per
    /// asset (sub-extractors run synchronously). 0 = use the
    /// described-asset dispatcher's default (64 MiB).
    pub max_attachment_bytes: u64,
}

impl Default for EmailExtractorConfig {
    fn default() -> Self {
        Self {
            // 4 MiB — comfortably bigger than the 99.5th-percentile
            // Enron message body; the long-tail bytes go through the
            // asset store anyway.
            max_body_bytes: 4 * 1024 * 1024,
            max_attachment_bytes: 0,
        }
    }
}

/// RFC-5322 + MIME folder walker. Handles maildir layout (one file per
/// message, no `.eml` extension) and `.eml` files. `.mbox` support is
/// deferred — re-export `.mbox` to maildir if you need it for now.
pub struct EmailExtractor {
    pub config: EmailExtractorConfig,
    /// Optional asset store + dispatcher for attachments. When `None`,
    /// attachments still appear in `metadata.attachments` but no Asset
    /// atom / Attaches edge is written.
    pub asset_dispatch: Option<EmailAssetDispatch>,
}

/// Bundle the email extractor needs to dispatch attachments through
/// Phase 1's substrate. Built by the engine factory; passed in at
/// construction.
#[derive(Clone)]
pub struct EmailAssetDispatch {
    pub store: AssetStoreHandle,
    pub registry: AssetSubExtractorRegistry,
    pub asset_atoms_sidecar: PathBuf,
    pub asset_edges_sidecar: PathBuf,
}

impl EmailExtractor {
    pub fn new(config: EmailExtractorConfig) -> Self {
        Self {
            config,
            asset_dispatch: None,
        }
    }

    pub fn with_asset_dispatch(mut self, dispatch: EmailAssetDispatch) -> Self {
        self.asset_dispatch = Some(dispatch);
        self
    }
}

impl Extractor for EmailExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let mut files = Vec::new();
        walk(source_path, &mut files)?;
        files.sort();
        Ok(Box::new(EmailIterator {
            files: files.into(),
            config: self.config.clone(),
            dispatch: self.asset_dispatch.clone(),
        }))
    }
}

struct EmailIterator {
    files: VecDeque<PathBuf>,
    config: EmailExtractorConfig,
    dispatch: Option<EmailAssetDispatch>,
}

impl Iterator for EmailIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let path = self.files.pop_front()?;
            match self.parse_one(&path) {
                Ok(Some(doc)) => return Some(Ok(doc)),
                Ok(None) => continue,
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

impl EmailIterator {
    fn parse_one(&self, path: &Path) -> Result<Option<ExtractedDoc>> {
        let bytes = fs::read(path)
            .map_err(|e| Error::Extraction(format!("email: read {}: {e}", path.display())))?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let parsed = parse_mail(&bytes).map_err(|e| {
            Error::Extraction(format!(
                "email: rfc5322 parse failed for {}: {e}",
                path.display()
            ))
        })?;

        let from = header(&parsed, "From");
        let to = header(&parsed, "To");
        let cc = header(&parsed, "Cc");
        let bcc = header(&parsed, "Bcc");
        let date = header(&parsed, "Date");
        let subject = header(&parsed, "Subject");
        let message_id =
            header(&parsed, "Message-ID").unwrap_or_else(|| synthesize_message_id(path));
        let in_reply_to = header(&parsed, "In-Reply-To");
        let references_raw = header(&parsed, "References").unwrap_or_default();
        let references: Vec<String> = split_msgids(&references_raw);

        let thread_id = derive_thread_id(&references, &in_reply_to, &message_id);

        let (body, body_was_truncated) = collect_body(&parsed, self.config.max_body_bytes)?;
        let attachments = self.handle_attachments(&parsed, &message_id, path)?;
        tracing::debug!(
            path = %path.display(),
            message_id = %message_id,
            body_bytes = body.len(),
            body_truncated = body_was_truncated,
            attachments = attachments.len(),
            "email_rfc5322: parsed"
        );

        let metadata = serde_json::json!({
            "doc_type": "email",
            "message_id": message_id,
            "thread_id": thread_id,
            "from": from,
            "to": to,
            "cc": cc,
            "bcc": bcc,
            "date": date,
            "subject": subject,
            "in_reply_to": in_reply_to,
            "references": references,
            "body_was_truncated": body_was_truncated,
            "attachments": attachments,
            "source_path": path.to_string_lossy(),
        });

        let source_id = slug(&message_id);
        let title = subject.clone();
        // Prepend an RFC5322 header preamble to the body so downstream
        // domains see sender/recipient identities at extraction time.
        // StoredChunk only carries `(id, content, title)` to the domain
        // prompt — the structured `metadata` JSON above is preserved on
        // disk but the entity-extraction pass never sees it. Without
        // this preamble the `conversational` domain extracts only body
        // mentions and silently filters every sender ("the user is the
        // speaker — do not extract them"), which produced the headline
        // gap on enron-sample-multi-tiny: 7/35 canonical ground-truth
        // entities matched because Lay-in-Lay's-mailbox and Skilling-
        // in-Skilling's-mailbox were both dropped as "the user".
        // BusinessEmailDomain pairs with this preamble to surface them
        // as Person atoms. Body chunks past the first inherit the
        // preamble too — the redundancy is cheap and keeps every chunk
        // self-describing for the LLM.
        let content_with_headers = build_header_preamble(
            from.as_deref(),
            to.as_deref(),
            cc.as_deref(),
            date.as_deref(),
            subject.as_deref(),
        ) + &body;
        Ok(Some(ExtractedDoc {
            title,
            content: content_with_headers,
            url: None,
            source_id,
            metadata: Some(metadata),
            source_file: path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string()),
            embed_text: None,
        }))
    }

    fn handle_attachments(
        &self,
        parsed: &ParsedMail,
        message_id: &str,
        path: &Path,
    ) -> Result<Vec<serde_json::Value>> {
        let mut descriptors = Vec::new();
        let mut walk_stack: Vec<&ParsedMail> = vec![parsed];
        while let Some(part) = walk_stack.pop() {
            for sub in &part.subparts {
                walk_stack.push(sub);
            }
            if !is_attachment(part) {
                continue;
            }
            let body_bytes = part.get_body_raw().map_err(|e| {
                Error::Extraction(format!(
                    "email: decode attachment body in {}: {e}",
                    path.display()
                ))
            })?;
            if body_bytes.is_empty() {
                continue;
            }
            let filename = part_filename(part).unwrap_or_else(|| {
                let ct = part.ctype.mimetype.clone();
                format!("attachment.{}", default_ext_for_mime(&ct))
            });
            let mime = part.ctype.mimetype.clone();

            let descriptor = match &self.dispatch {
                Some(dispatch) => {
                    self.dispatch_attachment(dispatch, &body_bytes, &filename, &mime, message_id)?
                }
                None => serde_json::json!({
                    "original_filename": filename,
                    "mime": mime,
                    "size": body_bytes.len(),
                    "dispatched": false,
                }),
            };
            descriptors.push(descriptor);
        }
        Ok(descriptors)
    }

    fn dispatch_attachment(
        &self,
        dispatch: &EmailAssetDispatch,
        bytes: &[u8],
        filename: &str,
        mime: &str,
        message_id: &str,
    ) -> Result<serde_json::Value> {
        // 1. Put raw bytes into the asset store (idempotent on
        //    duplicate attachments shared across messages).
        let receipt = dispatch.store.put_raw(
            bytes,
            Some(filename),
            Some(mime),
            message_id,
        )?;
        // 2. Pick a sub-extractor + run it. Falls through to opaque
        //    fallback if the asset exceeds the configured cap.
        let max_bytes = if self.config.max_attachment_bytes == 0 {
            crate::extractors::described_asset::DescribedAssetExtractor::DEFAULT_MAX_BYTES_PER_ASSET
        } else {
            self.config.max_attachment_bytes
        };
        let extraction = if bytes.len() as u64 > max_bytes {
            OpaqueFallback.extract(Path::new(filename), bytes, &receipt.sha256, dispatch.store.as_ref())?
        } else {
            let head = &bytes[..512.min(bytes.len())];
            let extractors = dispatch.registry.snapshot();
            let mut picked: Option<&Arc<dyn AssetSubExtractor>> = None;
            for sub in &extractors {
                if sub.detect(Path::new(filename), head) {
                    picked = Some(sub);
                    break;
                }
            }
            let sub = picked.ok_or_else(|| {
                Error::Extraction(format!(
                    "email: no sub-extractor matched attachment {filename} — register OpaqueFallback last"
                ))
            })?;
            sub.extract(Path::new(filename), bytes, &receipt.sha256, dispatch.store.as_ref())?
        };
        if let Some(parsed_path) = extraction.parsed_form.as_deref() {
            dispatch.store.record_parsed_form(&receipt.sha256, parsed_path)?;
        }

        let asset_kind = extraction.asset_kind.clone();
        let tier_str = extraction.tier.as_str();

        // 3. Write the Asset atom to the sidecar.
        let atom = build_asset_atom(
            &receipt.sha256,
            &extraction.mime.clone().unwrap_or(mime.to_string()),
            &asset_kind,
            filename,
            receipt.size,
            extraction.parsed_form.clone(),
            None,
            message_id,
        );
        append_asset_atom(&dispatch.asset_atoms_sidecar, &atom)?;

        // 4. Write the Attaches edge.
        let source_atom_id = synthetic_message_atom_id(message_id);
        let edge = Edge {
            id: EdgeId::from_raw(format!(
                "edge-attaches-{}-{}",
                short_hash(message_id),
                &receipt.sha256[..16.min(receipt.sha256.len())]
            )),
            edge_type: EdgeType::Attaches,
            source: source_atom_id,
            target: Asset::make_id(&receipt.sha256),
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        };
        append_edge(&dispatch.asset_edges_sidecar, &edge)?;

        Ok(serde_json::json!({
            "sha256": receipt.sha256,
            "original_filename": filename,
            "mime": mime,
            "asset_kind": asset_kind,
            "extraction_tier": tier_str,
            "size": receipt.size,
            "parsed_form": extraction.parsed_form.as_ref().map(|p| p.to_string_lossy().to_string()),
            "dispatched": true,
        }))
    }
}

// ── Helpers ──────────────────────────────────────────────────

fn header(parsed: &ParsedMail, name: &str) -> Option<String> {
    parsed.headers.get_first_value(name).filter(|s| !s.is_empty())
}

fn split_msgids(refs: &str) -> Vec<String> {
    refs.split_whitespace()
        .filter_map(|s| {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.trim_matches(|c: char| c == '<' || c == '>' || c.is_whitespace()).to_string())
            }
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn derive_thread_id(
    references: &[String],
    in_reply_to: &Option<String>,
    message_id: &str,
) -> String {
    // Thread root convention: first ID in `References` if present
    // (RFC 5322 §3.6.4 places the root there), else In-Reply-To
    // trimmed to a single id, else the message itself.
    if let Some(first) = references.first() {
        return first.clone();
    }
    if let Some(irt) = in_reply_to {
        let parts = split_msgids(irt);
        if let Some(first) = parts.first() {
            return first.clone();
        }
    }
    message_id
        .trim_matches(|c: char| c == '<' || c == '>')
        .to_string()
}

fn is_attachment(part: &ParsedMail) -> bool {
    // RFC 2183: Content-Disposition: attachment → attachment.
    // Also treat any part with a filename parameter as attachment-
    // shaped (mailers vary on Content-Disposition presence).
    let disp = part
        .headers
        .get_first_value("Content-Disposition")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if disp.starts_with("attachment") {
        return true;
    }
    if disp.starts_with("inline") && disp.contains("filename=") {
        return true;
    }
    // Heuristic: any non-text/non-multipart part with a filename
    // parameter on Content-Type.
    let ct = part.ctype.mimetype.to_ascii_lowercase();
    if ct.starts_with("multipart/") || ct.starts_with("text/") {
        return false;
    }
    part.ctype.params.contains_key("name") || disp.contains("filename=")
}

fn part_filename(part: &ParsedMail) -> Option<String> {
    if let Some(name) = part.ctype.params.get("name") {
        return Some(name.clone());
    }
    let disp = part.headers.get_first_value("Content-Disposition")?;
    let lower = disp.to_ascii_lowercase();
    let key_idx = lower.find("filename=")?;
    let after = &disp[key_idx + "filename=".len()..];
    let trimmed = after.trim_start();
    let extracted = if let Some(rest) = trimmed.strip_prefix('"') {
        rest.find('"').map(|end| rest[..end].to_string())
    } else {
        trimmed
            .split(|c: char| c == ';' || c == ' ' || c == '\t')
            .next()
            .map(|s| s.to_string())
    };
    extracted.filter(|s| !s.is_empty())
}

fn collect_body(parsed: &ParsedMail, max_bytes: usize) -> Result<(String, bool)> {
    // Walk the MIME tree, preferring text/plain over text/html. Falls
    // back to text/html stripped of tags if no plain part exists.
    let mut plain_text = String::new();
    let mut html_text = String::new();
    let mut walk: Vec<&ParsedMail> = vec![parsed];
    while let Some(part) = walk.pop() {
        for sub in &part.subparts {
            walk.push(sub);
        }
        if is_attachment(part) {
            continue;
        }
        let ct = part.ctype.mimetype.to_ascii_lowercase();
        if ct == "text/plain" {
            if let Ok(body) = part.get_body() {
                plain_text.push_str(&body);
                plain_text.push('\n');
            }
        } else if ct == "text/html" && plain_text.is_empty() {
            if let Ok(body) = part.get_body() {
                html_text.push_str(&body);
                html_text.push('\n');
            }
        }
    }
    let mut body = if !plain_text.trim().is_empty() {
        plain_text
    } else if !html_text.trim().is_empty() {
        super::strip_html(&html_text)
    } else {
        // No body at all — fall through with empty content; the
        // ExtractedDoc still carries the headers via metadata.
        String::new()
    };
    let truncated = body.len() > max_bytes;
    if truncated {
        body.truncate(max_bytes);
        body.push_str("\n[…body truncated by extractor…]");
    }
    Ok((body, truncated))
}

/// Build a compact `From: / To: / Cc: / Date: / Subject:` header block
/// prefixed onto every email's chunk content. Empty headers are
/// omitted so the block stays short on terse messages. Trailing blank
/// line separates the preamble from the body.
fn build_header_preamble(
    from: Option<&str>,
    to: Option<&str>,
    cc: Option<&str>,
    date: Option<&str>,
    subject: Option<&str>,
) -> String {
    let mut out = String::new();
    if let Some(v) = from.filter(|s| !s.is_empty()) {
        out.push_str("From: ");
        out.push_str(v);
        out.push('\n');
    }
    if let Some(v) = to.filter(|s| !s.is_empty()) {
        out.push_str("To: ");
        out.push_str(v);
        out.push('\n');
    }
    if let Some(v) = cc.filter(|s| !s.is_empty()) {
        out.push_str("Cc: ");
        out.push_str(v);
        out.push('\n');
    }
    if let Some(v) = date.filter(|s| !s.is_empty()) {
        out.push_str("Date: ");
        out.push_str(v);
        out.push('\n');
    }
    if let Some(v) = subject.filter(|s| !s.is_empty()) {
        out.push_str("Subject: ");
        out.push_str(v);
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

fn synthesize_message_id(path: &Path) -> String {
    // Maildir messages typically don't carry a Message-ID. Synthesize
    // one from the path so the source_id is still stable across
    // re-ingest.
    format!(
        "synth-{}",
        short_hash(&path.to_string_lossy())
    )
}

fn synthetic_message_atom_id(message_id: &str) -> AtomId {
    AtomId::from_raw(format!("message-{}", short_hash(message_id)))
}

fn short_hash(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    let digest = h.finalize();
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn default_ext_for_mime(mime: &str) -> &'static str {
    match mime {
        "application/pdf" => "pdf",
        m if m.starts_with("application/vnd.openxmlformats-officedocument.spreadsheetml") => {
            "xlsx"
        }
        m if m.starts_with("application/vnd.openxmlformats-officedocument.wordprocessingml") => {
            "docx"
        }
        "text/csv" => "csv",
        "text/calendar" => "ics",
        _ => "bin",
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if dir.is_file() {
        out.push(dir.to_path_buf());
        return Ok(());
    }
    let entries = fs::read_dir(dir).map_err(|e| {
        Error::Extraction(format!("email: read_dir {}: {e}", dir.display()))
    })?;
    for entry in entries {
        let entry = entry
            .map_err(|e| Error::Extraction(format!("email: dir entry: {e}")))?;
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if name.starts_with('.') || name == "Thumbs.db" {
            continue;
        }
        if path.is_dir() {
            walk(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

fn append_asset_atom(sidecar: &Path, atom: &Asset) -> Result<()> {
    if let Some(parent) = sidecar.parent() {
        fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    let envelope = AtomEnvelope::Asset(atom.clone());
    let line = serde_json::to_string(&envelope).map_err(|e| {
        Error::Extraction(format!("email: serialise asset atom: {e}"))
    })?;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(sidecar)
        .map_err(Error::Io)?;
    let mut w = BufWriter::new(&mut f);
    w.write_all(line.as_bytes()).map_err(Error::Io)?;
    w.write_all(b"\n").map_err(Error::Io)?;
    w.flush().map_err(Error::Io)?;
    Ok(())
}

fn append_edge(sidecar: &Path, edge: &Edge) -> Result<()> {
    if let Some(parent) = sidecar.parent() {
        fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    let line = serde_json::to_string(edge).map_err(|e| {
        Error::Extraction(format!("email: serialise edge: {e}"))
    })?;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(sidecar)
        .map_err(Error::Io)?;
    let mut w = BufWriter::new(&mut f);
    w.write_all(line.as_bytes()).map_err(Error::Io)?;
    w.write_all(b"\n").map_err(Error::Io)?;
    w.flush().map_err(Error::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_store::FilesystemAssetStore;
    use crate::extractors::described_asset::AssetSubExtractorRegistry;

    const SIMPLE_EMAIL: &str = "From: alice@example.com\r\n\
To: bob@example.com\r\n\
Subject: hello\r\n\
Date: Mon, 27 May 2026 10:00:00 -0500\r\n\
Message-ID: <abc123@example.com>\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hi Bob,\r\n\
This is a plain-text email.\r\n";

    const THREADED_REPLY: &str = "From: bob@example.com\r\n\
To: alice@example.com\r\n\
Subject: Re: hello\r\n\
Date: Mon, 27 May 2026 11:00:00 -0500\r\n\
Message-ID: <reply1@example.com>\r\n\
In-Reply-To: <abc123@example.com>\r\n\
References: <abc123@example.com>\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Quoting Alice…";

    const EMAIL_WITH_TEXT_ATTACHMENT: &str = "From: alice@example.com\r\n\
To: bob@example.com\r\n\
Subject: see attached\r\n\
Date: Tue, 28 May 2026 09:00:00 -0500\r\n\
Message-ID: <attach1@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=BOUNDARY\r\n\
\r\n\
--BOUNDARY\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
See the attached notes.\r\n\
--BOUNDARY\r\n\
Content-Type: text/plain; name=\"notes.txt\"\r\n\
Content-Disposition: attachment; filename=\"notes.txt\"\r\n\
\r\n\
Notes body here.\r\n\
--BOUNDARY--\r\n";

    fn write_msg(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn parses_plain_email_into_metadata_and_body() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        write_msg(&inbox, "1.eml", SIMPLE_EMAIL);

        let extractor = EmailExtractor::new(EmailExtractorConfig::default());
        let docs: Vec<_> = extractor
            .extract(&inbox)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(docs.len(), 1);
        let d = &docs[0];
        assert!(d.content.contains("This is a plain-text email."));
        // Header preamble must reach the domain prompt — StoredChunk
        // exposes only `(id, content, title)`, so From:/To:/Subject:
        // need to be in the content body or the BusinessEmailDomain
        // can't capture sender/recipient identities. Keep this strict.
        assert!(
            d.content.starts_with("From: alice@example.com"),
            "expected header preamble at start of content; got: {:?}",
            d.content.chars().take(80).collect::<String>()
        );
        assert!(d.content.contains("To: bob@example.com"));
        assert!(d.content.contains("Subject: hello"));
        let meta = d.metadata.as_ref().unwrap();
        assert_eq!(meta["doc_type"], "email");
        assert_eq!(meta["from"], "alice@example.com");
        assert_eq!(meta["message_id"], "<abc123@example.com>");
        assert_eq!(meta["subject"], "hello");
        assert_eq!(meta["thread_id"], "abc123@example.com");
    }

    #[test]
    fn thread_id_collapses_reply_chain() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        write_msg(&inbox, "1.eml", SIMPLE_EMAIL);
        write_msg(&inbox, "2.eml", THREADED_REPLY);

        let extractor = EmailExtractor::new(EmailExtractorConfig::default());
        let docs: Vec<_> = extractor
            .extract(&inbox)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(docs.len(), 2);
        let thread_ids: Vec<_> = docs
            .iter()
            .map(|d| d.metadata.as_ref().unwrap()["thread_id"].as_str().unwrap().to_string())
            .collect();
        // Both messages collapse to the original Message-ID as the
        // thread root.
        assert_eq!(thread_ids[0], "abc123@example.com");
        assert_eq!(thread_ids[1], "abc123@example.com");
    }

    #[test]
    fn attachments_dispatch_through_asset_store() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        write_msg(&inbox, "1.eml", EMAIL_WITH_TEXT_ATTACHMENT);
        let assets_root = dir.path().join("assets");
        let store: AssetStoreHandle =
            Arc::new(FilesystemAssetStore::new(&assets_root).unwrap());
        let dispatch = EmailAssetDispatch {
            store: store.clone(),
            registry: AssetSubExtractorRegistry::defaults(),
            asset_atoms_sidecar: dir.path().join("atlas/asset_atoms.jsonl"),
            asset_edges_sidecar: dir.path().join("atlas/asset_edges.jsonl"),
        };
        let extractor = EmailExtractor::new(EmailExtractorConfig::default())
            .with_asset_dispatch(dispatch.clone());
        let docs: Vec<_> = extractor
            .extract(&inbox)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(docs.len(), 1);
        let meta = docs[0].metadata.as_ref().unwrap();
        let attachments = meta["attachments"].as_array().expect("attachments array");
        assert_eq!(attachments.len(), 1);
        let att = &attachments[0];
        assert_eq!(att["original_filename"], "notes.txt");
        assert_eq!(att["dispatched"], true);
        assert!(att["sha256"].is_string());

        // Asset store has the attachment bytes.
        let entries = store.entries().unwrap();
        assert_eq!(entries.len(), 1);
        // Asset atom landed in the sidecar.
        let sidecar = std::fs::read_to_string(&dispatch.asset_atoms_sidecar).unwrap();
        assert_eq!(sidecar.lines().count(), 1);
        // Attaches edge landed in the edges sidecar.
        let edges = std::fs::read_to_string(&dispatch.asset_edges_sidecar).unwrap();
        let edge: Edge = serde_json::from_str(edges.lines().next().unwrap()).unwrap();
        assert!(matches!(edge.edge_type, EdgeType::Attaches));
        assert!(edge.target.as_str().starts_with("asset-"));
    }

    #[test]
    fn maildir_layout_walked_recursively() {
        let dir = tempfile::tempdir().unwrap();
        // Maildir-like layout: nested folders with extension-less files.
        std::fs::create_dir_all(dir.path().join("user/inbox/cur")).unwrap();
        std::fs::create_dir_all(dir.path().join("user/sent/cur")).unwrap();
        std::fs::write(dir.path().join("user/inbox/cur/1"), SIMPLE_EMAIL).unwrap();
        std::fs::write(dir.path().join("user/sent/cur/2"), THREADED_REPLY).unwrap();
        let extractor = EmailExtractor::new(EmailExtractorConfig::default());
        let docs: Vec<_> = extractor
            .extract(dir.path())
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(docs.len(), 2);
    }

    #[test]
    fn html_only_email_falls_back_to_stripped_text() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let html_email = "From: a@e.com\r\n\
To: b@e.com\r\n\
Subject: html only\r\n\
Date: Wed, 29 May 2026 09:00:00 -0500\r\n\
Message-ID: <htm1@example.com>\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>Hello <b>world</b>.</p>\r\n";
        write_msg(&inbox, "1.eml", html_email);
        let extractor = EmailExtractor::new(EmailExtractorConfig::default());
        let docs: Vec<_> = extractor
            .extract(&inbox)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert!(docs[0].content.contains("Hello world."));
    }

    #[test]
    fn missing_message_id_synthesizes_one() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let no_msgid = "From: a@e.com\r\n\
To: b@e.com\r\n\
Subject: anon\r\n\
\r\n\
body";
        write_msg(&inbox, "1.eml", no_msgid);
        let extractor = EmailExtractor::new(EmailExtractorConfig::default());
        let docs: Vec<_> = extractor
            .extract(&inbox)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let mid = docs[0].metadata.as_ref().unwrap()["message_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(mid.starts_with("synth-"));
    }

    #[test]
    fn body_truncated_when_over_cap() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let huge = format!(
            "From: a@e.com\r\nTo: b@e.com\r\nSubject: huge\r\nMessage-ID: <hg@e.com>\r\nContent-Type: text/plain\r\n\r\n{}",
            "x".repeat(2048)
        );
        write_msg(&inbox, "1.eml", &huge);
        let extractor = EmailExtractor::new(EmailExtractorConfig {
            max_body_bytes: 256,
            max_attachment_bytes: 0,
        });
        let docs: Vec<_> = extractor
            .extract(&inbox)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let meta = docs[0].metadata.as_ref().unwrap();
        assert_eq!(meta["body_was_truncated"], true);
        assert!(docs[0].content.contains("body truncated"));
    }
}

