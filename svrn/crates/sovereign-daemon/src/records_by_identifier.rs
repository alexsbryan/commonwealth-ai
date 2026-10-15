// SPDX-License-Identifier: AGPL-3.0-or-later
//! Records by identifier (OICP v0.6 §2.16, `ingest:lookups`). A document
//! whose declared metadata carries an identifier this host has a resolver
//! for (a `DOI`, an `ISBN`) has a record: its declared metadata, merged with
//! what the resolver returned when the install allowed a lookup. Every
//! declared field is kept, a looked-up field enters only where none is
//! declared, and each field names its origin. An identifier that does not
//! resolve, or a resolver that does not answer, is named and adds nothing.
//!
//! Which variable goes to which resolver is the data file
//! (`data/record_resolvers.toml`, [`ResolverTable`]); how each vendor's reply
//! reads is `vendors`, one reader per reply shape; the request leaves through
//! [`RecordFetch`], so tests drive recorded replies offline, and the one
//! network implementation ([`WebFetch`]) takes its client from the egress
//! boundary (`sovereign_contracts::egress::search_client`). A lookup is sent
//! only through [`RecordLookups::Allowed`], which only
//! [`RecordLookups::for_install`] makes, from the install's own allowance.
//!
//! The records are kept with the install in the corpus's `_records.json`
//! ([`InstallRecords`], keyed by text name), written before the install's
//! documents are, so the first poll listing a document lists its record:
//! progress gives it beside each ready document, and a hit's document (and
//! its span's) carries the same one ([`attach_to_hits`]). A text's declared
//! metadata is its lowest `(source, ordinal)` stored record's
//! (`oicp_evidence::record_for`). A document with no text name (past the
//! listed cap, or in a corpus that keeps no texts) has no record.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

use corpus_index::corpus::Corpus;
use corpus_index::index::CorpusIndex;
use corpus_index::ingest_port::daemon::{DocumentOutcome, DocumentState};
use futures::StreamExt;
use kernel_types::Sha256Hash;
use oicp_types::record::DECLARED;
use oicp_types::{KnowledgeResult, LookupOutcome, MergedRecord, RecordLookup};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[cfg(any(test, feature = "test-doubles"))]
#[path = "records_by_identifier/recorded.rs"]
mod recorded;
#[path = "records_by_identifier/retry.rs"]
mod retry;
#[path = "records_by_identifier/vendors.rs"]
mod vendors;

pub use retry::{retry_last_install, retry_unreachable};

#[cfg(any(test, feature = "test-doubles"))]
pub use recorded::RecordedFetch;

/// The data file, vendored into the binary.
pub const DATA_FILE: &str = include_str!("../data/record_resolvers.toml");

/// What the data file says. Every key is required and an unknown key is an
/// error, so a typo never reads as a default.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolverTable {
    /// The CSL variables a reply may add to a record.
    pub keep: BTreeSet<String>,
    /// Distinct identifiers asked at once.
    pub concurrency: usize,
    /// One row per CSL variable this host resolves.
    #[serde(rename = "resolver")]
    pub resolvers: Vec<ResolverRow>,
}

/// One CSL variable and the resolver it is looked up with.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResolverRow {
    /// The CSL variable carrying the identifier: `DOI`, `ISBN`.
    pub variable: String,
    /// The resolver's name, each looked-up field's origin.
    pub name: String,
    /// How a declared value becomes the one asked for.
    pub normalize: Normalize,
    /// The request, the identifier's place named `{value}`.
    pub url: String,
    /// The shape the resolver answers in.
    pub reply: ReplyShape,
}

/// How a declared identifier is normalised before it is asked for. A closed
/// set; another is a code change and a row naming it.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Normalize {
    /// Drop a `https://doi.org/`, `http://dx.doi.org/` or `doi:` prefix.
    Doi,
    /// Drop hyphens and spaces.
    Isbn,
}

/// The shape a resolver answers in; `vendors` holds one reader per shape.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplyShape {
    /// A CSL-JSON object, `404` for an identifier it does not know
    /// (Crossref's `transform` route).
    Csl,
    /// Open Library's Books API (`jscmd=data`): an object keyed
    /// `ISBN:<value>`, empty for an ISBN it does not know.
    OpenLibraryBooks,
}

