// SPDX-License-Identifier: AGPL-3.0-or-later
//! The text store: each document's canonical text, named by the sha256 of its
//! bytes, with one record per document (ADDRESSED_TEXT §3).
//!
//! A text is the string one extracted document's chunks are cut from. It
//! lives at `<index>/texts/<sha256>` ([`Corpus::texts_in`]), written once by
//! temp-and-rename, and its record is a row of the per-corpus `documents`
//! Lance table beside `chunks`. Chunks carry the name in `text_sha256`.
//!
//! [`TextWriter::store_document`] is the one writer, and every ingest path
//! (the main loop, the watched-folder delta, reindex) routes through it. The
//! reads — [`CorpusIndex::text`], [`CorpusIndex::documents_for`],
//! [`CorpusIndex::documents`] — answer a named [`TextAbsence`] rather than
//! an empty result when the corpus cannot answer.
//!
//! The record table is a SET: a row repeated by a resumed ingest names the
//! same document, and every read collapses identical rows.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use arrow_array::{Array, BooleanArray, Int64Array, RecordBatch, StringArray};
use arrow_schema::SchemaRef;
use futures::TryStreamExt;
use kernel_types::Sha256Hash;
use lancedb::query::{ExecutableQuery, QueryBase, Select};

use super::{read_meta, write_meta, CorpusIndex};
use crate::corpus::Corpus;
use crate::error::{Error, Result};

/// The per-corpus record table, one row per stored text per document.
pub const DOCUMENTS_TABLE: &str = "documents";

/// The keys a front door that read a file itself states on a staged JSONL
/// line (svrn's local-corpus staging). The JSONL extractor reads them as
/// [`DocSource::Hashed`].
pub const STAGED_SOURCE_SHA256: &str = "source_sha256";
pub const STAGED_EXTRACTOR: &str = "extractor";

/// Where an extracted document's bytes came from — what its record's
/// `source_sha256` says. Required on every `ExtractedDoc`, so each extractor
/// decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocSource {
    /// The extractor read this one file; the writer hashes it, once per path.
    File(PathBuf),
    /// A front door read the bytes itself and states their hash and its own
    /// extractor (svrn's local-corpus staging of PDF, EPUB, DOCX).
    Hashed {
        sha256: Sha256Hash,
        extractor: String,
    },
    /// One record among many in a file (JSONL, CSV, Parquet, dumps): no source
    /// hash, said by name.
    Record,
}

/// One document's record: §3's fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentRecord {
    /// The text's name: sha256 of its UTF-8 bytes.
    pub text_sha256: Sha256Hash,
    /// The recipe's `[extract]` tag `@` the corpus-engine version, or the
    /// extractor a `Hashed` front door stated.
    pub extractor: String,
    /// Today's `source_doc_id`: an attribute, never the name.
    pub source_id: String,
    /// sha256 of the bytes the extractor read; `None` for a record.
    pub source_sha256: Option<Sha256Hash>,
    /// The text's position among its source's texts.
    pub ordinal: u32,
    /// The document's metadata JSON, verbatim.
    pub metadata: Option<String>,
    /// `false` when the recipe declared `store_texts = false`.
    pub text_stored: bool,
}

impl DocumentRecord {
    /// What makes two rows the same document. Metadata is not part of it.
    fn key(&self) -> (Sha256Hash, &str, u32, Option<Sha256Hash>, &str) {
        (
            self.text_sha256,
            self.source_id.as_str(),
            self.ordinal,
            self.source_sha256,
            self.extractor.as_str(),
        )
    }
}

/// Why a corpus cannot answer with a text. Wave 2's routes map each variant to
/// the published reason (`oicp_types::evidence::reasons`); this crate holds
/// no second spelling of those strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAbsence {
    /// No document of this corpus has that text.
    NotHeld,
    /// The corpus has no text store: written before schema v4, or merged from
    /// a shard that had none.
    TextsNotStored,
    /// The document is held, but its recipe declared `store_texts = false`.
    TextNotStored,
}

