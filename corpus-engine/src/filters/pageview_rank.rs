//! Filter that accepts articles ranked at-or-better-than a threshold by
//! a pageview-rank table.
//!
//! The rank table is a two-column CSV (`title,rank`) sorted ascending by
//! rank (rank 1 = most viewed). Construction parses the whole table
//! once into a `HashMap<normalized_title, rank>`, then `accept` is a
//! single hash lookup.
//!
//! Source format: at 100K entries the uncompressed CSV is ~2.5 MB,
//! gzipped ~700 KB. The bundled file is gzipped (`.csv.gz`) and lives
//! under `corpus-engine/assets/`. See
//! `sovereign-recipes/wikipedia/scripts/build_pageview_ranks.py` for
//! the generation pipeline.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};

use crate::error::{Error, Result};
use crate::extractors::ExtractedDoc;
use crate::filters::{doc_title_for_filter, DocumentFilter};

pub struct PageviewRankFilter {
    ranks: HashMap<String, u32>,
    max_rank: u32,
    description: String,
}

impl PageviewRankFilter {
    /// Build from gzip-compressed CSV bytes (the form bundled in the
    /// crate's `assets/` directory and embedded via `include_bytes!`).
    pub fn from_gz_csv_bytes(gz_bytes: &[u8], max_rank: u32) -> Result<Self> {
        let decoder = flate2::read::GzDecoder::new(gz_bytes);
        Self::from_csv_reader(decoder, max_rank, "bundled")
    }

    /// Build from raw CSV bytes (uncompressed).
    pub fn from_csv_bytes(csv_bytes: &[u8], max_rank: u32) -> Result<Self> {
        Self::from_csv_reader(csv_bytes, max_rank, "bytes")
    }

    /// Build from a `.csv` or `.csv.gz` file on disk. Detection is by
    /// extension — the recipe override directory is the typical caller.
    pub fn from_path(path: &std::path::Path, max_rank: u32) -> Result<Self> {
        let file = std::fs::File::open(path).map_err(Error::Io)?;
        let label = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("rank_file")
            .to_string();
        if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("gz"))
            .unwrap_or(false)
        {
            let decoder = flate2::read::GzDecoder::new(file);
            Self::from_csv_reader(decoder, max_rank, &label)
        } else {
            Self::from_csv_reader(file, max_rank, &label)
        }
    }

    fn from_csv_reader<R: Read>(reader: R, max_rank: u32, source_label: &str) -> Result<Self> {
        let buf = BufReader::new(reader);
        let mut ranks = HashMap::with_capacity(max_rank as usize);
        let mut line_no: usize = 0;
        // Header is the first non-comment, non-blank line — index can
        // be anywhere because the file may start with `#` metadata
        // (`# date=2023-11-01 ...`) before the `title,rank` row. Track
        // whether we've seen *any* data line yet so the header check
        // can tolerate a non-numeric rank exactly once.
        let mut data_seen = false;
        for line in buf.lines() {
            let line = line.map_err(Error::Io)?;
            line_no += 1;
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // CSV-light: split on the LAST comma. Wikipedia titles never
            // contain commas in their canonical form (the rank file
            // generator percent-encodes any oddities), so the rank is
            // always the trailing token. Guarding against a comma in
            // the title would require a real CSV parser; keep it simple.
            let Some((title_raw, rank_raw)) = line.rsplit_once(',') else {
                if !data_seen {
                    data_seen = true;
                    continue;
                }
                return Err(Error::Recipe(format!(
                    "pageview rank file {source_label} line {line_no}: missing comma"
                )));
            };
            // Header detection — first data-shaped row with a non-numeric
            // rank column is treated as a header. Subsequent malformed
            // rows are real errors.
            let Ok(rank) = rank_raw.trim().parse::<u32>() else {
                if !data_seen {
                    data_seen = true;
                    continue;
                }
                return Err(Error::Recipe(format!(
                    "pageview rank file {source_label} line {line_no}: rank '{rank_raw}' not a u32"
                )));
            };
            data_seen = true;
            if rank == 0 || rank > max_rank {
                // Rank 0 is meaningless; ranks beyond the cutoff don't
                // need to occupy memory. Bundled files are pre-trimmed
                // but defend against larger inputs (e.g. a user-supplied
                // override path).
                continue;
            }
            let normalized = crate::filters::normalize_title(title_raw);
            // First-occurrence wins. The CSV is rank-sorted, so the
            // first time we see a duplicate-spelling collision the
            // earlier entry is the better-ranked one. Real-world
            // duplicates are vanishingly rare (Wikipedia titles are
            // unique per namespace).
            ranks.entry(normalized).or_insert(rank);
        }
        let count = ranks.len();
        Ok(Self {
            ranks,
            max_rank,
            description: format!("pageview rank ≤ {max_rank} ({count} titles)"),
        })
    }

    /// Number of titles that pass the threshold (i.e. live in the map).
    pub fn accepted_titles(&self) -> usize {
        self.ranks.len()
    }
}