impl ResolverTable {
    /// The vendored data file. Checked by this crate's tests, so a binary
    /// whose file does not parse is not built green; should one run anyway,
    /// each install says so and records only what was declared.
    pub fn from_data_file() -> Result<Self, String> {
        Self::parse(DATA_FILE)
    }

    pub fn parse(toml_text: &str) -> Result<Self, String> {
        let t: Self = toml::from_str(toml_text).map_err(|e| e.to_string())?;
        if t.concurrency == 0 {
            return Err("concurrency must be at least 1".into());
        }
        let mut variables = BTreeSet::new();
        for r in &t.resolvers {
            if !r.url.contains("{value}") {
                return Err(format!("resolver `{}`'s url names no {{value}}", r.name));
            }
            if !variables.insert(r.variable.as_str()) {
                return Err(format!("`{}` has two resolvers", r.variable));
            }
        }
        Ok(t)
    }

    /// Each identifier `declared` carries that a row resolves, as the row's
    /// index and the declared value. A non-string value is no identifier.
    fn identifiers<'a>(&self, declared: &'a Map<String, Value>) -> Vec<(usize, &'a str)> {
        self.resolvers
            .iter()
            .enumerate()
            .filter_map(|(i, row)| match declared.get(&row.variable) {
                Some(Value::String(v)) => Some((i, v.as_str())),
                Some(other) => {
                    tracing::debug!(variable = %row.variable, value = %other, "records: an identifier that is not a string, not looked up");
                    None
                }
                None => None,
            })
            .collect()
    }
}

impl Normalize {
    pub fn apply(self, value: &str) -> String {
        let v = value.trim();
        match self {
            Normalize::Doi => {
                let lower = v.to_ascii_lowercase();
                let prefix = [
                    "https://doi.org/",
                    "http://doi.org/",
                    "https://dx.doi.org/",
                    "http://dx.doi.org/",
                    "doi:",
                ]
                .into_iter()
                .find(|p| lower.starts_with(p))
                .map_or(0, str::len);
                v[prefix..].trim().to_string()
            }
            Normalize::Isbn => v
                .chars()
                .filter(|c| *c != '-' && !c.is_whitespace())
                .collect(),
        }
    }
}

impl ResolverRow {
    /// The URL that asks this resolver for `value`.
    pub fn url_for(&self, value: &str) -> String {
        let v = self.normalize.apply(value);
        let encoded: String = v
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect();
        self.url.replace("{value}", &encoded)
    }
}

/// A resolver's answer as it came: its status and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchReply {
    pub status: u16,
    pub body: String,
}

/// Where a lookup leaves this host. `Err` names why there was no answer.
#[async_trait::async_trait]
pub trait RecordFetch: Send + Sync {
    async fn get(&self, url: &str) -> Result<FetchReply, String>;
}

/// The one network [`RecordFetch`]: its client is the egress boundary's
/// query client, built on the first lookup this daemon sends.
#[derive(Default)]
pub struct WebFetch {
    client: OnceLock<Result<reqwest::Client, String>>,
}

#[async_trait::async_trait]
impl RecordFetch for WebFetch {
    async fn get(&self, url: &str) -> Result<FetchReply, String> {
        let client = self
            .client
            .get_or_init(|| sovereign_contracts::egress::search_client().map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| format!("the egress client did not build: {e}"))?;
        let resp = client
            .get(url)
            .header(
                reqwest::header::USER_AGENT,
                concat!(
                    "sovereign-daemon/",
                    env!("CARGO_PKG_VERSION"),
                    " (OICP records by identifier)"
                ),
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(|e| e.to_string())?;
        Ok(FetchReply { status, body })
    }
}

/// The daemon's default [`RecordFetch`].
pub fn web() -> std::sync::Arc<dyn RecordFetch> {
    std::sync::Arc::new(WebFetch::default())
}

/// What a resolver said of one identifier.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolverAnswer {
    /// Its fields, before the data file's `keep` filter.
    Found(Map<String, Value>),
    /// It does not know the identifier.
    Unresolved,
    /// It could not be asked or did not answer, and why.
    Unreachable(String),
}