/// A read that either answers or names why it cannot.
pub type TextLookup<T> = std::result::Result<T, TextAbsence>;

/// A stored text and every document whose text it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredText {
    pub text: String,
    /// Sorted by `(source_id, ordinal)`. Identical texts from different
    /// sources share one file and have one record each.
    pub documents: Vec<DocumentRecord>,
}

/// What the writer is told about one document.
pub struct DocumentInput<'a> {
    /// The canonical text: `normalize_content(doc.content)`.
    pub text: &'a str,
    pub source_id: &'a str,
    pub ordinal: u32,
    pub source: &'a DocSource,
    /// The record's metadata as JSON text, stored exactly as given: the
    /// recipe's declared text verbatim, or the extractor's value serialized.
    /// corpus-engine decides which (`recipe_documents::DeclaredMetadata`).
    pub metadata: Option<std::borrow::Cow<'a, str>>,
}

/// The one writer of texts and their records for one index directory.
///
/// Texts are written at once; records are buffered and written by
/// [`flush`](Self::flush), which the main ingest calls at each chunk flush, or
/// by [`replace_source`](Self::replace_source) on the delta and reindex paths.
pub struct TextWriter {
    texts_dir: PathBuf,
    extractor: String,
    store_texts: bool,
    active: bool,
    pending: Vec<DocumentRecord>,
    last_file: Option<(PathBuf, Sha256Hash)>,
    texts_written: u64,
    texts_present: u64,
}

impl TextWriter {
    /// A writer for `index`. The store is begun only on an EMPTY index: an
    /// index that already holds chunks without one predates the store, and
    /// starting it there would make a corpus that holds some of its texts.
    /// Such a writer stores nothing, and the corpus answers `texts not stored`.
    pub async fn open(
        index: &CorpusIndex,
        extractor: impl Into<String>,
        store_texts: bool,
    ) -> Result<Self> {
        let dir = index.path();
        let mut active = index.text_store();
        if !active {
            let chunks = index.chunk_count().await?;
            if chunks == 0 {
                index.set_text_store(true)?;
                active = true;
                tracing::debug!(index = %dir.display(), "text store: begun on an empty index");
            } else {
                tracing::info!(
                    index = %dir.display(),
                    chunks,
                    "text store: this index predates it, so no texts are stored until it is \
                     reingested"
                );
            }
        }
        if active && !store_texts {
            tracing::info!(
                index = %dir.display(),
                "text store: recipe declares store_texts = false; records kept, texts not stored"
            );
        }
        Ok(Self {
            texts_dir: Corpus::texts_in(&dir),
            extractor: extractor.into(),
            store_texts,
            active,
            pending: Vec::new(),
            last_file: None,
            texts_written: 0,
            texts_present: 0,
        })
    }

    /// Whether this writer stores anything at all.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Store one document's text and buffer its record. Returns the name the
    /// document's chunks are stamped with, or `None` when the index has no
    /// text store. With `store_texts = false` the name is still returned and
    /// the record kept, so the document answers `text not stored`.
    pub fn store_document(&mut self, doc: DocumentInput<'_>) -> Result<Option<Sha256Hash>> {
        if !self.active {
            return Ok(None);
        }
        let text_sha256 = Sha256Hash::of_str(doc.text);
        let (source_sha256, extractor) = match doc.source {
            DocSource::File(path) => (Some(self.hash_file(path)?), self.extractor.clone()),
            DocSource::Hashed { sha256, extractor } => (Some(*sha256), extractor.clone()),
            DocSource::Record => (None, self.extractor.clone()),
        };
        if self.store_texts {
            self.write_text(&text_sha256, doc.text)?;
        }
        self.pending.push(DocumentRecord {
            text_sha256,
            extractor,
            source_id: doc.source_id.to_string(),
            source_sha256,
            ordinal: doc.ordinal,
            metadata: doc.metadata.map(std::borrow::Cow::into_owned),
            text_stored: self.store_texts,
        });
        Ok(Some(text_sha256))
    }

