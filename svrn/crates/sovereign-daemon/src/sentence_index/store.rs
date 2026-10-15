// SPDX-License-Identifier: AGPL-3.0-or-later
//! One corpus's sentence index: every stored text's sentences by address (the
//! text's name and the sentence's code-point range, with its byte range for
//! slicing), the key each is cached under (the sha256 of the sentence's own
//! bytes), and the unit vectors embedded for them, stamped with the
//! embedder's identity. Kept in the corpus's canonical directory
//! (`Corpus::sentences_path`) so a restart reloads it rather than embedding
//! again.
//!
//! The identity is the embedder's answer to a fixed sentinel, as
//! `sovereign_core::router_embed_cache` keys its cache: providers name no
//! stable model id across wrappers, so two embedders are the same when their
//! sentinel vectors agree. A vector is only ever served under the identity it
//! was stamped with; a different embedder's probe makes every row pending
//! again.

use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use kernel_types::Sha256Hash;

/// The sentinel whose vector is an embedder's identity here. Never searched.
pub const PROBE_TEXT: &str = "sovereign sentence index — embedder identity probe v1";

/// Two probes from one embedder agree to about 1.0 (GPU reduction order aside);
/// another model or quantisation lands far lower. The router cache's bar.
pub const PROBE_MIN_COSINE: f32 = 0.98;

/// The splitter whose ranges a file holds; a file cut by another is re-cut.
const SPLITTER: &str = "sentence_ranges/1";
const MAGIC: &[u8; 8] = b"SVSENT01";

/// One text's rows, contiguous in [`CorpusSentences::rows`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRows {
    pub text: Sha256Hash,
    pub rows: std::ops::Range<usize>,
}

/// One sentence's address and cache key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentenceRow {
    /// Code points of the text, the address a client reads back.
    pub start: u32,
    pub end: u32,
    /// The same range in bytes, so a hit is sliced without walking the text.
    pub byte_start: u32,
    pub byte_end: u32,
    /// sha256 of the sentence's UTF-8 bytes: the cache key, wherever it stands.
    pub key: [u8; 32],
}

/// A corpus's sentences and their vectors.
#[derive(Debug, Default)]
pub struct CorpusSentences {
    /// The record table's version the addresses were cut from.
    pub version: Option<u64>,
    /// The identity the vectors were stamped with; empty before the first.
    pub probe: Vec<f32>,
    pub dims: usize,
    pub texts: Vec<TextRows>,
    pub rows: Vec<SentenceRow>,
    /// `rows.len() * dims` unit vectors; zeros where a row is pending.
    pub vectors: Vec<f32>,
    pub embedded: Vec<bool>,
    /// Rows embedded.
    pub done: usize,
}

pub fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
    v
}

/// Whether `probe` is the embedder `stamped` was made by.
pub fn same_embedder(stamped: &[f32], probe: &[f32]) -> bool {
    stamped.len() == probe.len()
        && !stamped.is_empty()
        && stamped.iter().zip(probe).map(|(a, b)| a * b).sum::<f32>() >= PROBE_MIN_COSINE
}

/// `text`'s sentences, by the rule verify splits claims by
/// (`sovereign_core::runtime::sentence_ranges`), each with its byte range and key.
pub fn split(text: &str) -> Vec<SentenceRow> {
    let mut byte_at: Vec<u32> = text.char_indices().map(|(b, _)| b as u32).collect();
    byte_at.push(text.len() as u32);
    sovereign_core::runtime::sentence_ranges(text)
        .into_iter()
        .map(|r| {
            let (bs, be) = (byte_at[r.start], byte_at[r.end]);
            SentenceRow {
                start: r.start as u32,
                end: r.end as u32,
                byte_start: bs,
                byte_end: be,
                key: *Sha256Hash::of(&text.as_bytes()[bs as usize..be as usize]).as_bytes(),
            }
        })
        .collect()
}

impl CorpusSentences {
    pub fn pending(&self) -> usize {
        self.rows.len() - self.done
    }

    /// Bytes this index holds in memory: the vectors, the rows and flags.
    pub fn resident_bytes(&self) -> usize {
        self.vectors.len() * 4
            + self.rows.len() * (std::mem::size_of::<SentenceRow>() + 1)
            + self.texts.len() * std::mem::size_of::<TextRows>()
    }

    pub fn vector(&self, row: usize) -> &[f32] {
        &self.vectors[row * self.dims..(row + 1) * self.dims]
    }

    /// The rows of `scope`'s texts (every text when `None`), in text order.
    pub fn scoped(&self, scope: Option<&[Sha256Hash]>) -> Vec<std::ops::Range<usize>> {
        self.texts
            .iter()
            .filter(|t| scope.is_none_or(|s| s.contains(&t.text)))
            .map(|t| t.rows.clone())
            .collect()
    }

