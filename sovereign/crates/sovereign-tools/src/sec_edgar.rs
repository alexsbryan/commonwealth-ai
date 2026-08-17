//! `sec_edgar` — the custom acquirer that turns a TICKER into an
//! acquired SEC filings corpus, so installing one stops requiring a
//! repo clone and a bash script.
//!
//! # Why this is an acquirer and not `[parameters]` interpolation
//!
//! `[parameters]` does string SUBSTITUTION. Ticker -> corpus for EDGAR
//! is RESOLUTION: the document URL must be COMPOSED from three parallel
//! arrays in the submissions JSON (`form[i]` / `accessionNumber[i]` /
//! `primaryDocument[i]`), and `http_api`'s `FollowConfig.document_url_path`
//! is a plain JSONPath selecting URL *strings* it fetches verbatim — no
//! base-url join, no templating, no fetch chaining. So no `{ticker}`
//! placeholder in any built-in acquirer can reach the filing. This
//! confirms `proxy-company/recipe.toml`'s finding from a second endpoint
//! rather than reversing it: EFTS carries no document URL at all, and
//! submissions carries one that must be assembled.
//!
//! # What this acquirer does and deliberately does NOT do
//!
//! It fetches BYTES and shapes them for the `plaintext` extractor:
//!
//! 1. ticker -> CIK, via `www.sec.gov/files/company_tickers.json`
//! 2. the 10-K filings in window, via `data.sec.gov/submissions/CIK##########.json`
//! 3. ONE selected filing, with every other in-window 10-K NAMED as skipped
//! 4. the primary document, cleaned to prose part files under `docs/prose/`
//! 5. `companyfacts.json`, saved RAW under `raw/`
//!
//! It does NOT interpret companyfacts. `sec_facts_render::render` is THE
//! one decider (ARCH §10.6) for `(company, concept, period) -> figure`:
//! alias-chain precedence, instant-vs-duration typing, the annual-10-K
//! selection window, the never-consult-`fy` rule, and the refusals that
//! NAME the nearest available period rather than substituting it. Rendering
//! facts here would be a SECOND implementation of that decider, so this
//! acquirer CALLS it (step 6) rather than reimplementing any part of it,
//! and saves `companyfacts.json` untouched under `raw/` as the decider's
//! input of record.
//!
//! # Where the rendered outputs land, and why each one there
//!
//! [`render`](crate::sec_facts_render::render) is pure — it returns data
//! and [`place_rendered`] writes it, mirroring `setup-sec-corpus.sh`'s
//! placement exactly so the two install paths produce the same corpus:
//!
//! - `docs/facts/*.txt` — INGESTED DOCUMENTS. Written BEFORE this
//!   function returns `docs/`, because the engine extracts from the
//!   returned directory; a fact file placed after ingest is a fact file
//!   retrieval never sees.
//! - `raw/sec_facts.json` — the typed sidecar, STAGED where
//!   [`install_fact_sidecar`] finds it and copies it into the corpus
//!   index dir synchronously post-ingest.
//! - `raw/_unmapped_concepts.json` — the coverage deliverable (F5). In
//!   `raw/`, not `docs/facts/`, so the `plaintext` extractor never
//!   ingests it as prose.
//!
//! # Glassbox
//!
//! Every decision is a `tracing` event on target `sec_edgar`: which ticker
//! resolved to which CIK, which filings were in window, which ONE was
//! selected, and which were skipped and why. A silent skip during install
//! is the same defect as a silent skip during ingest.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::engine::{CorpusEngine, CustomAcquirerFn};
use corpus_engine::error::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::sec_facts_render::RenderOutput;

/// The `kind` string a recipe names in `[acquire] type = "custom"`.
pub const KIND: &str = "sec_edgar";

const TICKERS_URL: &str = "https://www.sec.gov/files/company_tickers.json";

/// SEC's fair-access policy requires a User-Agent that names a REACHABLE
/// CONTACT, not merely a product. Measured 2026-08-16, same URL, same
/// second, one variable changed:
///
/// ```text
/// UA "commonwealth-ai/0.1 (sovereign corpus installer)"                    -> 403
/// UA "commonwealth-ai/0.1 (sec-filings-corpus; alexbryan01@gmail.com)"     -> 200
/// ```
///
/// So a contactless default is not a stylistic choice, it is a
/// guaranteed refusal. The recipe supplies the real value through the
/// `contact` parameter, where the user can SEE it and replace it with
/// their own; this constant is only the fallback for a params blob that
/// omits `user_agent` entirely. `scripts/setup-sec-corpus.sh:28` carries
/// the same string for the script path.
const DEFAULT_USER_AGENT: &str = "commonwealth-ai/0.1 (sec-filings-corpus; alexbryan01@gmail.com)";

/// The concept-normalization registry, compiled in from the CANONICAL
/// `sovereign-recipes/sec-filings-company/concept-map.toml`.
///
/// Vendored rather than read from disk for the same reason corpus-engine
/// vendors `registry.toml` into its bundled snapshot (`corpus-engine/build.rs`):
/// an end user installs from the catalog, which fetches `recipe.toml`
/// alone — the recipe's sibling data files never reach their machine. A
/// runtime path lookup here would therefore work in the repo and refuse
/// in the product, which is the one failure mode this corpus exists to
/// avoid. `include_str!` registers the file as a rebuild dependency, so
/// the canonical tree and this snapshot cannot drift.
///
/// There is still exactly ONE registry: this is a copy of the same
/// bytes, not a second map. `scripts/setup-sec-corpus.sh` passes the
/// canonical file to the `sec_facts_render` example by path.
const CONCEPT_MAP_TOML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../sovereign-recipes/sec-filings-company/concept-map.toml"
));

/// Prose part sizing. Each part must become ONE chunk: the engine
/// prepends the doc title to every chunk AFTER the chunker bounds
/// content at `max_chars` (3000 in the recipe), and `recipe test`'s size
/// gate counts the prepended result — so a part must fit `max_chars`
/// minus title headroom or the gate is structurally red. See `chunk_doc`
/// in `engine/ingest_helpers.rs`.
const PROSE_TARGET_CHARS: usize = 2600;
/// Word-boundary overlap repeated between parts, so a figure or a claim
/// straddling a cut still appears whole in one part.
const PROSE_OVERLAP_CHARS: usize = 300;
/// Non-space runs at least this long are inline-XBRL ids, base64 blobs
/// or URLs, never prose. Also the FTS index drops tokens over ~40 chars
/// (tantivy `RemoveLongFilter`), so keeping them would cost index space
/// for tokens that can never be matched.
const GARBAGE_RUN_CHARS: usize = 40;