/// Whether an install's lookups may leave this host. `Allowed` carries the
/// only way out, so a lookup for an install that did not allow one has
/// nothing to be sent through.
pub enum RecordLookups<'a> {
    NotAllowed,
    Allowed(&'a dyn RecordFetch),
}

impl<'a> RecordLookups<'a> {
    /// The install's own allowance (`CorpusInstallRequest::allow_lookups`):
    /// the one place it becomes a way out.
    pub fn for_install(allow_lookups: bool, fetch: &'a dyn RecordFetch) -> Self {
        if allow_lookups {
            RecordLookups::Allowed(fetch)
        } else {
            RecordLookups::NotAllowed
        }
    }
}

/// Each distinct `(resolver, value)` the declared metadata carries, and what
/// its resolver answered: asked once each, `concurrency` at a time. `None`
/// when the install did not allow lookups: nothing was asked.
pub async fn resolve<'m>(
    table: &ResolverTable,
    declared: impl IntoIterator<Item = &'m Map<String, Value>>,
    lookups: &RecordLookups<'_>,
) -> Option<BTreeMap<(String, String), ResolverAnswer>> {
    let RecordLookups::Allowed(fetch) = lookups else {
        tracing::debug!("records: the install did not allow lookups, none sent");
        return None;
    };
    let wanted: BTreeSet<(usize, String)> = declared
        .into_iter()
        .flat_map(|d| table.identifiers(d))
        .map(|(i, v)| (i, v.to_string()))
        .collect();
    let answers = futures::stream::iter(wanted)
        .map(|(i, value)| async move {
            let row = &table.resolvers[i];
            let url = row.url_for(&value);
            let answer = vendors::read(row, &value, fetch.get(&url).await);
            match &answer {
                ResolverAnswer::Found(fields) => tracing::debug!(resolver = %row.name, %value, fields = fields.len(), "records: resolved"),
                ResolverAnswer::Unresolved => tracing::debug!(resolver = %row.name, %value, "records: unresolved, nothing taken"),
                ResolverAnswer::Unreachable(why) => tracing::warn!(resolver = %row.name, %value, %url, %why, "records: unreachable, nothing taken"),
            }
            ((row.name.clone(), value), answer)
        })
        .buffer_unordered(table.concurrency)
        .collect::<BTreeMap<_, _>>()
        .await;
    Some(answers)
}

/// `declared`'s record: `None` when it carries no identifier a row resolves.
/// `answers` is [`resolve`]'s: `None` names every lookup `not_allowed`; an
/// identifier it lacks was not answered, so `unreachable`. Only a `Found`
/// answer adds fields, those in `keep` that no declaration (and no earlier
/// resolver) holds.
pub fn merge(
    table: &ResolverTable,
    declared: &Map<String, Value>,
    answers: Option<&BTreeMap<(String, String), ResolverAnswer>>,
) -> Option<MergedRecord> {
    let ids = table.identifiers(declared);
    if ids.is_empty() {
        return None;
    }
    let mut csl = declared.clone();
    let mut origins: BTreeMap<String, String> = declared
        .keys()
        .map(|k| (k.clone(), DECLARED.to_string()))
        .collect();
    let mut lookups = Vec::with_capacity(ids.len());
    for (i, value) in ids {
        let row = &table.resolvers[i];
        let outcome = match answers.map(|a| a.get(&(row.name.clone(), value.to_string()))) {
            None => LookupOutcome::NotAllowed,
            Some(Some(ResolverAnswer::Found(fields))) => {
                for (k, v) in fields {
                    if table.keep.contains(k) && !csl.contains_key(k) {
                        csl.insert(k.clone(), v.clone());
                        origins.insert(k.clone(), row.name.clone());
                    }
                }
                LookupOutcome::Resolved
            }
            Some(Some(ResolverAnswer::Unresolved)) => LookupOutcome::Unresolved,
            Some(Some(ResolverAnswer::Unreachable(_))) | Some(None) => LookupOutcome::Unreachable,
        };
        tracing::debug!(identifier = %row.variable, %value, resolver = %row.name, ?outcome, "records: lookup merged");
        lookups.push(RecordLookup {
            identifier: row.variable.clone(),
            value: value.to_string(),
            resolver: row.name.clone(),
            outcome,
        });
    }
    Some(MergedRecord {
        csl,
        origins,
        lookups,
    })
}