    /// Write the buffered records. Called before the chunks that name them are
    /// inserted, so a chunk never names a text whose record was lost.
    pub async fn flush(&mut self, index: &CorpusIndex) -> Result<usize> {
        if self.pending.is_empty() {
            return Ok(0);
        }
        let rows = std::mem::take(&mut self.pending);
        let table = documents_table(index, true)
            .await?
            .ok_or_else(|| Error::Database("documents table not created".into()))?;
        table
            .add(vec![rows_to_batch(&rows)?])
            .execute()
            .await
            .map_err(|e| Error::Database(format!("documents insert: {e}")))?;
        tracing::debug!(
            index = %index.path().display(),
            rows = rows.len(),
            texts_written = self.texts_written,
            texts_already_present = self.texts_present,
            "text store: records flushed"
        );
        Ok(rows.len())
    }

    /// Replace every record of `source_id` with the buffered ones — the delta
    /// and reindex paths, where a source is re-read whole. A text no remaining
    /// record names is removed from the store (the closure half: an edited file
    /// does not leave its old text behind).
    pub async fn replace_source(&mut self, index: &CorpusIndex, source_id: &str) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        let filter = format!("source_id = '{}'", source_id.replace('\'', "''"));
        let old = match documents_table(index, false).await? {
            Some(table) => {
                let old = read_rows(&table, Some(&filter)).await?;
                table
                    .delete(&filter)
                    .await
                    .map_err(|e| Error::Database(format!("documents delete: {e}")))?;
                old
            }
            None => Vec::new(),
        };
        let kept: HashSet<Sha256Hash> = self
            .pending
            .iter()
            .filter(|r| r.source_id == source_id)
            .map(|r| r.text_sha256)
            .collect();
        self.flush(index).await?;
        let superseded: BTreeSet<Sha256Hash> = old
            .iter()
            .map(|r| r.text_sha256)
            .filter(|n| !kept.contains(n))
            .collect();
        if superseded.is_empty() {
            return Ok(());
        }
        let Some(table) = documents_table(index, false).await? else {
            return Ok(());
        };
        for name in superseded {
            let still = read_rows(&table, Some(&format!("text_sha256 = '{}'", name.to_hex())))
                .await?
                .len();
            if still > 0 {
                tracing::debug!(%name, still, source_id, "text store: superseded text still named");
                continue;
            }
            let path = self.texts_dir.join(name.to_hex());
            match std::fs::remove_file(&path) {
                Ok(()) => tracing::debug!(%name, source_id, "text store: superseded text removed"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    tracing::debug!(%name, source_id, "text store: superseded text was never stored")
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    fn hash_file(&mut self, path: &Path) -> Result<Sha256Hash> {
        if let Some((p, h)) = &self.last_file {
            if p == path {
                return Ok(*h);
            }
        }
        let h = Sha256Hash::of_reader(std::fs::File::open(path)?)?;
        self.last_file = Some((path.to_path_buf(), h));
        Ok(h)
    }

    fn write_text(&mut self, name: &Sha256Hash, text: &str) -> Result<()> {
        let hex = name.to_hex();
        let path = self.texts_dir.join(&hex);
        if path.is_file() {
            // Content-addressed: the same name is the same bytes.
            self.texts_present += 1;
            return Ok(());
        }
        std::fs::create_dir_all(&self.texts_dir)?;
        let tmp = self.texts_dir.join(format!(".{hex}.tmp"));
        std::fs::write(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, &path)?;
        self.texts_written += 1;
        Ok(())
    }
}

impl CorpusIndex {
    /// Whether this index carries a text store covering every document.
    pub fn text_store(&self) -> bool {
        read_meta(&self.path())
            .map(|m| m.text_store)
            .unwrap_or(false)
    }

    /// Record whether this index carries a text store. Set by the first
    /// writer on an empty index and by merges, which keep it only when every
    /// input had one.
    pub fn set_text_store(&self, on: bool) -> Result<()> {
        let dir = self.path();
        let mut meta = read_meta(&dir)?;
        meta.text_store = on;
        write_meta(&dir, &meta)
    }

    /// Read one stored text by its name, with every document whose text it is.
    /// The bytes are rehashed on read: a file that does not match its name is
    /// an error, never evidence.
    pub async fn text(&self, name: &Sha256Hash) -> Result<TextLookup<StoredText>> {
        if !self.text_store() {
            tracing::debug!(corpus = %self.corpus_id(), %name, "text read: texts not stored");
            return Ok(Err(TextAbsence::TextsNotStored));
        }
        let mut documents = match self.documents_for(&[*name]).await? {
            Ok(mut by_name) => by_name.remove(name).unwrap_or_default(),
            Err(absence) => return Ok(Err(absence)),
        };
        if documents.is_empty() {
            tracing::debug!(corpus = %self.corpus_id(), %name, "text read: not held");
            return Ok(Err(TextAbsence::NotHeld));
        }
        if !documents.iter().any(|d| d.text_stored) {
            tracing::debug!(corpus = %self.corpus_id(), %name, "text read: text not stored");
            return Ok(Err(TextAbsence::TextNotStored));
        }
        let path = Corpus::texts_in(self.path()).join(name.to_hex());
        let bytes = std::fs::read(&path).map_err(|e| {
            tracing::error!(path = %path.display(), error = %e, "text store: recorded text unreadable");
            Error::Database(format!("text {name} is recorded as stored but unreadable: {e}"))
        })?;
        if Sha256Hash::of(&bytes) != *name {
            tracing::error!(path = %path.display(), "text store: file does not match its name");
            return Err(Error::Database(format!(
                "text {name} at {} does not hash to its name",
                path.display()
            )));
        }
        let text = String::from_utf8(bytes)
            .map_err(|e| Error::Database(format!("text {name} is not UTF-8: {e}")))?;
        documents.sort_by(|a, b| (&a.source_id, a.ordinal).cmp(&(&b.source_id, b.ordinal)));
        Ok(Ok(StoredText { text, documents }))
    }

    /// The records of every named text this corpus holds, for a page of hits.
    /// A name the corpus does not hold is absent from the map.
    pub async fn documents_for(
        &self,
        names: &[Sha256Hash],
    ) -> Result<TextLookup<HashMap<Sha256Hash, Vec<DocumentRecord>>>> {
        if !self.text_store() {
            return Ok(Err(TextAbsence::TextsNotStored));
        }
        let mut out: HashMap<Sha256Hash, Vec<DocumentRecord>> = HashMap::new();
        let Some(table) = documents_table(self, false).await? else {
            return Ok(Ok(out));
        };
        let wanted: BTreeSet<Sha256Hash> = names.iter().copied().collect();
        let wanted: Vec<Sha256Hash> = wanted.into_iter().collect();
        for page in wanted.chunks(500) {
            let list: Vec<String> = page.iter().map(|n| format!("'{}'", n.to_hex())).collect();
            let filter = format!("text_sha256 IN ({})", list.join(","));
            for row in read_rows(&table, Some(&filter)).await? {
                out.entry(row.text_sha256).or_default().push(row);
            }
        }
        Ok(Ok(out))
    }

    /// Every record this corpus holds, identical rows collapsed, with the
    /// record table's Lance version (`None` before the first record). The
    /// library digest is computed from these by corpus-engine through
    /// `oicp_types::evidence::texts_digest_preimage`, the one derivation, and
    /// can be memoised on the version. One full scan of the record table.
    pub async fn documents(&self) -> Result<TextLookup<(Option<u64>, Vec<DocumentRecord>)>> {
        if !self.text_store() {
            return Ok(Err(TextAbsence::TextsNotStored));
        }
        let Some(table) = documents_table(self, false).await? else {
            return Ok(Ok((None, Vec::new())));
        };
        let version = table
            .version()
            .await
            .map_err(|e| Error::Database(format!("documents version: {e}")))?;
        Ok(Ok((Some(version), read_rows(&table, None).await?)))
    }

    /// The stored-text name each chunk was cut from. A chunk without one (a
    /// pre-v4 row, a producer with no document) is absent from the map.
    pub async fn chunk_text_sha256s(&self, chunk_ids: &[u64]) -> Result<HashMap<u64, Sha256Hash>> {
        let mut out = HashMap::new();
        for page in chunk_ids.chunks(500) {
            let list: Vec<String> = page.iter().map(u64::to_string).collect();
            let batches: Vec<RecordBatch> = self
                .table()
                .query()
                .only_if(format!("id IN ({})", list.join(",")))
                .select(Select::Columns(vec!["id".into(), "text_sha256".into()]))
                .execute()
                .await
                .map_err(|e| Error::Database(format!("chunk text names query: {e}")))?
                .try_collect()
                .await
                .map_err(|e| Error::Database(format!("chunk text names collect: {e}")))?;
            for batch in &batches {
                let ids = int64_col(batch, "id")?;
                let names = str_col(batch, "text_sha256")?;
                for i in 0..batch.num_rows() {
                    if names.is_null(i) {
                        continue;
                    }
                    out.insert(ids.value(i) as u64, parse_name(names.value(i))?);
                }
            }
        }
        Ok(out)
    }

    /// Union another index directory's texts and records into this one —
    /// idempotent, because the names are content hashes and the records a
    /// set. Text files are hard-linked where the filesystem allows (they are
    /// immutable), else copied. Whether the result still covers every
    /// document is the caller's flag to set ([`set_text_store`](Self::set_text_store)).
    pub async fn union_texts_from(&self, src_dir: &Path) -> Result<usize> {
        let (src_texts, dst_texts) = (Corpus::texts_in(src_dir), Corpus::texts_in(self.path()));
        let (mut linked, mut present) = (0u64, 0u64);
        if src_texts.is_dir() {
            std::fs::create_dir_all(&dst_texts)?;
            for entry in std::fs::read_dir(&src_texts)? {
                let entry = entry?;
                let name = entry.file_name();
                if name.to_string_lossy().starts_with('.') || !entry.file_type()?.is_file() {
                    continue; // an interrupted write's temp file
                }
                let to = dst_texts.join(&name);
                if to.is_file() {
                    present += 1;
                    continue;
                }
                if std::fs::hard_link(entry.path(), &to).is_err() {
                    std::fs::copy(entry.path(), &to)?;
                }
                linked += 1;
            }
        }
        let src = CorpusIndex::open(src_dir).await?;
        let incoming = match documents_table(&src, false).await? {
            Some(t) => read_rows(&t, None).await?,
            None => Vec::new(),
        };
        let mut rows_added = 0usize;
        if !incoming.is_empty() {
            let existing = match documents_table(self, false).await? {
                Some(t) => read_rows(&t, None).await?,
                None => Vec::new(),
            };
            let mut seen: HashSet<_> = existing.iter().map(|r| r.key()).collect();
            let fresh: Vec<DocumentRecord> = incoming
                .iter()
                .filter(|r| seen.insert(r.key()))
                .cloned()
                .collect();
            rows_added = fresh.len();
            if !fresh.is_empty() {
                let table = documents_table(self, true)
                    .await?
                    .ok_or_else(|| Error::Database("documents table not created".into()))?;
                table
                    .add(vec![rows_to_batch(&fresh)?])
                    .execute()
                    .await
                    .map_err(|e| Error::Database(format!("documents union insert: {e}")))?;
            }
        }
        tracing::info!(
            from = %src_dir.display(),
            into = %self.path().display(),
            texts_linked = linked,
            texts_already_present = present,
            rows_added,
            "text store: union"
        );
        Ok(rows_added)
    }
}

fn documents_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("text_sha256", DataType::Utf8, false),
        Field::new("extractor", DataType::Utf8, false),
        Field::new("source_id", DataType::Utf8, false),
        Field::new("source_sha256", DataType::Utf8, true),
        Field::new("ordinal", DataType::Int64, false),
        Field::new("metadata", DataType::Utf8, true),
        Field::new("text_stored", DataType::Boolean, false),
    ]))
}