/// Deserialized from `AcquirerConfig::Custom.params` AFTER `{name}`
/// interpolation against install-time parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecEdgarAcquirerParams {
    /// The user's sole input: a ticker (`AAPL`, case-insensitive) or a
    /// bare CIK. A ticker is a LABEL; the CIK it resolves to is the
    /// essence (ARCH §7.5) and is what [`Resident`] records.
    pub ticker: String,
    /// Inclusive filing-date window for 10-K discovery, `YYYY-MM-DD`.
    #[serde(default = "default_from_date")]
    pub from_date: String,
    #[serde(default = "default_to_date")]
    pub to_date: String,
    /// Pin an exact accession instead of taking the latest in window.
    #[serde(default)]
    pub accession: Option<String>,
    /// Override the declaring User-Agent (SEC fair-access policy).
    #[serde(default)]
    pub user_agent: Option<String>,
}

fn default_from_date() -> String {
    "2024-01-01".to_string()
}

fn default_to_date() -> String {
    "2026-12-31".to_string()
}

/// One 10-K row assembled from the submissions JSON's parallel arrays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilingHit {
    pub accession: String,
    pub primary_document: String,
    pub filing_date: String,
}

impl FilingHit {
    /// `0000320193-25-000073` -> `000032019325000073`, the Archives path
    /// segment.
    fn accession_nodash(&self) -> String {
        self.accession.replace('-', "")
    }
}

/// What the acquirer decided, so the caller can log it as one event
/// rather than reconstructing it from prose.
#[derive(Debug, Clone)]
pub struct Selection {
    pub selected: FilingHit,
    /// Every other 10-K in window. NAMED, never silently dropped.
    pub skipped: Vec<FilingHit>,
}

// ---------------------------------------------------------------------------
// Pure resolution steps — no network, each independently testable
// ---------------------------------------------------------------------------

/// `AAPL` / `aapl` -> a ticker lookup; `320193` -> a CIK taken as-is.
/// The script (`setup-sec-corpus.sh:69`) accepts both and so does this.
pub fn parse_subject(arg: &str) -> Subject {
    let trimmed = arg.trim();
    if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
        // `10#` semantics: leading zeros are not octal, and a padded CIK
        // must resolve to the same company as an unpadded one.
        match trimmed.trim_start_matches('0').parse::<u64>() {
            Ok(n) => return Subject::Cik(n),
            // All zeros: not a company, fall through to the ticker path
            // so the failure names the input rather than panicking.
            Err(_) => return Subject::Cik(0),
        }
    }
    Subject::Ticker(trimmed.to_ascii_uppercase())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    Ticker(String),
    Cik(u64),
}

impl Subject {
    /// The `_downloads/` sub-directory this subject owns. `download_dir`
    /// is `index_dir.join("_downloads")` (`engine/ingest.rs:615`) and is
    /// SHARED across every corpus — the acquirer receives no corpus id,
    /// so it must namespace itself or two companies overwrite each
    /// other's bytes. Derived from the user's sole input, so no
    /// pre-ingest resolve seam is minted.
    pub fn download_slug(&self) -> String {
        match self {
            Subject::Ticker(t) => format!("sec-{}", t.to_ascii_lowercase()),
            Subject::Cik(n) => format!("sec-cik{n:010}"),
        }
    }
}

/// Find a ticker in SEC's `company_tickers.json` (an object whose values
/// are `{cik_str, ticker, title}`). Absence is REPORTED by the caller,
/// never defaulted to a near-neighbour match.
pub fn resolve_ticker(tickers: &serde_json::Value, ticker: &str) -> Option<(u64, String)> {
    let obj = tickers.as_object()?;
    for entry in obj.values() {
        let t = entry.get("ticker").and_then(|v| v.as_str())?;
        if t.eq_ignore_ascii_case(ticker) {
            let cik = entry.get("cik_str").and_then(|v| v.as_u64())?;
            let title = entry
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            return Some((cik, title));
        }
    }
    None
}

/// Pull the in-window 10-K rows out of `.filings.recent`, which is
/// PARALLEL ARRAYS rather than a list of objects: `form[i]`,
/// `accessionNumber[i]`, `primaryDocument[i]`, `filingDate[i]` are four
/// separate arrays indexed in lockstep. Sorted ascending by filing date.
pub fn in_window_10ks(
    submissions: &serde_json::Value,
    from_date: &str,
    to_date: &str,
    accession: Option<&str>,
) -> Vec<FilingHit> {
    let recent = match submissions.get("filings").and_then(|f| f.get("recent")) {
        Some(r) => r,
        None => return Vec::new(),
    };
    let arr = |k: &str| recent.get(k).and_then(|v| v.as_array());
    let (forms, accs, docs, dates) = match (
        arr("form"),
        arr("accessionNumber"),
        arr("primaryDocument"),
        arr("filingDate"),
    ) {
        (Some(f), Some(a), Some(d), Some(t)) => (f, a, d, t),
        _ => return Vec::new(),
    };
    let n = forms.len().min(accs.len()).min(docs.len()).min(dates.len());
    let mut hits = Vec::new();
    for i in 0..n {
        if forms[i].as_str() != Some("10-K") {
            continue;
        }
        let (acc, doc, date) = match (accs[i].as_str(), docs[i].as_str(), dates[i].as_str()) {
            (Some(a), Some(d), Some(t)) => (a, d, t),
            _ => continue,
        };
        let in_scope = match accession {
            Some(pinned) => acc == pinned,
            None => date >= from_date && date <= to_date,
        };
        if in_scope {
            hits.push(FilingHit {
                accession: acc.to_string(),
                primary_document: doc.to_string(),
                filing_date: date.to_string(),
            });
        }
    }
    hits.sort_by(|a, b| a.filing_date.cmp(&b.filing_date));
    hits
}

/// Take the latest in-window 10-K and NAME every other one as skipped.
/// An empty window is a refusal that states the window, never a
/// success-shaped empty corpus (ARCH §18.3).
pub fn select_filing(hits: Vec<FilingHit>, window: &str) -> Result<Selection> {
    let mut hits = hits;
    let selected = hits.pop().ok_or_else(|| {
        Error::Recipe(format!(
            "no 10-K found {window}. Nothing was installed — widen the date \
             window or pin an accession rather than accepting a partial corpus."
        ))
    })?;
    Ok(Selection {
        selected,
        skipped: hits,
    })
}