/// One install's records, kept with it (`Corpus::records_path`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRecords {
    /// Whether the install allowed lookups: part of the install (§2.16).
    #[serde(default)]
    pub allow_lookups: bool,
    /// Each ready document's record, by its text name.
    #[serde(default)]
    pub records: BTreeMap<String, MergedRecord>,
    /// Why the install could not make its records, when it could not: kept
    /// so a later read says "unreadable" and not "kept none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread: Option<String>,
}

/// What reading an install's `_records.json` found. The three are never
/// folded into one another: "kept none" and "could not read" are different
/// claims, and only the first may be answered as no record (ARCH 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordsRead {
    /// The install kept its records (possibly an empty set).
    Kept(InstallRecords),
    /// No file: a corpus installed before records existed, which allowed no
    /// lookup.
    NotWritten,
    /// A file that does not read or parse, or one the install wrote to say
    /// its own records could not be made ([`InstallRecords::unread`]).
    Unreadable(String),
}

impl InstallRecords {
    /// The records at `path`: kept, not written, or unreadable with why.
    pub fn read(path: &Path) -> RecordsRead {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(path = %path.display(), "records: none written");
                return RecordsRead::NotWritten;
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "records: the records do not read");
                return RecordsRead::Unreadable(format!("does not read: {e}"));
            }
        };
        match serde_json::from_str::<Self>(&raw) {
            Ok(Self {
                unread: Some(why), ..
            }) => {
                tracing::warn!(path = %path.display(), %why, "records: the install could not keep its records");
                RecordsRead::Unreadable(why)
            }
            Ok(kept) => RecordsRead::Kept(kept),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "records: the records do not parse");
                RecordsRead::Unreadable(format!("does not parse: {e}"))
            }
        }
    }

    /// `corpus`'s records.
    pub fn of(corpus: &Corpus) -> RecordsRead {
        Self::read(&corpus.records_path())
    }

    /// The record of the text `text_sha256` names.
    pub fn record(&self, text_sha256: &str) -> Option<MergedRecord> {
        self.records.get(text_sha256).cloned()
    }

    fn write(&self, corpus: &Corpus) -> std::io::Result<()> {
        let target = corpus.records_path();
        let tmp = target.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(&tmp, &target)
    }
}

/// The records of `documents`' ready texts, read from `index`'s stored
/// records and looked up through `lookups`.
pub async fn records_of(
    index: &CorpusIndex,
    documents: &[DocumentOutcome],
    table: &ResolverTable,
    lookups: &RecordLookups<'_>,
) -> Result<BTreeMap<String, MergedRecord>, String> {
    let names: Vec<Sha256Hash> = documents
        .iter()
        .filter_map(|d| match &d.state {
            DocumentState::Ready {
                text_sha256: Some(h),
            } => Some(*h),
            _ => None,
        })
        .collect();
    if names.is_empty() {
        tracing::debug!("records: no ready document names a text, no record");
        return Ok(BTreeMap::new());
    }
    let stored = match index.documents_for(&names).await {
        Ok(Ok(s)) => s,
        Ok(Err(absence)) => {
            tracing::debug!(
                ?absence,
                "records: the corpus keeps no records of its texts, no record"
            );
            return Ok(BTreeMap::new());
        }
        Err(e) => return Err(e.to_string()),
    };
    let mut declared: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    for (name, recs) in &stored {
        let metadata =
            crate::oicp_evidence::record_for(recs, None).and_then(|r| r.metadata.as_deref());
        match metadata.map(serde_json::from_str::<Value>) {
            Some(Ok(Value::Object(m))) => {
                declared.insert(name.to_hex(), m);
            }
            Some(Ok(_)) | Some(Err(_)) => {
                tracing::warn!(text = %name, "records: the declared metadata is not a JSON object, no record");
            }
            None => {}
        }
    }
    let answers = resolve(table, declared.values(), lookups).await;
    Ok(declared
        .iter()
        .filter_map(|(name, d)| merge(table, d, answers.as_ref()).map(|r| (name.clone(), r)))
        .collect())
}

