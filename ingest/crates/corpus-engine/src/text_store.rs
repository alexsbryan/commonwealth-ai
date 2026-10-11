// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine's half of the text store (ADDRESSED_TEXT §3, §4).
//!
//! corpus-index owns the store itself — the writer, the records, the reads.
//! This module owns what needs the recipe or the published vocabulary: the
//! extractor tag a record carries, the library digest (through
//! `oicp_types::evidence::texts_digest_preimage`, the one derivation), and the
//! rule a merge applies to decide whether its output still has every text.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use kernel_types::Sha256Hash;

use crate::error::Result;
use crate::index::{CorpusIndex, TextLookup};
use crate::recipe::ExtractorConfig;

/// A record's `extractor`: the recipe's `[extract]` type tag, with the custom
/// `kind` for a runtime-registered extractor, `@` this corpus-engine version.
/// The tag is read from the config's own serde tag, so a new extractor needs
/// no entry here.
pub fn extractor_tag(extract: &ExtractorConfig) -> String {
    let value = serde_json::to_value(extract).unwrap_or_default();
    let tag = value.get("type").and_then(|t| t.as_str());
    let kind = value.get("kind").and_then(|k| k.as_str());
    let name = match (tag, kind) {
        (Some("custom"), Some(kind)) => format!("custom:{kind}"),
        (Some(tag), _) => tag.to_string(),
        (None, _) => {
            tracing::warn!("text store: extractor config has no type tag; recorded as untagged");
            "untagged".to_string()
        }
    };
    format!("{name}@{}", env!("CARGO_PKG_VERSION"))
}

/// The corpus's library digest, `CorpusTexts.texts_digest`: sha256 of the
/// preimage `oicp_types::evidence::texts_digest_preimage` builds from its
/// records, as lowercase hex. Memoised per record-table version, since it is
/// one full scan.
pub async fn texts_digest(index: &CorpusIndex) -> Result<TextLookup<Sha256Hash>> {
    let (version, rows) = match index.documents().await? {
        Ok(read) => read,
        Err(absence) => return Ok(Err(absence)),
    };
    let dir = index.path();
    if let (Some(v), Ok(memo)) = (version, digest_memo().lock()) {
        if let Some((mv, d)) = memo.get(&dir) {
            if *mv == v {
                return Ok(Ok(*d));
            }
        }
    }
    let hex: Vec<(String, Option<String>, &str)> = rows
        .iter()
        .map(|r| {
            (
                r.text_sha256.to_hex(),
                r.source_sha256.map(|s| s.to_hex()),
                r.extractor.as_str(),
            )
        })
        .collect();
    let preimage = oicp_types::evidence::texts_digest_preimage(
        hex.iter().map(|(t, s, e)| (t.as_str(), s.as_deref(), *e)),
    );
    let digest = Sha256Hash::of_str(&preimage);
    tracing::debug!(corpus = %index.corpus_id(), ?version, records = rows.len(), %digest, "texts digest computed");
    if let (Some(v), Ok(mut memo)) = (version, digest_memo().lock()) {
        memo.insert(dir, (v, digest));
    }
    Ok(Ok(digest))
}

type DigestMemo = Mutex<HashMap<PathBuf, (u64, Sha256Hash)>>;

fn digest_memo() -> &'static DigestMemo {
    static MEMO: OnceLock<DigestMemo> = OnceLock::new();
    MEMO.get_or_init(Default::default)
}

/// Carry the inputs' texts into a merge's output. The output covers every
/// document only when it did before (`dst_was_empty`, or it already had a
/// store) AND every input did; otherwise it is marked as having no store and
/// answers `texts not stored`, never a partial one.
pub(crate) async fn carry_texts(
    dst: &CorpusIndex,
    srcs: &[PathBuf],
    dst_was_empty: bool,
) -> Result<()> {
    let into = dst.path();
    if !dst_was_empty && !dst.text_store() {
        tracing::info!(into = %into.display(), "text store: merge target predates it; not carried");
        return Ok(());
    }
    let mut without: Vec<&Path> = Vec::new();
    for src in srcs {
        if !CorpusIndex::open(src).await?.text_store() {
            without.push(src);
        }
    }
    if !without.is_empty() {
        dst.set_text_store(false)?;
        tracing::warn!(
            into = %into.display(),
            ?without,
            "text store: an input has none, so the merged corpus answers texts not stored"
        );
        return Ok(());
    }
    for src in srcs {
        dst.union_texts_from(src).await?;
    }
    dst.set_text_store(true)?;
    tracing::debug!(into = %into.display(), inputs = srcs.len(), "text store: carried through merge");
    Ok(())
}

#[cfg(test)]
#[path = "text_store_tests.rs"]
mod tests;