/// `https://www.sec.gov/Archives/edgar/data/<cik-no-leading-zeros>/<accession-nodash>/<primary>`.
/// The three fields come from three DIFFERENT parallel arrays — this
/// composition is precisely what `http_api` cannot express.
pub fn document_url(cik_bare: u64, hit: &FilingHit) -> String {
    format!(
        "https://www.sec.gov/Archives/edgar/data/{}/{}/{}",
        cik_bare,
        hit.accession_nodash(),
        hit.primary_document
    )
}

pub fn companyfacts_url(cik10: &str) -> String {
    format!("https://data.sec.gov/api/xbrl/companyfacts/CIK{cik10}.json")
}

pub fn submissions_url(cik10: &str) -> String {
    format!("https://data.sec.gov/submissions/CIK{cik10}.json")
}

// ---------------------------------------------------------------------------
// Text shaping
// ---------------------------------------------------------------------------

/// SEC-specific normalization applied AFTER tag stripping: drop
/// zero-width marks, fold unicode punctuation to ASCII so a quote in a
/// query matches a quote in the filing, remove inline-XBRL/URL garbage
/// runs, and collapse whitespace.
///
/// Tag stripping itself reuses [`crate::corpus::strip_html`] rather than
/// minting a third implementation (ARCH §19). That stripper emits a
/// newline for `<br>`/`<p>`/`<div>` where the bash path emits a space;
/// both are collapsed by the whitespace pass below, so the shaped text
/// agrees on everything the chunker can see.
pub fn normalize_sec_text(input: &str) -> String {
    let mut folded = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\u{200b}' | '\u{feff}' => {}
            '\u{2013}' | '\u{2014}' => folded.push('-'),
            '\u{2018}' | '\u{2019}' => folded.push('\''),
            '\u{201c}' | '\u{201d}' => folded.push('"'),
            '\u{2026}' => folded.push_str("..."),
            '\u{00a0}' => folded.push(' '),
            c => folded.push(c),
        }
    }

    // Drop non-space runs of GARBAGE_RUN_CHARS or more, then collapse
    // whitespace to single spaces. Done in one pass over whitespace-
    // separated runs, which is exactly what `\S{40,}` + `\s+` compose to.
    let mut out = String::with_capacity(folded.len());
    for run in folded.split_whitespace() {
        if run.chars().count() >= GARBAGE_RUN_CHARS {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(run);
    }
    out
}

/// Split normalized prose into part strings of at most `target` chars,
/// cut at word boundaries, each repeating up to `overlap` chars of the
/// previous part's tail.
pub fn split_parts(text: &str, target: usize, overlap: usize) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut cap = 0usize;
    for tok in text.split(' ').filter(|t| !t.is_empty()) {
        if cap + tok.len() + 1 > target && !cur.is_empty() {
            parts.push(cur.join(" "));
            let mut tail: Vec<&str> = Vec::new();
            let mut tlen = 0usize;
            for w in cur.iter().rev() {
                if tlen + w.len() + 1 > overlap {
                    break;
                }
                tail.insert(0, w);
                tlen += w.len() + 1;
            }
            cur = tail;
            cap = tlen;
        }
        cur.push(tok);
        cap += tok.len() + 1;
    }
    if !cur.is_empty() {
        parts.push(cur.join(" "));
    }
    parts
}

// ---------------------------------------------------------------------------
// The resident company — a replacement is NAMED, never silent
// ---------------------------------------------------------------------------

/// One file, at the root of the shared `_downloads/`, naming which
/// company the installed SEC corpus currently holds.
const RESIDENT_FILE: &str = "sec_edgar_resident.json";

/// Which company the SEC filings corpus holds. Durable rather than a log
/// line so a later surface (the coverage card) can render the SAME fact
/// the install decided, instead of a second surface guessing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resident {
    /// Zero-padded CIK — the company's essence (ARCH §7.5).
    pub cik: String,
    /// SEC's registrant title, for a human-readable line.
    pub entity: String,
    /// The label the user typed. A ticker is a label the SEC can
    /// reassign; it never decides identity.
    pub subject: String,
    pub accession: String,
    pub filed: String,
}

/// What installing `next` does to a corpus already holding `previous`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Residency {
    /// Nothing was installed here before.
    First,
    /// Same company — a refresh, possibly of a newer filing.
    Refresh,
    /// A DIFFERENT company takes over the single-instance corpus. This
    /// is ordinary reinstall semantics, but for this corpus the user's
    /// mental model is "I installed Apple", so the swap is stated with
    /// both companies named (ARCH §18.3 — never silently substitute).
    Replaces(Box<Resident>),
}

/// Pure classification, so the named-replacement rule is testable without
/// a filesystem or a network.
pub fn classify_residency(previous: Option<&Resident>, next_cik: &str) -> Residency {
    match previous {
        None => Residency::First,
        Some(p) if p.cik == next_cik => Residency::Refresh,
        Some(p) => Residency::Replaces(Box::new(p.clone())),
    }
}