/// Look up and keep the records of an install that returned, before its
/// documents are recorded, so the first poll listing a document lists its
/// record. The allowance is kept whatever comes of the rest.
pub async fn record_install(
    corpus: &Corpus,
    documents: &[DocumentOutcome],
    allow_lookups: bool,
    fetch: &dyn RecordFetch,
) {
    let lookups = RecordLookups::for_install(allow_lookups, fetch);
    let made: Result<BTreeMap<String, MergedRecord>, String> = match ResolverTable::from_data_file()
    {
        Err(e) => Err(format!("the resolver data file does not load: {e}")),
        Ok(table) => match corpus.open().await {
            Err(e) => Err(format!("the installed corpus does not open: {e}")),
            Ok(index) => records_of(&index, documents, &table, &lookups)
                .await
                .map_err(|e| format!("the stored records do not read: {e}")),
        },
    };
    let kept = match made {
        Ok(records) => InstallRecords {
            allow_lookups,
            records,
            unread: None,
        },
        Err(why) => {
            tracing::warn!(corpus = %corpus, %why, "records: not made; kept as unreadable, never as none");
            InstallRecords {
                allow_lookups,
                records: BTreeMap::new(),
                unread: Some(why),
            }
        }
    };
    match kept.write(corpus) {
        Ok(()) => {
            tracing::info!(corpus = %corpus, allow_lookups, records = kept.records.len(), "records: kept with the install")
        }
        Err(e) => {
            tracing::warn!(corpus = %corpus, error = %e, "records: did not write; progress and hits list none for this corpus")
        }
    }
}

/// Whether `corpus`'s last install allowed lookups; `None` when its records
/// do not read, which is unknown and never "did not allow".
pub fn allowed_by_last_install(corpus: &Corpus) -> Option<bool> {
    match InstallRecords::of(corpus) {
        RecordsRead::Kept(r) => Some(r.allow_lookups),
        RecordsRead::NotWritten => Some(false),
        RecordsRead::Unreadable(why) => {
            tracing::warn!(corpus = %corpus, %why, "records: the last install's allowance is unknown");
            None
        }
    }
}

/// Give each of `corpus_id`'s hits' documents, and its span's, the record
/// progress lists beside it. `index_root` is that corpus's index directory;
/// another corpus's hits are left as they are.
pub fn attach_to_hits(index_root: &Path, corpus_id: &str, hits: &mut [KnowledgeResult]) {
    let path = Corpus::records_in(index_root);
    let mine = |h: &KnowledgeResult| h.corpus_id == corpus_id && h.document.is_some();
    if !hits.iter().any(mine) || !path.is_file() {
        tracing::debug!(
            corpus = corpus_id,
            "records: no hit document or no records, none attached"
        );
        return;
    }
    let records = match InstallRecords::read(&path) {
        RecordsRead::Kept(r) => r,
        RecordsRead::NotWritten => return,
        RecordsRead::Unreadable(why) => {
            // §2.16 has no field to say "records unreadable" beside a
            // document; the hit's place for a damaged record is
            // `document_absent` (v0.5 §3), and a span's is `span_absent`.
            tracing::warn!(corpus = corpus_id, %why, "records: unreadable; hits name their document unreadable");
            for h in hits.iter_mut().filter(|h| h.corpus_id == corpus_id) {
                if h.document.take().is_some() {
                    h.document_absent =
                        Some(oicp_types::evidence::reasons::DOCUMENT_UNREADABLE.into());
                }
                if h.span.take().is_some() {
                    h.span_absent = Some(oicp_types::evidence::reasons::DOCUMENT_UNREADABLE.into());
                }
            }
            return;
        }
    };
    let mut carried = 0usize;
    for h in hits.iter_mut().filter(|h| h.corpus_id == corpus_id) {
        if let Some(d) = h.document.as_mut() {
            d.record = records.record(&d.text_sha256);
            carried += usize::from(d.record.is_some());
        }
        if let Some(s) = h.span.as_mut() {
            s.document.record = records.record(&s.document.text_sha256);
        }
    }
    tracing::debug!(corpus = corpus_id, carried, "records: attached to hits");
}

#[cfg(test)]
#[path = "records_by_identifier/tests.rs"]
mod tests;