/// The record table, opened fresh so a write by another handle is seen.
/// `None` when it does not exist and `create` is false.
async fn documents_table(index: &CorpusIndex, create: bool) -> Result<Option<lancedb::Table>> {
    let db = index.connection();
    let names = db
        .table_names()
        .execute()
        .await
        .map_err(|e| Error::Database(format!("table names: {e}")))?;
    if names.iter().any(|n| n == DOCUMENTS_TABLE) {
        let t = db
            .open_table(DOCUMENTS_TABLE)
            .execute()
            .await
            .map_err(|e| Error::Database(format!("open documents: {e}")))?;
        return Ok(Some(t));
    }
    if !create {
        return Ok(None);
    }
    let t = db
        .create_empty_table(DOCUMENTS_TABLE, documents_schema())
        .execute()
        .await
        .map_err(|e| Error::Database(format!("create documents: {e}")))?;
    Ok(Some(t))
}

fn rows_to_batch(rows: &[DocumentRecord]) -> Result<RecordBatch> {
    let hex = |h: &Sha256Hash| h.to_hex();
    RecordBatch::try_new(
        documents_schema(),
        vec![
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| hex(&r.text_sha256)),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.extractor.as_str()),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.source_id.as_str()),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|r| r.source_sha256.as_ref().map(hex))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from_iter_values(
                rows.iter().map(|r| r.ordinal as i64),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|r| r.metadata.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(BooleanArray::from(
                rows.iter().map(|r| r.text_stored).collect::<Vec<_>>(),
            )),
        ],
    )
    .map_err(|e| Error::Serialization(format!("documents batch: {e}")))
}