    pub fn text_rows(&self, text: &Sha256Hash) -> Option<std::ops::Range<usize>> {
        self.texts
            .binary_search_by(|t| t.text.cmp(text))
            .ok()
            .map(|i| self.texts[i].rows.clone())
    }

    /// The text `row` was cut from.
    pub fn text_of(&self, row: usize) -> Option<Sha256Hash> {
        let i = self.texts.partition_point(|t| t.rows.end <= row);
        self.texts
            .get(i)
            .filter(|t| t.rows.contains(&row))
            .map(|t| t.text)
    }

    /// The rows of `text` overlapping its code points `start..end`: the
    /// sentences a passage there touches.
    pub fn overlapping(&self, text: &Sha256Hash, start: u32, end: u32) -> Vec<usize> {
        self.text_rows(text)
            .map(|rows| {
                rows.filter(|&r| self.rows[r].start < end && self.rows[r].end > start)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The embedded row holding each key, for a sentence seen again.
    pub fn by_key(&self) -> HashMap<[u8; 32], usize> {
        (0..self.rows.len())
            .filter(|&r| self.embedded[r])
            .map(|r| (self.rows[r].key, r))
            .collect()
    }

    /// Adopt `probe` as the identity: the first vector's, or an embedder that
    /// is not the one the vectors were made by, whose rows all go pending.
    pub fn stamp(&mut self, probe: Vec<f32>) {
        if same_embedder(&self.probe, &probe) {
            return;
        }
        if !self.probe.is_empty() {
            tracing::warn!(
                target: "sentence_index",
                embedded = self.done,
                "sentence index: the embedder is not the one these vectors were made by; every row is pending again"
            );
        }
        self.dims = probe.len();
        self.probe = probe;
        self.vectors = vec![0.0; self.rows.len() * self.dims];
        self.embedded = vec![false; self.rows.len()];
        self.done = 0;
    }

    /// Set `row`'s unit vector. The identity must already be stamped.
    pub fn put(&mut self, row: usize, v: &[f32]) {
        if v.len() != self.dims {
            tracing::error!(target: "sentence_index", row, got = v.len(), dims = self.dims, "sentence index: a vector of another width; the row stays pending");
            return;
        }
        self.vectors[row * self.dims..(row + 1) * self.dims].copy_from_slice(v);
        if !self.embedded[row] {
            self.embedded[row] = true;
            self.done += 1;
        }
    }

    /// The index over `texts` (name and its rows, cut fresh or kept), keeping
    /// every vector this one holds for a key still present.
    pub fn rebuilt(
        &self,
        version: Option<u64>,
        mut texts: Vec<(Sha256Hash, Vec<SentenceRow>)>,
    ) -> Self {
        texts.sort_by(|a, b| a.0.cmp(&b.0));
        let keep = self.by_key();
        let mut out = CorpusSentences {
            version,
            probe: self.probe.clone(),
            dims: self.dims,
            ..Default::default()
        };
        for (text, rows) in texts {
            let start = out.rows.len();
            out.rows.extend(rows);
            out.texts.push(TextRows {
                text,
                rows: start..out.rows.len(),
            });
        }
        out.vectors = vec![0.0; out.rows.len() * out.dims];
        out.embedded = vec![false; out.rows.len()];
        for r in 0..out.rows.len() {
            if let Some(&old) = keep.get(&out.rows[r].key) {
                let v = self.vector(old).to_vec();
                out.put(r, &v);
            }
        }
        out
    }

    /// The rows of an existing text, as this index holds them.
    pub fn rows_of(&self, text: &Sha256Hash) -> Option<Vec<SentenceRow>> {
        self.text_rows(text).map(|r| self.rows[r].to_vec())
    }

    /// Write to `path` by temp and rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("tmp");
        {
            let mut w = BufWriter::with_capacity(1 << 20, std::fs::File::create(&tmp)?);
            let header = serde_json::json!({
                "splitter": SPLITTER,
                "version": self.version,
                "dims": self.dims,
                "probe": self.probe,
                "texts": self.texts.len(),
                "rows": self.rows.len(),
            })
            .to_string();
            w.write_all(MAGIC)?;
            w.write_all(&(header.len() as u32).to_le_bytes())?;
            w.write_all(header.as_bytes())?;
            for t in &self.texts {
                w.write_all(t.text.as_bytes())?;
                w.write_all(&(t.rows.start as u32).to_le_bytes())?;
                w.write_all(&(t.rows.end as u32).to_le_bytes())?;
            }
            for r in &self.rows {
                for n in [r.start, r.end, r.byte_start, r.byte_end] {
                    w.write_all(&n.to_le_bytes())?;
                }
                w.write_all(&r.key)?;
            }
            let flags: Vec<u8> = self.embedded.iter().map(|&e| u8::from(e)).collect();
            w.write_all(&flags)?;
            // The vectors as one byte run: a cast, not a loop per float.
            let bytes: Vec<u8> = self.vectors.iter().flat_map(|f| f.to_le_bytes()).collect();
            w.write_all(&bytes)?;
            w.flush()?;
        }
        std::fs::rename(&tmp, path)
    }

    /// Read `path`, or why it does not read. A file cut by another splitter
    /// is refused, so its texts are cut again rather than served off by one.
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut r = BufReader::with_capacity(1 << 20, file);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic).map_err(|e| e.to_string())?;
        if &magic != MAGIC {
            return Err("not a sentence index".into());
        }
        let header_len = read_u32(&mut r)? as usize;
        let mut header = vec![0u8; header_len];
        r.read_exact(&mut header).map_err(|e| e.to_string())?;
        let h: serde_json::Value = serde_json::from_slice(&header).map_err(|e| e.to_string())?;
        if h["splitter"] != SPLITTER {
            return Err(format!("cut by {}, not {SPLITTER}", h["splitter"]));
        }
        let field = |k: &str| h[k].as_u64().ok_or_else(|| format!("header lacks `{k}`"));
        let (dims, n_texts, n_rows) = (field("dims")? as usize, field("texts")?, field("rows")?);
        let probe: Vec<f32> =
            serde_json::from_value(h["probe"].clone()).map_err(|e| e.to_string())?;
        let mut out = CorpusSentences {
            version: h["version"].as_u64(),
            probe,
            dims,
            ..Default::default()
        };
        for _ in 0..n_texts {
            let mut name = [0u8; 32];
            r.read_exact(&mut name).map_err(|e| e.to_string())?;
            let (start, end) = (read_u32(&mut r)? as usize, read_u32(&mut r)? as usize);
            out.texts.push(TextRows {
                text: Sha256Hash::from_bytes(name),
                rows: start..end,
            });
        }
        for _ in 0..n_rows {
            let (start, end) = (read_u32(&mut r)?, read_u32(&mut r)?);
            let (byte_start, byte_end) = (read_u32(&mut r)?, read_u32(&mut r)?);
            let mut key = [0u8; 32];
            r.read_exact(&mut key).map_err(|e| e.to_string())?;
            out.rows.push(SentenceRow {
                start,
                end,
                byte_start,
                byte_end,
                key,
            });
        }
        let mut flags = vec![0u8; out.rows.len()];
        r.read_exact(&mut flags).map_err(|e| e.to_string())?;
        out.embedded = flags.iter().map(|&f| f == 1).collect();
        out.done = out.embedded.iter().filter(|&&e| e).count();
        let mut bytes = vec![0u8; out.rows.len() * dims * 4];
        r.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        out.vectors = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        Ok(out)
    }
}