/// Read the resident record, treating a missing or unreadable file as
/// "nothing installed" — a corrupt record must not wedge an install.
pub fn read_resident(download_dir: &Path) -> Option<Resident> {
    let raw = std::fs::read_to_string(download_dir.join(RESIDENT_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_resident(download_dir: &Path, resident: &Resident) -> Result<()> {
    let path = download_dir.join(RESIDENT_FILE);
    let body = serde_json::to_vec_pretty(resident)
        .map_err(|e| Error::Serialization(format!("resident record: {e}")))?;
    std::fs::write(path, body)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Post-install: the typed fact sidecar reaches the index dir
// ---------------------------------------------------------------------------

/// Filename of the typed fact store, in the ONE place both the writer
/// and `sec_facts.rs`'s reader agree on.
pub const FACT_SIDECAR: &str = "sec_facts.json";

/// Where the acquirer stages the sidecar, relative to `download_dir`.
fn staged_sidecar_path(download_dir: &Path, resident: &Resident) -> PathBuf {
    download_dir
        .join(parse_subject(&resident.subject).download_slug())
        .join("raw")
        .join(FACT_SIDECAR)
}

/// Place the staged typed fact store at
/// `<indexes>/<corpus_id>/sec_facts.json`, which is where
/// `SecFactsTool::resolve_corpus` looks for it.
///
/// MUST be called SYNCHRONOUSLY, before any detached post-install work.
/// The window between "corpus installed" and "sidecar present" is a
/// window in which `sec_facts` answers "no installed SEC filings
/// corpus" for a corpus the user just watched install — a silent
/// substitution of absence for not-ready (ARCH §18.3). A file copy is
/// microseconds; there is no reason to detach it.
///
/// Returns `Ok(None)` when nothing is staged, which is the case for
/// every non-SEC corpus that passes through the shared post-install
/// path. A staged sidecar that cannot be placed is an `Err` — it is
/// never collapsed into the success-shaped `None`.
pub fn install_fact_sidecar(corpus_id: &str, indexes_dir: &Path) -> Result<Option<PathBuf>> {
    let download_dir = indexes_dir.join("_downloads");
    let resident = match read_resident(&download_dir) {
        Some(r) => r,
        None => return Ok(None),
    };
    let staged = staged_sidecar_path(&download_dir, &resident);
    if !staged.exists() {
        // The company is resident but the decider has not rendered a
        // store for it. Named, not silent: the tool will refuse, and
        // this line is what explains why.
        tracing::warn!(target: "sec_edgar", corpus = %corpus_id, cik = %resident.cik,
            expected = %staged.display(),
            "sec_edgar: no typed fact store staged — figures will refuse until one is rendered");
        return Ok(None);
    }
    let dest_dir = indexes_dir.join(corpus_id);
    std::fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join(FACT_SIDECAR);
    std::fs::copy(&staged, &dest)?;
    tracing::info!(target: "sec_edgar", corpus = %corpus_id, cik = %resident.cik,
        entity = %resident.entity, path = %dest.display(),
        "sec_edgar: typed fact store installed — the sec_facts tool can now claim this corpus");
    Ok(Some(dest))
}

// ---------------------------------------------------------------------------
// Placing what the decider rendered
// ---------------------------------------------------------------------------

/// What [`place_rendered`] wrote, so the caller logs one event instead of
/// reconstructing it from the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedFacts {
    /// `docs/facts/*.txt` written — one per concept that resolved.
    pub fact_files: usize,
    /// Typed facts across every concept in the staged sidecar.
    pub facts: usize,
    pub concepts: usize,
    /// Filer XBRL tags the registry does not cover. Not an error — the
    /// coverage deliverable, and F5's growth chart.
    pub unmapped: usize,
    pub filer_tags_total: usize,
}

/// Write a [`RenderOutput`] into an acquired corpus root, mirroring
/// `scripts/setup-sec-corpus.sh`'s placement so the ticker path and the
/// script path produce the same corpus.
///
/// A render that resolved NO concept is an `Err`, never a prose-only
/// corpus written as if it succeeded (ARCH §18.3). The recipe declares
/// `[authority] tool = "sec_facts"`; installing a corpus that makes that
/// claim and carries no typed store would put the user in front of a
/// financial corpus that refuses every figure, with nothing saying why.
pub fn place_rendered(root: &Path, rendered: &RenderOutput) -> Result<PlacedFacts> {
    let facts_dir = root.join("docs").join("facts");
    let raw_dir = root.join("raw");
    std::fs::create_dir_all(&facts_dir)?;
    std::fs::create_dir_all(&raw_dir)?;

    let store = rendered.sidecar.as_ref().ok_or_else(|| {
        Error::Recipe(format!(
            "the concept map resolved NO figure from this filer's companyfacts \
             ({}/{} of its XBRL tags are unmapped). Nothing was installed: this \
             recipe declares `sec_facts` authoritative for its figures, and a \
             corpus carrying that claim with no typed fact store would refuse \
             every figure without saying why.",
            rendered.unmapped.unmapped.len(),
            rendered.unmapped.filer_tags_total
        ))
    })?;

    // Stale fact files from a previous acquisition — of a different
    // filing or a DIFFERENT COMPANY — would otherwise be ingested
    // alongside the new ones, and the prose pass does the same.
    for entry in std::fs::read_dir(&facts_dir)?.flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) == Some("txt") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    for (name, body) in &rendered.fact_files {
        std::fs::write(facts_dir.join(name), body)?;
    }

    // Staged, not installed: `install_fact_sidecar` places it in the
    // index dir once the corpus id is known.
    let sidecar = serde_json::to_vec_pretty(store)
        .map_err(|e| Error::Serialization(format!("typed fact store: {e}")))?;
    std::fs::write(raw_dir.join(FACT_SIDECAR), sidecar)?;

    let unmapped = serde_json::to_vec_pretty(&rendered.unmapped)
        .map_err(|e| Error::Serialization(format!("unmapped report: {e}")))?;
    std::fs::write(raw_dir.join("_unmapped_concepts.json"), unmapped)?;

    Ok(PlacedFacts {
        fact_files: rendered.fact_files.len(),
        facts: store.concepts.values().map(|c| c.facts.len()).sum(),
        concepts: store.concepts.len(),
        unmapped: rendered.unmapped.unmapped.len(),
        filer_tags_total: rendered.unmapped.filer_tags_total,
    })
}

// ---------------------------------------------------------------------------
// Registration + the networked acquire
// ---------------------------------------------------------------------------

/// Register the acquirer on `engine` under [`KIND`]. Call once at daemon
/// startup, before any ingest of a recipe naming it. Idempotent:
/// re-registering overwrites.
pub fn register(engine: &CorpusEngine) {
    let acquirer: CustomAcquirerFn = Arc::new(|params_blob, download_dir| {
        Box::pin(async move { acquire(params_blob, download_dir).await })
    });
    engine.register_acquirer(KIND, acquirer);
    tracing::debug!(target: "sec_edgar", kind = KIND, "sec_edgar: acquirer registered");
}

async fn get_bytes(client: &reqwest::Client, url: &str, ua: &str) -> Result<Vec<u8>> {
    let resp = client
        .get(url)
        .header(reqwest::header::USER_AGENT, ua)
        .send()
        .await?;
    let status = resp.status();
    if status == reqwest::StatusCode::FORBIDDEN {
        // The single most likely cause, named precisely rather than left
        // as a generic HTTP failure: SEC refuses a User-Agent that does
        // not carry a reachable contact.
        return Err(Error::Recipe(format!(
            "SEC refused the request (403) for {url}. Its fair-access policy requires a \
             User-Agent naming a REACHABLE CONTACT — a product name alone is refused. \
             This install sent `{ua}`. Supply your own with the recipe's `contact` \
             parameter. Nothing was installed."
        )));
    }
    if !status.is_success() {
        return Err(Error::Recipe(format!(
            "SEC returned {status} for {url} (User-Agent `{ua}`). SEC throttles bursts; \
             nothing was installed."
        )));
    }
    Ok(resp.bytes().await?.to_vec())
}

async fn get_json(client: &reqwest::Client, url: &str, ua: &str) -> Result<serde_json::Value> {
    let bytes = get_bytes(client, url, ua).await?;
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::Recipe(format!("{url} did not return JSON: {e}")))
}

async fn acquire(params_blob: serde_json::Value, download_dir: PathBuf) -> Result<PathBuf> {
    let params: SecEdgarAcquirerParams = serde_json::from_value(params_blob).map_err(|e| {
        Error::Recipe(format!(
            "sec_edgar params invalid: {e}. Expected at least `ticker`; a recipe \
             installs this acquirer with `params = {{ ticker = \"{{ticker}}\" }}`."
        ))
    })?;
    if params.ticker.trim().is_empty() || params.ticker.contains('{') {
        return Err(Error::Recipe(format!(
            "sec_edgar received ticker `{}` — an empty or un-interpolated value. The \
             install path must supply a ticker parameter; nothing was acquired.",
            params.ticker
        )));
    }
    let ua = params
        .user_agent
        .clone()
        .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string());
    let subject = parse_subject(&params.ticker);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| Error::Recipe(format!("sec_edgar http client: {e}")))?;

    // ── 1. subject -> CIK ────────────────────────────────────────────
    let (cik_bare, title) = match &subject {
        Subject::Cik(n) => (*n, format!("CIK {n}")),
        Subject::Ticker(t) => {
            let tickers = get_json(&client, TICKERS_URL, &ua).await?;
            resolve_ticker(&tickers, t).ok_or_else(|| {
                Error::Recipe(format!(
                    "ticker `{t}` is not in SEC's company_tickers.json. It is reported \
                     as absent, never resolved to a near neighbour — check the symbol \
                     or pass the CIK directly."
                ))
            })?
        }
    };
    let cik10 = format!("{cik_bare:010}");
    tracing::info!(target: "sec_edgar", input = %params.ticker, cik = %cik10,
        entity = %title, "sec_edgar: resolved subject to CIK");

    // ── 2. namespace + name any replacement ──────────────────────────
    // The corpus is single-instance (it installs under the recipe's own
    // `[corpus] id`), so installing a second company REPLACES the first.
    // That is ordinary reinstall semantics, but a silent swap would be
    // principle 6 in the install path: state it, naming both companies.
    let previous = read_resident(&download_dir);
    match classify_residency(previous.as_ref(), &cik10) {
        Residency::First => tracing::info!(target: "sec_edgar", cik = %cik10,
            entity = %title, "sec_edgar: first install — corpus will hold this company"),
        Residency::Refresh => tracing::info!(target: "sec_edgar", cik = %cik10,
            entity = %title, "sec_edgar: refreshing the resident company"),
        Residency::Replaces(prev) => tracing::warn!(target: "sec_edgar",
            replaced_cik = %prev.cik, replaced_entity = %prev.entity,
            cik = %cik10, entity = %title,
            "sec_edgar: REPLACING the resident company — this corpus held {} (CIK {}) \
             and will now hold {} (CIK {}); the previous company's figures leave the corpus",
            prev.entity, prev.cik, title, cik10),
    }

    let root = download_dir.join(subject.download_slug());
    let raw_dir = root.join("raw");
    let docs_dir = root.join("docs");
    let prose_dir = docs_dir.join("prose");
    std::fs::create_dir_all(&raw_dir)?;
    std::fs::create_dir_all(&prose_dir)?;

    // ── 3. discover 10-Ks; select ONE, NAME every skip ───────────────
    let subs = get_json(&client, &submissions_url(&cik10), &ua).await?;
    let hits = in_window_10ks(
        &subs,
        &params.from_date,
        &params.to_date,
        params.accession.as_deref(),
    );
    let window = match &params.accession {
        Some(acc) => format!("with accession {acc} for CIK {cik10}"),
        None => format!(
            "for CIK {cik10} filed in [{} .. {}]",
            params.from_date, params.to_date
        ),
    };
    let selection = select_filing(hits, &window)?;
    tracing::info!(target: "sec_edgar", cik = %cik10,
        accession = %selection.selected.accession,
        filed = %selection.selected.filing_date,
        primary = %selection.selected.primary_document,
        in_window = selection.skipped.len() + 1,
        "sec_edgar: selected 10-K (latest in window)");
    for skipped in &selection.skipped {
        tracing::info!(target: "sec_edgar", cik = %cik10,
            accession = %skipped.accession, filed = %skipped.filing_date,
            reason = "not latest in window",
            "sec_edgar: SKIPPED 10-K");
    }

    // ── 4. fetch + shape the primary document ────────────────────────
    let url = document_url(cik_bare, &selection.selected);
    tracing::debug!(target: "sec_edgar", %url, "sec_edgar: fetching primary document");
    let raw = get_bytes(&client, &url, &ua).await?;
    let acc_nodash = selection.selected.accession_nodash();
    std::fs::write(raw_dir.join(format!("{acc_nodash}.html")), &raw)?;
    let html = String::from_utf8_lossy(&raw);
    let text = normalize_sec_text(&crate::corpus::strip_html(&html));
    let parts = split_parts(&text, PROSE_TARGET_CHARS, PROSE_OVERLAP_CHARS);
    if parts.is_empty() {
        return Err(Error::Recipe(format!(
            "10-K {} downloaded {} bytes but cleaning produced no text — refusing to \
             install an empty corpus.",
            selection.selected.accession,
            raw.len()
        )));
    }
    // Stale parts from a previous acquisition of a DIFFERENT filing would
    // otherwise be ingested alongside the new one.
    for entry in std::fs::read_dir(&prose_dir)?.flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) == Some("txt") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    for (i, part) in parts.iter().enumerate() {
        std::fs::write(
            prose_dir.join(format!("{acc_nodash}-{:03}.txt", i + 1)),
            part,
        )?;
    }
    tracing::info!(target: "sec_edgar", cik = %cik10, raw_bytes = raw.len(),
        chars = text.len(), parts = parts.len(),
        "sec_edgar: cleaned primary document into prose parts");

    // ── 5. companyfacts, saved RAW ───────────────────────────────────
    // Saved uninterpreted: this acquirer never decides what a figure is.
    // Step 6 hands these bytes to THE one decider (ARCH §10.6).
    let facts_bytes = get_bytes(&client, &companyfacts_url(&cik10), &ua).await?;
    let facts_path = raw_dir.join("companyfacts.json");
    let parsed: serde_json::Value = serde_json::from_slice(&facts_bytes)
        .map_err(|e| Error::Recipe(format!("companyfacts for CIK{cik10} is not JSON: {e}")))?;
    if parsed.get("facts").is_none() {
        return Err(Error::Recipe(format!(
            "companyfacts response for CIK{cik10} carries no `facts` object — the \
             XBRL figures are absent and are reported as such, not defaulted."
        )));
    }
    std::fs::write(&facts_path, &facts_bytes)?;
    tracing::info!(target: "sec_edgar", cik = %cik10, bytes = facts_bytes.len(),
        path = %facts_path.display(),
        "sec_edgar: saved raw companyfacts (not interpreted — the decider owns that)");

    // ── 6. the decider renders; this acquirer only places the files ──
    // `fiscal_years: None` = the latest 3 available per concept, which is
    // what every corpus the F1-F6 bars were measured against was rendered
    // with. Pinning a set here would silently change what an installed
    // corpus CONTAINS relative to those measurements.
    let cmap = crate::sec_facts_render::ConceptMap::from_toml(CONCEPT_MAP_TOML)
        .map_err(|e| Error::Recipe(format!("bundled concept map is unreadable: {e}")))?;
    let rendered = crate::sec_facts_render::render(crate::sec_facts_render::RenderRequest {
        companyfacts: &parsed,
        concept_map: &cmap,
        ticker: Some(&params.ticker),
        fiscal_years: None,
    })
    .map_err(|e| Error::Recipe(format!("rendering CIK{cik10}'s figures failed: {e}")))?;
    let placed = place_rendered(&root, &rendered)?;
    tracing::info!(target: "sec_edgar", cik = %cik10,
        fact_files = placed.fact_files, facts = placed.facts,
        concepts = placed.concepts, unmapped = placed.unmapped,
        filer_tags = placed.filer_tags_total,
        "sec_edgar: rendered typed figures — fact files staged for ingest, \
         sidecar staged for the index dir");

    // Claimed only now that the acquisition SUCCEEDED — a failed install
    // must not leave a record saying this company is resident.
    write_resident(
        &download_dir,
        &Resident {
            cik: cik10.clone(),
            entity: title.clone(),
            subject: params.ticker.clone(),
            accession: selection.selected.accession.clone(),
            filed: selection.selected.filing_date.clone(),
        },
    )?;

    Ok(docs_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn submissions_fixture() -> serde_json::Value {
        // Parallel arrays, exactly as data.sec.gov shapes them — the
        // reason a JSONPath acquirer cannot reach the document.
        json!({
            "filings": { "recent": {
                "form":            ["10-Q",       "10-K",       "8-K",        "10-K"],
                "accessionMumbo":  [],
                "accessionNumber": ["0000-24-001","0000-24-002","0000-25-003","0000-25-004"],
                "primaryDocument": ["q.htm",      "aapl-24.htm","e.htm",      "aapl-25.htm"],
                "filingDate":      ["2024-02-01", "2024-11-01", "2025-01-15", "2025-11-01"]
            }}
        })
    }

    #[test]
    fn parse_subject_reads_a_bare_cik_as_a_cik_and_a_symbol_as_a_ticker() {
        assert_eq!(parse_subject("aapl"), Subject::Ticker("AAPL".into()));
        assert_eq!(parse_subject(" MSFT "), Subject::Ticker("MSFT".into()));
        assert_eq!(parse_subject("320193"), Subject::Cik(320193));
        // Zero-padded must be the SAME company as unpadded, not octal.
        assert_eq!(parse_subject("0000320193"), Subject::Cik(320193));
    }

    #[test]
    fn download_slug_namespaces_per_subject_so_two_companies_cannot_collide() {
        // `download_dir` is shared across every corpus (engine/ingest.rs:615)
        // and the acquirer is handed no corpus id.
        assert_eq!(parse_subject("AAPL").download_slug(), "sec-aapl");
        assert_eq!(parse_subject("MSFT").download_slug(), "sec-msft");
        assert_eq!(parse_subject("320193").download_slug(), "sec-cik0000320193");
        assert_ne!(
            parse_subject("AAPL").download_slug(),
            parse_subject("MSFT").download_slug()
        );
    }

    #[test]
    fn resolve_ticker_matches_case_insensitively_and_reports_absence() {
        let tickers = json!({
            "0": {"cik_str": 320193, "ticker": "AAPL", "title": "Apple Inc."},
            "1": {"cik_str": 789019, "ticker": "MSFT", "title": "MICROSOFT CORP"}
        });
        assert_eq!(
            resolve_ticker(&tickers, "aapl"),
            Some((320193, "Apple Inc.".to_string()))
        );
        // Absence is reported, never resolved to a near neighbour.
        assert_eq!(resolve_ticker(&tickers, "AAPLE"), None);
        assert_eq!(resolve_ticker(&tickers, "AAP"), None);
    }

    #[test]
    fn in_window_10ks_reads_parallel_arrays_and_filters_by_form_and_date() {
        let hits = in_window_10ks(&submissions_fixture(), "2024-01-01", "2026-12-31", None);
        assert_eq!(hits.len(), 2, "only the two 10-Ks, not the 10-Q or the 8-K");
        assert_eq!(hits[0].accession, "0000-24-002");
        assert_eq!(hits[0].primary_document, "aapl-24.htm");
        assert_eq!(hits[1].accession, "0000-25-004");
        // Narrowing the window drops the earlier one.
        let narrow = in_window_10ks(&submissions_fixture(), "2025-01-01", "2026-12-31", None);
        assert_eq!(narrow.len(), 1);
        assert_eq!(narrow[0].accession, "0000-25-004");
    }

    #[test]
    fn in_window_10ks_honours_a_pinned_accession_over_the_date_window() {
        // Pinned accession is OUTSIDE the given window: the pin wins.
        let hits = in_window_10ks(
            &submissions_fixture(),
            "2026-01-01",
            "2026-12-31",
            Some("0000-24-002"),
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].accession, "0000-24-002");
    }

    #[test]
    fn select_filing_takes_the_latest_and_names_every_skip() {
        let hits = in_window_10ks(&submissions_fixture(), "2024-01-01", "2026-12-31", None);
        let sel = select_filing(hits, "for CIK 0000320193").expect("two hits available");
        assert_eq!(sel.selected.accession, "0000-25-004");
        assert_eq!(
            sel.skipped.len(),
            1,
            "the older 10-K must be NAMED, not dropped"
        );
        assert_eq!(sel.skipped[0].accession, "0000-24-002");
    }

    #[test]
    fn select_filing_refuses_an_empty_window_instead_of_installing_nothing() {
        let err = select_filing(
            Vec::new(),
            "for CIK 0000320193 filed in [2030-01-01 .. 2030-12-31]",
        )
        .expect_err("an empty window is a refusal, not an empty corpus");
        let msg = err.to_string();
        assert!(msg.contains("no 10-K found"), "{msg}");
        assert!(
            msg.contains("2030-01-01"),
            "the refusal must state the window: {msg}"
        );
        assert!(msg.contains("Nothing was installed"), "{msg}");
    }

    #[test]
    fn document_url_composes_three_parallel_array_fields() {
        // This composition is exactly what http_api's document_url_path
        // (a JSONPath selecting a URL string) cannot express.
        let hit = FilingHit {
            accession: "0000320193-25-000073".into(),
            primary_document: "aapl-20250927.htm".into(),
            filing_date: "2025-11-01".into(),
        };
        assert_eq!(
            document_url(320193, &hit),
            "https://www.sec.gov/Archives/edgar/data/320193/000032019325000073/aapl-20250927.htm"
        );
    }

    #[test]
    fn companyfacts_and_submissions_urls_use_the_zero_padded_cik() {
        assert_eq!(
            submissions_url("0000320193"),
            "https://data.sec.gov/submissions/CIK0000320193.json"
        );
        assert_eq!(
            companyfacts_url("0000320193"),
            "https://data.sec.gov/api/xbrl/companyfacts/CIK0000320193.json"
        );
    }

    #[test]
    fn normalize_sec_text_drops_inline_xbrl_runs_and_folds_punctuation() {
        let garbage = "x".repeat(GARBAGE_RUN_CHARS);
        let short = "y".repeat(GARBAGE_RUN_CHARS - 1);
        let input = format!("Net\u{00a0}sales \u{201c}rose\u{201d} {garbage} {short} 10\u{2013}K");
        let out = normalize_sec_text(&input);
        assert!(
            !out.contains(&garbage),
            "40-char run must be dropped: {out}"
        );
        assert!(out.contains(&short), "39-char run must survive: {out}");
        assert_eq!(out, format!("Net sales \"rose\" {short} 10-K"));
    }

    #[test]
    fn normalize_sec_text_collapses_all_whitespace_to_single_spaces() {
        let out = normalize_sec_text("a\n\n  b\t\tc \u{200b}d");
        assert_eq!(out, "a b c d");
    }

    #[test]
    fn split_parts_bounds_each_part_and_repeats_an_overlap() {
        let text = (1..=200)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let parts = split_parts(&text, 100, 20);
        assert!(parts.len() > 1, "long text must split");
        for p in &parts {
            assert!(p.len() <= 100, "part over target: {} chars", p.len());
        }
        // Overlap: the tail of part N reappears at the head of part N+1.
        let first_tail = parts[0].split(' ').next_back().unwrap();
        assert!(
            parts[1].split(' ').any(|w| w == first_tail),
            "part 2 must repeat part 1's boundary region"
        );
    }

    #[test]
    fn split_parts_keeps_short_text_whole() {
        let parts = split_parts("a short filing", 100, 20);
        assert_eq!(parts, vec!["a short filing".to_string()]);
    }

    fn apple() -> Resident {
        Resident {
            cik: "0000320193".into(),
            entity: "Apple Inc.".into(),
            subject: "AAPL".into(),
            accession: "0000320193-25-000073".into(),
            filed: "2025-11-01".into(),
        }
    }

    #[test]
    fn classify_residency_distinguishes_first_refresh_and_replacement() {
        assert_eq!(classify_residency(None, "0000320193"), Residency::First);
        assert_eq!(
            classify_residency(Some(&apple()), "0000320193"),
            Residency::Refresh,
            "the same CIK is a refresh, not a replacement"
        );
        // The named failing input: install AAPL, then install MSFT.
        match classify_residency(Some(&apple()), "0000789019") {
            Residency::Replaces(prev) => {
                assert_eq!(prev.cik, "0000320193");
                assert_eq!(prev.entity, "Apple Inc.");
            }
            other => panic!("a different CIK must be a NAMED replacement, got {other:?}"),
        }
    }

    #[test]
    fn resident_record_round_trips_so_a_later_surface_reads_what_install_decided() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Nothing installed yet.
        assert_eq!(read_resident(dir.path()), None);
        write_resident(dir.path(), &apple()).expect("write resident");
        assert_eq!(read_resident(dir.path()), Some(apple()));
    }

    #[test]
    fn a_corrupt_resident_record_reads_as_nothing_installed_rather_than_wedging() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(RESIDENT_FILE), b"{ not json").unwrap();
        assert_eq!(read_resident(dir.path()), None);
        assert_eq!(classify_residency(None, "0000320193"), Residency::First);
    }

    #[test]
    fn install_fact_sidecar_is_a_no_op_for_a_corpus_with_no_resident_company() {
        // Every non-SEC install passes through the shared post-install
        // path and must not be disturbed by this hook.
        let dir = tempfile::tempdir().expect("tempdir");
        let placed = install_fact_sidecar("wikipedia-core", dir.path())
            .expect("a corpus with no resident record is not an error");
        assert_eq!(placed, None);
    }

    #[test]
    fn install_fact_sidecar_places_the_staged_store_where_the_tool_looks() {
        let indexes = tempfile::tempdir().expect("tempdir");
        let downloads = indexes.path().join("_downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        write_resident(&downloads, &apple()).unwrap();
        let staged = downloads.join("sec-aapl").join("raw");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join(FACT_SIDECAR), br#"{"schema":1}"#).unwrap();

        let placed = install_fact_sidecar("sec-filings-company", indexes.path())
            .expect("a staged store must be placeable")
            .expect("a staged store must be placed");
        // The path `SecFactsTool::resolve_corpus` reads.
        assert_eq!(
            placed,
            indexes
                .path()
                .join("sec-filings-company")
                .join("sec_facts.json")
        );
        assert!(
            placed.exists(),
            "the sidecar must exist where the tool looks"
        );
    }

    #[test]
    fn install_fact_sidecar_reports_a_resident_company_with_no_rendered_store() {
        // Named absence: the company is resident but nothing rendered a
        // store, so figures will refuse. That is reported, not collapsed
        // into a success-shaped placement.
        let indexes = tempfile::tempdir().expect("tempdir");
        let downloads = indexes.path().join("_downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        write_resident(&downloads, &apple()).unwrap();
        let placed =
            install_fact_sidecar("sec-filings-company", indexes.path()).expect("not an error");
        assert_eq!(placed, None);
        assert!(
            !indexes
                .path()
                .join("sec-filings-company")
                .join(FACT_SIDECAR)
                .exists(),
            "nothing may be written when nothing was staged"
        );
    }

    // ── the rendered figures reach the corpus ───────────────────────────

    /// The one place the bundled snapshot and the canonical registry are
    /// compared. Structural, not remembered (principle 10): edit
    /// `sovereign-recipes/sec-filings-company/concept-map.toml` and this
    /// fails at the next build unless the snapshot rebuilt with it.
    #[test]
    fn the_bundled_concept_map_is_byte_identical_to_the_canonical_registry() {
        let canonical = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../sovereign-recipes/sec-filings-company/concept-map.toml");
        let on_disk = std::fs::read_to_string(&canonical).expect("the canonical map is committed");
        assert_eq!(
            CONCEPT_MAP_TOML,
            on_disk,
            "the compiled-in map drifted from {}",
            canonical.display()
        );
    }

    fn aapl_companyfacts() -> serde_json::Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sec_facts/aapl-companyfacts.json");
        serde_json::from_str(&std::fs::read_to_string(&path).expect("fixture is committed"))
            .expect("fixture parses")
    }

    fn render_aapl() -> RenderOutput {
        // Through the BUNDLED map, so this exercises the constant the
        // product actually installs with — not a second copy read from
        // disk that could be fine while the binary's is not.
        let cmap = crate::sec_facts_render::ConceptMap::from_toml(CONCEPT_MAP_TOML)
            .expect("the bundled map parses");
        crate::sec_facts_render::render(crate::sec_facts_render::RenderRequest {
            companyfacts: &aapl_companyfacts(),
            concept_map: &cmap,
            ticker: Some("AAPL"),
            fiscal_years: None,
        })
        .expect("the fixture renders")
    }

    #[test]
    fn place_rendered_puts_fact_files_where_ingest_reads_and_the_sidecar_where_install_stages() {
        let root = tempfile::tempdir().expect("tempdir");
        let rendered = render_aapl();
        let placed = place_rendered(root.path(), &rendered).expect("placement succeeds");

        assert!(placed.fact_files > 0, "the fixture resolves concepts");
        assert!(placed.facts > 0, "and typed facts");

        // 1. Ingested documents: under docs/, which is what `acquire`
        //    returns to the engine.
        let facts_dir = root.path().join("docs").join("facts");
        let txt: Vec<_> = std::fs::read_dir(&facts_dir)
            .expect("docs/facts exists")
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("txt"))
            .collect();
        assert_eq!(
            txt.len(),
            placed.fact_files,
            "every rendered fact file must be on disk before ingest"
        );
        let revenue = facts_dir.join("facts-revenue.txt");
        let body = std::fs::read_to_string(&revenue).expect("revenue rendered for AAPL");
        assert!(
            body.contains("accession"),
            "a fact line must carry its citation: {body}"
        );

        // 2. The sidecar is STAGED where install_fact_sidecar looks, and
        //    is NOT in docs/ where the plaintext extractor would ingest it.
        let staged = root.path().join("raw").join(FACT_SIDECAR);
        assert!(staged.is_file(), "sidecar staged at raw/{FACT_SIDECAR}");
        assert!(
            !facts_dir.join(FACT_SIDECAR).exists(),
            "the sidecar must not be an ingested document"
        );
        assert!(
            root.path().join("raw").join("_unmapped_concepts.json").is_file(),
            "the coverage deliverable lands in raw/, not docs/"
        );

        // 3. What was staged is what the tool will read back.
        let store: corpus_engine::enrichment::atlas::analysis::sec_facts::SecFactStore =
            serde_json::from_str(&std::fs::read_to_string(&staged).unwrap())
                .expect("the staged sidecar parses as the type the tool reads");
        assert_eq!(store.concepts.len(), placed.concepts);
    }

    #[test]
    fn place_rendered_clears_a_previous_companys_fact_files() {
        // The named failing input: install AAPL, then install MSFT into
        // the same single-instance corpus. A stale facts-*.txt from the
        // first company would be ingested alongside the second's.
        let root = tempfile::tempdir().expect("tempdir");
        let facts_dir = root.path().join("docs").join("facts");
        std::fs::create_dir_all(&facts_dir).unwrap();
        std::fs::write(facts_dir.join("facts-ghost_concept.txt"), "MSFT revenue").unwrap();

        place_rendered(root.path(), &render_aapl()).expect("placement succeeds");
        assert!(
            !facts_dir.join("facts-ghost_concept.txt").exists(),
            "a previous company's fact file must not survive into this corpus"
        );
    }

    #[test]
    fn place_rendered_refuses_a_render_that_resolved_nothing() {
        // Absence is reported, never a prose-only corpus written as if it
        // succeeded — the recipe declares sec_facts authoritative.
        let root = tempfile::tempdir().expect("tempdir");
        let empty = RenderOutput {
            fact_files: Vec::new(),
            sidecar: None,
            unmapped: crate::sec_facts_render::UnmappedReport {
                cik: "0000320193".into(),
                entity: "Apple Inc.".into(),
                filer_tags_total: 7,
                covered_by_map: Vec::new(),
                unmapped: vec!["SomeTag".into()],
            },
        };
        let err = place_rendered(root.path(), &empty)
            .expect_err("no resolved figure is a refusal, not an empty corpus");
        let msg = err.to_string();
        assert!(msg.contains("Nothing was installed"), "{msg}");
        assert!(msg.contains("sec_facts"), "{msg}");
        assert!(
            !root.path().join("raw").join(FACT_SIDECAR).exists(),
            "nothing may be staged when nothing resolved"
        );
    }

    #[tokio::test]
    async fn acquire_refuses_an_uninterpolated_ticker_placeholder() {
        // The failure mode this guards: a recipe ships `{ticker}` but the
        // install path does not thread parameters, so the acquirer would
        // otherwise look up a company literally named "{ticker}".
        let dir = tempfile::tempdir().expect("tempdir");
        let err = acquire(json!({"ticker": "{ticker}"}), dir.path().to_path_buf())
            .await
            .expect_err("an un-interpolated placeholder must refuse before any network call");
        assert!(err.to_string().contains("un-interpolated"), "{err}");
    }

    #[tokio::test]
    async fn acquire_refuses_params_without_a_ticker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = acquire(json!({"from_date": "2024-01-01"}), dir.path().to_path_buf())
            .await
            .expect_err("ticker is required");
        assert!(
            err.to_string().contains("sec_edgar params invalid"),
            "{err}"
        );
    }

    #[test]
    fn registering_makes_the_kind_resolvable_by_the_engine() {
        let dir = tempfile::tempdir().expect("tempdir");
        let embed: corpus_engine::types::EmbedFn =
            Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0f32; 8]) }));
        let engine = CorpusEngine::new(dir.path().join("recipes"), dir.path().join("idx"), embed);
        register(&engine);
        // The dispatch arm in ingest_factories.rs errors with
        // "No custom acquirer registered for kind 'sec_edgar'" when this
        // registration is missing; a recipe naming KIND now resolves.
        assert_eq!(KIND, "sec_edgar");
    }
}