/// Every record matching `filter`, identical rows collapsed.
async fn read_rows(table: &lancedb::Table, filter: Option<&str>) -> Result<Vec<DocumentRecord>> {
    let mut q = table.query();
    if let Some(f) = filter {
        q = q.only_if(f.to_string());
    }
    let batches: Vec<RecordBatch> = q
        .execute()
        .await
        .map_err(|e| Error::Database(format!("documents query: {e}")))?
        .try_collect()
        .await
        .map_err(|e| Error::Database(format!("documents collect: {e}")))?;
    let mut out: Vec<DocumentRecord> = Vec::new();
    let mut seen = HashSet::new();
    for batch in &batches {
        let text = str_col(batch, "text_sha256")?;
        let extractor = str_col(batch, "extractor")?;
        let source_id = str_col(batch, "source_id")?;
        let source = str_col(batch, "source_sha256")?;
        let ordinal = int64_col(batch, "ordinal")?;
        let metadata = str_col(batch, "metadata")?;
        let stored = batch
            .column_by_name("text_stored")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>())
            .ok_or_else(|| Error::Database("documents: text_stored column".into()))?;
        for i in 0..batch.num_rows() {
            let row = DocumentRecord {
                text_sha256: parse_name(text.value(i))?,
                extractor: extractor.value(i).to_string(),
                source_id: source_id.value(i).to_string(),
                source_sha256: if source.is_null(i) {
                    None
                } else {
                    Some(parse_name(source.value(i))?)
                },
                ordinal: ordinal.value(i) as u32,
                metadata: (!metadata.is_null(i)).then(|| metadata.value(i).to_string()),
                text_stored: stored.value(i),
            };
            let key = (
                row.text_sha256,
                row.source_id.clone(),
                row.ordinal,
                row.source_sha256,
                row.extractor.clone(),
            );
            if seen.insert(key) {
                out.push(row);
            }
        }
    }
    Ok(out)
}

fn parse_name(s: &str) -> Result<Sha256Hash> {
    Sha256Hash::from_hex(s)
        .ok_or_else(|| Error::Database(format!("text store: {s:?} is not a sha256 name")))
}

fn str_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| Error::Database(format!("column {name} missing or not text")))
}

fn int64_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a Int64Array> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
        .ok_or_else(|| Error::Database(format!("column {name} missing or not int64")))
}

#[cfg(test)]
#[path = "tests/texts_tests.rs"]
mod tests;