impl DocumentFilter for PageviewRankFilter {
    fn accept(&self, doc: &ExtractedDoc) -> bool {
        let Some(title) = doc_title_for_filter(doc) else {
            return false;
        };
        match self.ranks.get(&title) {
            Some(&rank) => rank <= self.max_rank,
            None => false,
        }
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn expected_count(&self) -> Option<usize> {
        Some(self.ranks.len())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_doc(title: &str) -> ExtractedDoc {
        ExtractedDoc {
            title: Some(title.into()),
            content: String::new(),
            url: None,
            source_id: title.into(),
            metadata: None,
            source_file: None,
        }
    }

    #[test]
    fn parses_csv_and_filters_by_rank() {
        let csv = "title,rank\nAlbert Einstein,1\nPhotosynthesis,2\nMain Page,3\nObscure_Article,500\n";
        let f = PageviewRankFilter::from_csv_bytes(csv.as_bytes(), 3).unwrap();
        assert!(f.accept(&make_doc("Albert Einstein")));
        assert!(f.accept(&make_doc("Photosynthesis")));
        assert!(f.accept(&make_doc("Main Page")));
        assert!(!f.accept(&make_doc("Obscure Article"))); // rank 500 > max 3
        assert!(!f.accept(&make_doc("Not Listed")));
    }

    #[test]
    fn underscores_and_case_are_normalized() {
        let csv = "albert_einstein,1\n";
        let f = PageviewRankFilter::from_csv_bytes(csv.as_bytes(), 100).unwrap();
        assert!(f.accept(&make_doc("Albert Einstein")));
        assert!(f.accept(&make_doc("ALBERT_einstein")));
    }

    #[test]
    fn header_lines_are_tolerated() {
        let csv = "title,rank\nFoo,1\n";
        let f = PageviewRankFilter::from_csv_bytes(csv.as_bytes(), 100).unwrap();
        assert!(f.accept(&make_doc("Foo")));
    }

    #[test]
    fn comments_and_blank_lines_skipped() {
        let csv = "# comment\n\nFoo,1\n# another\nBar,2\n";
        let f = PageviewRankFilter::from_csv_bytes(csv.as_bytes(), 10).unwrap();
        assert!(f.accept(&make_doc("Foo")));
        assert!(f.accept(&make_doc("Bar")));
    }

    #[test]
    fn entries_above_max_rank_are_dropped_at_load() {
        let csv = "Foo,1\nBar,9999\n";
        let f = PageviewRankFilter::from_csv_bytes(csv.as_bytes(), 100).unwrap();
        assert_eq!(f.accepted_titles(), 1);
        assert!(f.accept(&make_doc("Foo")));
        assert!(!f.accept(&make_doc("Bar")));
    }

    #[test]
    fn gzip_round_trip() {
        let csv = "Foo,1\nBar,2\n";
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(csv.as_bytes()).unwrap();
        let compressed = gz.finish().unwrap();
        let f = PageviewRankFilter::from_gz_csv_bytes(&compressed, 100).unwrap();
        assert!(f.accept(&make_doc("Foo")));
        assert!(f.accept(&make_doc("Bar")));
    }

    #[test]
    fn malformed_rank_returns_error() {
        // First line is treated as a tolerated header. Malformed rank
        // on a subsequent line errors so the operator notices a corrupt
        // dump instead of silently dropping rows.
        let csv = "title,rank\nFoo,1\nBar,not_a_number\n";
        let res = PageviewRankFilter::from_csv_bytes(csv.as_bytes(), 100);
        assert!(res.is_err());
    }
}
