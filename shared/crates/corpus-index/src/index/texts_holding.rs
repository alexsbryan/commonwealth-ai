// SPDX-License-Identifier: AGPL-3.0-or-later
//! Every stored text a predicate keeps: the align route's verbatim recall
//! (OICP v0.6 §2.18), which reads the corpus's texts rather than trusting a
//! lexical top-k to have reached the one a quotation stands in.

use std::collections::BTreeSet;

use kernel_types::Sha256Hash;

use super::{CorpusIndex, TextLookup};
use crate::corpus::Corpus;
use crate::error::Result;

impl CorpusIndex {
    /// The names of the stored texts whose words `keep` accepts, in name
    /// order. Each text is read from the store as it lies; the predicate is a
    /// filter, so the bytes are not rehashed here (a text a caller then reads
    /// through [`CorpusIndex::text`] is). A text recorded stored whose file
    /// is missing or not UTF-8 is skipped and said at warn. One scan of the
    /// record table, then one file read per distinct stored text.
    pub async fn texts_holding(
        &self,
        keep: impl Fn(&str) -> bool,
    ) -> Result<TextLookup<Vec<Sha256Hash>>> {
        let records = match self.documents().await? {
            Ok((_, records)) => records,
            Err(absence) => return Ok(Err(absence)),
        };
        let names: BTreeSet<Sha256Hash> = records
            .iter()
            .filter(|r| r.text_stored)
            .map(|r| r.text_sha256)
            .collect();
        let dir = Corpus::texts_in(self.path());
        let (mut kept, mut bytes, mut unread) = (Vec::new(), 0usize, 0usize);
        for name in &names {
            let path = dir.join(name.to_hex());
            let text = match std::fs::read(&path).map(String::from_utf8) {
                Ok(Ok(text)) => text,
                Ok(Err(e)) => {
                    tracing::warn!(corpus = %self.corpus_id(), %name, error = %e, "texts_holding: a stored text is not UTF-8; skipped");
                    unread += 1;
                    continue;
                }
                Err(e) => {
                    tracing::warn!(corpus = %self.corpus_id(), %name, path = %path.display(), error = %e, "texts_holding: a stored text is unreadable; skipped");
                    unread += 1;
                    continue;
                }
            };
            bytes += text.len();
            if keep(&text) {
                kept.push(*name);
            }
        }
        tracing::debug!(
            corpus = %self.corpus_id(),
            texts = names.len(),
            bytes,
            unread,
            kept = kept.len(),
            "texts_holding: stored texts read"
        );
        Ok(Ok(kept))
    }
}