fn read_u32(r: &mut impl Read) -> Result<u32, String> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).map_err(|e| e.to_string())?;
    Ok(u32::from_le_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each sentence's code-point and byte ranges cut the same bytes, keyed
    /// by those bytes, across multi-byte characters.
    #[test]
    fn a_row_s_ranges_cut_its_sentence_and_its_key_is_its_bytes() {
        let text = "Café opens at noon. The “river” rose! Is it 3 p.m. yet?";
        let rows = split(text);
        assert_eq!(rows.len(), 3);
        for r in &rows {
            let by_bytes = &text[r.byte_start as usize..r.byte_end as usize];
            let by_cps: String = text
                .chars()
                .skip(r.start as usize)
                .take((r.end - r.start) as usize)
                .collect();
            assert_eq!(by_bytes, by_cps);
            assert_eq!(&r.key, Sha256Hash::of_str(by_bytes).as_bytes());
        }
    }

    /// A save reads back whole, and a rebuild keeps a vector for a key still
    /// present wherever its sentence now stands.
    #[test]
    fn a_saved_index_reads_back_and_a_rebuild_keeps_vectors_by_key() {
        let a = Sha256Hash::of_str("a");
        let mut s = CorpusSentences::default().rebuilt(Some(3), vec![(a, split("One. Two."))]);
        s.stamp(unit(vec![1.0, 0.0]));
        s.put(0, &[0.0, 1.0]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.bin");
        s.save(&path).unwrap();
        let back = CorpusSentences::load(&path).unwrap();
        assert_eq!((back.version, back.done, back.dims), (Some(3), 1, 2));
        assert_eq!(back.rows, s.rows);
        assert_eq!(back.vector(0), &[0.0, 1.0]);

        let b = Sha256Hash::of_str("b");
        let moved = back.rebuilt(Some(4), vec![(b, split("Zero. One."))]);
        assert_eq!(moved.done, 1, "`One.` keeps its vector in another text");
        assert!(moved.embedded[1] && !moved.embedded[0]);
    }
}
