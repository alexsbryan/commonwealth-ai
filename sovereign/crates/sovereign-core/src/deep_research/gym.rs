// SPDX-License-Identifier: AGPL-3.0-or-later
//! The gym deck — the F-table injected, on-disk, search-gym + chaos-monkey
//! precedents composed (spec "Sim before flight").
//!
//! `MockBackendImpl` implements the loop's `ResearchPort` with the search
//! and fetch legs resolved from an on-disk deck: `deck.toml` declares the
//! web hits, the fetch failures, and the estate corpora; body files carry
//! the fetched content. The deck controls the whole
//! environment — a drill run never touches the operator's real estate or
//! the network. Missing body files and unknown URLs are LOUD errors (the
//! search-gym precedent), never silent empties.
//!
//! The search leg is TERM-RANKED (order deep-research-t1f, T1.9): a term
//! index is built at deck load over each hit's full declared surface
//! (match tokens + title + snippet + body), and a query retrieves the
//! hits whose terms it overlaps, ranked by overlap — real search's
//! shape, so a query for a CONCEPT retrieves the document carrying the
//! VALUE without the loop ever naming it. The deck's curated match
//! tokens remain part of the indexed surface (the deck author's
//! intent), not a filter the query must pass.
//!
//! The draft and terminal surfaces are NOT decked: `MockDraftSurface`
//! either scripts the draft (deterministic gym tests — a test double for
//! the PROVIDER, never a fork of the draft prompt) or delegates to the
//! real port (CLI mock runs — the drill drafts through the shipped
//! constrained surface, test what you fly).
//!
//! The F-table is the typed representation of the spec's FMEA table: one
//! row per failure mode, with the detection point and the rehearsed
//! response. Every row is either **watched** (a fixture in this module's
//! tests exercises its detection) or **named** (the table carries the
//! reason it is not watched) — a row whose detection never fires is
//! named, not silent.

use super::estate::{
    estate_snippet, read_staged_alignment, AlignmentDecision, EstateListing, PortHit, ResearchPort,
    FRONTIER_MAX,
};
use super::icd::CorpusEntry;
use super::icd::Plan;
use crate::traits::InferenceProvider;
use crate::types::Custody;
use corpus_engine::CorpusIndex;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// The deck version this module reads. A deck with another version is
/// refused at load — loud, never silently re-interpreted.
pub const DECK_VERSION: u32 = 1;

/// One web hit as the deck declares it.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeckHit {
    /// Optional stable id; defaults to `h{index}`.
    #[serde(default)]
    pub id: Option<String>,
    /// The deck author's intent words for the hit. Since T1.9 they are
    /// part of the hit's INDEXED SURFACE (with title + snippet + body —
    /// the term index), never a filter the query must pass: the hit
    /// matches by term overlap over the whole surface, ranked by
    /// relevance. Kept for deck authors who want intent words a thin
    /// body would not carry (and the F-table fixtures).
    #[serde(rename = "match")]
    pub match_tokens: Vec<String>,
    pub url: String,
    pub title: String,
    pub snippet: String,
    /// The body file, relative to the deck dir (or a body key in
    /// `Deck::parse`).
    pub body: String,
    #[serde(default = "default_score")]
    pub score: f64,
    #[serde(default = "default_custody")]
    pub custody: String,
    /// The F-table row this fixture exercises (glassbox; the mock logs
    /// when a row fires).
    #[serde(default)]
    pub f_row: Option<String>,
}

fn default_score() -> f64 {
    0.9
}

fn default_custody() -> String {
    Custody::PublicWeb.as_str().to_string()
}

/// A fetch failure: `web_fetch(url)` refuses with the deck's reason
/// (F2's shape).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeckFail {
    pub url: String,
    pub reason: String,
    #[serde(default)]
    pub f_row: Option<String>,
}

/// An estate corpus as the deck declares it (F13/F16's shape: a
/// listed-but-unsearchable corpus must refuse the web leg, never read
/// as "no evidence").
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeckCorpus {
    pub corpus_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub chunks_count: i64,
    #[serde(default = "default_searchable")]
    pub searchable: bool,
    #[serde(default = "default_custody")]
    pub custody: String,
}

fn default_searchable() -> bool {
    true
}

/// The resolved deck: the parsed declarations plus every body in memory.
#[derive(Debug, Clone)]
pub struct Deck {
    pub corpora: Vec<DeckCorpus>,
    pub hits: Vec<DeckHit>,
    /// body file key → body content (loaded at construction, missing =
    /// loud).
    pub bodies: HashMap<String, String>,
    /// url → body content — what `web_fetch` serves. Built in parse so
    /// the fetch path never looks a body up by the wrong key (the
    /// url-keyed index is the ONLY fetch surface; the file-keyed map is
    /// for loaders and tests).
    pub url_bodies: HashMap<String, String>,
    /// url → the failure record. A url may be BOTH a hit and a fail —
    /// that is F2's own shape (search returns the page, the fetch 404s) —
    /// and the fail wins at fetch.
    pub fails: HashMap<String, DeckFail>,
    /// The term index (T1.9): term → hit indices, built at parse over
    /// each hit's full declared surface (match tokens + title + snippet
    /// + body). The ONE retrieval surface — a search is a term lookup
    /// over this index, ranked by overlap. Vectors are in ascending hit
    /// order by construction (indices pushed as hits are parsed), so
    /// membership is a binary search.
    pub term_index: HashMap<String, Vec<usize>>,
}

impl Deck {
    /// Parse a deck from its TOML text plus a body map (deterministic
    /// tests, no disk). A hit whose body key is absent, a url in both
    /// the hit and the fail sets, or a non-v1 deck refuses loudly.
    pub fn parse(toml_text: &str, bodies: &[(&str, &str)]) -> Result<Deck, String> {
        let raw: RawDeck =
            toml::from_str(toml_text).map_err(|e| format!("deck.toml parse: {e}"))?;
        if raw.version != DECK_VERSION {
            return Err(format!(
                "deck version {} unsupported (this gym reads version {DECK_VERSION})",
                raw.version
            ));
        }
        let body_map: HashMap<String, String> = bodies
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let mut seen_urls: HashMap<String, String> = HashMap::new();
        let mut resolved = Vec::new();
        let mut url_bodies: HashMap<String, String> = HashMap::new();
        let mut term_index: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, hit) in raw.hits.iter().enumerate() {
            if let Some(prev) = seen_urls.get(&hit.url) {
                return Err(format!(
                    "deck hit url {} duplicates {prev} — a hit url must be unique in the deck",
                    hit.url
                ));
            }
            seen_urls.insert(hit.url.clone(), format!("hit {}", i + 1));
            let body = body_map.get(&hit.body).ok_or_else(|| {
                format!(
                    "deck hit {} body file {:#?} missing — a missing body is a loud error, never a silent empty",
                    hit.url, hit.body
                )
            })?;
            if body.is_empty() {
                return Err(format!(
                    "deck hit {} body file {:#?} is empty — an empty fetched page is a deck bug",
                    hit.url, hit.body
                ));
            }
            if Custody::parse_wire(&hit.custody).is_none() {
                return Err(format!(
                    "deck hit {} custody {:#?} unparseable (public-web | personal | peer | unknown)",
                    hit.url, hit.custody
                ));
            }
            resolved.push(DeckHit {
                id: hit.id.clone().or_else(|| Some(format!("h{}", i + 1))),
                match_tokens: hit.match_tokens.clone(),
                url: hit.url.clone(),
                title: hit.title.clone(),
                snippet: hit.snippet.clone(),
                body: hit.body.clone(),
                score: hit.score,
                custody: hit.custody.clone(),
                f_row: hit.f_row.clone(),
            });
            // The fetch index: url → content. Only the url keyed index
            // serves fetches — the body-keyed map is for loaders/tests.
            url_bodies.insert(hit.url.clone(), body.clone());
            // The term index (T1.9): the hit's full declared surface,
            // tokenized by the one tokenizer. Indices pushed in
            // ascending hit order — membership is a binary search.
            let surface = format!(
                "{} {} {} {}",
                hit.match_tokens.join(" "),
                hit.title,
                hit.snippet,
                body
            );
            for t in terms(&surface) {
                term_index.entry(t).or_default().push(i);
            }
        }
        let mut fails = HashMap::new();
        let mut fail_urls: std::collections::HashSet<String> = std::collections::HashSet::new();
        for f in &raw.fails {
            if !fail_urls.insert(f.url.clone()) {
                return Err(format!(
                    "deck declares the fail url {} twice — a url's failure reason is unique",
                    f.url
                ));
            }
            fails.insert(f.url.clone(), f.clone());
        }
        Ok(Deck {
            corpora: raw.corpora.clone(),
            hits: resolved,
            bodies: body_map,
            url_bodies,
            fails,
            term_index,
        })
    }

    /// Load a deck from a directory: `deck.toml` plus every body file
    /// the hits name. Missing or unreadable files refuse (the
    /// search-gym precedent).
    pub fn load(dir: &Path) -> Result<Deck, String> {
        let toml_path = dir.join("deck.toml");
        let toml_text =
            std::fs::read_to_string(&toml_path).map_err(|e| format!("read {toml_path:?}: {e}"))?;
        let raw: RawDeck =
            toml::from_str(&toml_text).map_err(|e| format!("{toml_path:?} parse: {e}"))?;
        if raw.version != DECK_VERSION {
            return Err(format!(
                "deck version {} unsupported (this gym reads version {DECK_VERSION})",
                raw.version
            ));
        }
        let mut owned: Vec<String> = Vec::new();
        for hit in &raw.hits {
            let body = std::fs::read_to_string(dir.join(&hit.body))
                .map_err(|e| format!("deck body {:#?} missing or unreadable: {e}", hit.body))?;
            owned.push(body);
        }
        let bodies: Vec<(&str, &str)> = raw
            .hits
            .iter()
            .zip(owned.iter())
            .map(|(hit, body)| (hit.body.as_str(), body.as_str()))
            .collect();
        Self::parse(&toml_text, &bodies)
    }

    /// A hit by url, if any.
    pub fn hit(&self, url: &str) -> Option<&DeckHit> {
        self.hits.iter().find(|h| h.url == url)
    }

    /// Term overlap (T1.9 — the ONE retrieval decider, shared by the
    /// mock's search leg and the coverage assertions): hit `hit` (its
    /// deck position) matches a query iff at least one query term is in
    /// the hit's term set — the term index over its full declared
    /// surface (match tokens + title + snippet + body). Case-
    /// insensitive by construction: the one tokenizer lowercases both
    /// sides. The exact-value matcher this replaced (OR substring over
    /// the curated match tokens) could not retrieve a document unless
    /// the query already named one of its tokens — a query for a
    /// CONCEPT never retrieved the document carrying the VALUE.
    pub fn query_matches(&self, hit: usize, query: &str) -> bool {
        self.relevance(hit, query) > 0
    }

    /// Term relevance (T1.9 — the ranking decider): the number of
    /// distinct query terms present in the hit's term set.
    pub fn relevance(&self, hit: usize, query: &str) -> usize {
        let mut n = 0;
        for t in terms(query) {
            if let Some(hits) = self.term_index.get(&t) {
                if hits.binary_search(&hit).is_ok() {
                    n += 1;
                }
            }
        }
        n
    }
}

/// The one tokenizer (T1.9): lowercase, split on non-alphanumeric,
/// empty tokens dropped, deduped in first-appearance order. Applied
/// identically to queries and to the deck's indexed surface — one
/// decider for both sides. A decimal figure splits at the point
/// ("0.5469" → "0", "5469") — the same split a punctuation-splitting
/// analyzer makes.
fn terms(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in text
        .to_ascii_lowercase()
        .split(|c: char| !c.is_alphanumeric())
    {
        if t.is_empty() {
            continue;
        }
        if !out.iter().any(|o| o == t) {
            out.push(t.to_string());
        }
    }
    out
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeck {
    version: u32,
    /// The deck spells the tables singular (`[[corpus]]`, `[[hit]]`,
    /// `[[fail]]`) — one table per row, plural on the read side. The
    /// renames are load-bearing: without them a singular-keyed deck
    /// parses as SILENTLY EMPTY (defaults), which is exactly the trap
    /// the deck exists to catch. `deny_unknown_fields` refuses a
    /// plural-keyed or typo'd deck loudly.
    #[serde(default, rename = "corpus")]
    corpora: Vec<DeckCorpus>,
    #[serde(default, rename = "hit")]
    hits: Vec<DeckHit>,
    #[serde(default, rename = "fail")]
    fails: Vec<DeckFail>,
}

/// Where the mock's draft + terminal surfaces come from: scripted
/// (deterministic gym tests) or the real port (CLI drill runs — the
/// draft goes through the shipped constrained surface, never a fork of
/// the prompt).
#[derive(Clone)]
pub enum MockDraftSurface {
    /// Canned draft text, returned verbatim for every round.
    Scripted(String),
    /// The real port's constrained draft + terminal poll.
    Delegated(Arc<dyn ResearchPort>),
}

impl std::fmt::Debug for MockDraftSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MockDraftSurface::Scripted(t) => write!(f, "Scripted({t:?})"),
            MockDraftSurface::Delegated(_) => write!(f, "Delegated(real port)"),
        }
    }
}

/// How a corpus surface embeds a query. The CLI wires the daemon's
/// provider slot (the embed model pin); unit tests wire a deterministic
/// fake — the corpus-engine tests' precedent (sharding_round_trip_e2e
/// builds real LanceDB corpora with seeded embeddings and searches
/// them). One implementation per embedder — the surface never reaches
/// into a provider directly.
#[async_trait::async_trait]
pub trait CorpusEmbed: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, String>;
}

/// The daemon-provider embedder for the mock's corpus surface (the
/// CLI mock runs: `--backend mock --search-source corpus`).
pub struct ProviderEmbed(pub Arc<dyn InferenceProvider>);

#[async_trait::async_trait]
impl CorpusEmbed for ProviderEmbed {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        self.0
            .embed(text)
            .await
            .map_err(|e| format!("embed `{text}`: {e}"))
    }
}

/// The mock's corpus surface (t1g rung 2): real LanceDB indexes plus
/// the embedder that queries them. When present, the mock's
/// `estate_search` runs REAL corpus search (the estate's
/// corpus-search surface — `CorpusIndex::search`, vector + FTS hybrid)
/// and `web_fetch` resolves the `estate:<corpus_id>:<chunk_id>` scheme
/// from the chunk store. When absent, the mock's estate leg answers
/// the decked empty (the F13/F16 listing-only shape). One index per
/// corpus id the surface serves; a corpus id the surface does NOT
/// serve refuses loudly — never a silent empty (the closed-set rule).
pub struct CorpusSurface {
    pub indexes: Vec<CorpusIndex>,
    pub embed: Box<dyn CorpusEmbed>,
}

/// The mock search/fetch backend: every loop surface except the draft
/// and terminal poll resolves from the deck. The estate leg resolves
/// from the OPTIONAL corpus surface (real corpus search) when one is
/// wired — additive, the deck surface stays the default.
pub struct MockBackendImpl {
    deck: Deck,
    draft_surface: MockDraftSurface,
    corpus_surface: Option<CorpusSurface>,
}

impl MockBackendImpl {
    pub fn new(deck: Deck, draft_surface: MockDraftSurface) -> MockBackendImpl {
        MockBackendImpl {
            deck,
            draft_surface,
            corpus_surface: None,
        }
    }

    /// The t1g rung-2 constructor: the mock serves its estate leg from
    /// a real corpus (or several) instead of the decked empty.
    pub fn with_corpus(
        deck: Deck,
        draft_surface: MockDraftSurface,
        surface: CorpusSurface,
    ) -> MockBackendImpl {
        MockBackendImpl {
            deck,
            draft_surface,
            corpus_surface: Some(surface),
        }
    }

    /// The web backend id the loop must be configured with. The mock
    /// refuses any other backend name — a misspelled or unregistered
    /// backend must not silently route (the closed-set rule).
    pub const BACKEND_ID: &'static str = "mock";

    fn row_log(&self, row: &str, fired: &str) {
        // Glassbox: which F-table row a deck exercised, on every firing
        // (a row whose detection never fires is named, not silent).
        tracing::debug!(target: "deep_research_gym", row, fired, "F-table row fired");
        eprintln!("gym: F-table row {row} fired ({fired})");
    }
}

#[async_trait::async_trait]
impl ResearchPort for MockBackendImpl {
    async fn estate_listing(&self, _corpus_ids: &[String]) -> Result<EstateListing, String> {
        let corpora: Vec<CorpusEntry> = self
            .deck
            .corpora
            .iter()
            .map(|c| CorpusEntry {
                corpus_id: c.corpus_id.clone(),
                kind: c.kind.clone(),
                chunks_count: c.chunks_count,
                searchable: c.searchable,
                custody: c.custody.clone(),
            })
            .collect();
        Ok(EstateListing { corpora })
    }

    async fn estate_search(
        &self,
        corpus_ids: &[String],
        query: &str,
        limit: usize,
    ) -> Result<Vec<PortHit>, String> {
        // Without a corpus surface, v1 deck estates declare LISTING
        // state (F13/F16's searchable shape) but no content rows — a
        // decked estate answers "nothing" honestly. The drill's estate
        // is empty; content rows would be a later deck extension.
        let Some(surface) = &self.corpus_surface else {
            return Ok(Vec::new());
        };
        // Real corpus search (t1g rung 2): the estate's corpus-search
        // surface — vector + FTS hybrid, the same surface the CLI port
        // uses. A corpus id the surface does not serve refuses loudly
        // (a configuration bug is never a silent empty).
        let mut hits: Vec<PortHit> = Vec::new();
        for id in corpus_ids {
            let Some(index) = surface.indexes.iter().find(|i| i.corpus_id() == id) else {
                return Err(format!(
                    "corpus search: corpus `{id}` is not on the mock's surface \
                     (serves: {})",
                    surface
                        .indexes
                        .iter()
                        .map(|i| i.corpus_id())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            };
            let embedding = surface.embed.embed(query).await?;
            let results = index
                .search(&embedding, query, limit)
                .await
                .map_err(|e| format!("corpus search `{id}`: {e}"))?;
            tracing::debug!(
                target: "deep_research_gym",
                rule = "corpus-search",
                corpus = id,
                query,
                hits = results.len(),
                "corpus surface retrieval"
            );
            for r in results {
                // The locator carries the CHUNK id (the estate window's
                // `estate:<corpus_id>:<chunk_id>` convention) — a
                // corpus-level-only locator would collapse every chunk
                // of a corpus in the window's dedup-by-url (the bug
                // journaled in the t1g declaration).
                let chunk_id = r
                    .chunk_id
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "0".to_string());
                hits.push(PortHit {
                    id: chunk_id.clone(),
                    url: format!("estate:{id}:{chunk_id}"),
                    title: r.title.unwrap_or_default(),
                    snippet: estate_snippet(&r.content, query, 600),
                    score: r.score as f64,
                    source: format!("estate:{id}"),
                    custody: Custody::Personal,
                });
            }
        }
        Ok(hits)
    }

    async fn web_search(
        &self,
        backend: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<PortHit>, String> {
        if backend != Self::BACKEND_ID {
            return Err(format!(
                "unknown web backend `{backend}` (closed set: {})",
                Self::BACKEND_ID
            ));
        }
        // Term-ranked retrieval (T1.9): every hit with term overlap is
        // scored and ranked — relevance desc, then the deck's declared
        // score desc (a deck prior breaking retrieval ties, never
        // overriding a relevance difference), then insertion order.
        // The returned score IS the relevance: the loop's triage ranks
        // by it (the t1e-era all-0.9 ties were the exact-value
        // instrument's flat defaults).
        let mut scored: Vec<(usize, usize, &DeckHit)> = Vec::new();
        for (i, hit) in self.deck.hits.iter().enumerate() {
            let rel = self.deck.relevance(i, query);
            if rel > 0 {
                scored.push((rel, i, hit));
            }
        }
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.2.score.total_cmp(&a.2.score))
                .then_with(|| a.1.cmp(&b.1))
        });
        if scored.is_empty() {
            // Zero results are a RECORD (F1/F28), never an error — the
            // loop's search call must see Ok(empty).
            return Ok(Vec::new());
        }
        tracing::debug!(
            target: "deep_research_gym",
            rule = "term-ranked",
            query,
            hits = scored.len(),
            "mock search retrieval"
        );
        let mut hits: Vec<PortHit> = Vec::new();
        for (rel, _, hit) in scored.into_iter().take(limit) {
            if let Some(row) = &hit.f_row {
                self.row_log(
                    row,
                    &format!("deck hit {} retrieved (term relevance {rel})", hit.url),
                );
            }
            hits.push(PortHit {
                id: hit.id.clone().unwrap_or_default(),
                url: hit.url.clone(),
                title: hit.title.clone(),
                snippet: hit.snippet.clone(),
                score: rel as f64,
                source: format!("web:{}", Self::BACKEND_ID),
                custody: Custody::parse_wire(&hit.custody).unwrap_or(Custody::PublicWeb),
            });
        }
        Ok(hits)
    }

    async fn web_fetch(&self, url: &str) -> Result<String, String> {
        // The estate scheme (t1g rung 2): `estate:<corpus_id>:<chunk_id>`
        // — the corpus IS the evidence store, the chunk's own content is
        // the fetch. Resolves from the corpus surface; without a surface
        // an estate url refuses loudly (never a silent empty).
        if let Some(rest) = url.strip_prefix("estate:") {
            let Some(surface) = &self.corpus_surface else {
                return Err(format!(
                    "not served: {url} — the mock has no corpus surface (estate urls \
                     resolve only when one is wired)"
                ));
            };
            let (id, chunk) = rest.split_once(':').ok_or_else(|| {
                format!("malformed estate locator: {url} (expected estate:<corpus_id>:<chunk_id>)")
            })?;
            let chunk_id: u64 = chunk.parse().map_err(|_| {
                format!("malformed estate locator: {url} (chunk id `{chunk}` is not an id)")
            })?;
            let index = surface
                .indexes
                .iter()
                .find(|i| i.corpus_id() == id)
                .ok_or_else(|| {
                    format!("not served: {url} — corpus `{id}` is not on the surface")
                })?;
            let stored = index
                .get_chunks(&[chunk_id])
                .await
                .map_err(|e| format!("estate fetch {url}: {e}"))?;
            return stored
                .into_iter()
                .next()
                .map(|c| c.content)
                .ok_or_else(|| format!("estate fetch {url}: chunk {chunk_id} not found"));
        }
        if let Some(fail) = self.deck.fails.get(url) {
            if let Some(row) = &fail.f_row {
                self.row_log(row, &format!("fetch of {url} refused by the deck"));
            }
            // The deck's fail wins over the hit: F2's shape is a page
            // that search returned and the fetch then refused.
            return Err(fail.reason.clone());
        }
        match self.deck.url_bodies.get(url) {
            Some(body) => Ok(body.clone()),
            None => Err(format!("not in deck: {url} — a fetch of an unknown url is a deck bug, never a silent empty")),
        }
    }

    async fn terminal_poll(&self) -> Result<(), String> {
        match &self.draft_surface {
            MockDraftSurface::Scripted(_) => Ok(()),
            MockDraftSurface::Delegated(inner) => inner.terminal_poll().await,
        }
    }

    async fn draft(
        &self,
        _prompt: &str,
        _system_message: Option<&str>,
        _allowed_urls: &[String],
    ) -> Result<String, String> {
        match &self.draft_surface {
            MockDraftSurface::Scripted(text) => Ok(text.clone()),
            MockDraftSurface::Delegated(inner) => {
                inner.draft(_prompt, _system_message, _allowed_urls).await
            }
        }
    }

    async fn plan_subquestions(&self, question: &str) -> Result<Vec<String>, String> {
        // The mock follows its draft surface (one decider per surface):
        // Scripted — the scripted text's non-empty lines ARE the
        // frontier (deterministic gym tests), deduped, capped at
        // FRONTIER_MAX like every surface; Delegated — the inner port's
        // decomposition (the CLI drill's real model call).
        match &self.draft_surface {
            MockDraftSurface::Scripted(text) => {
                let mut out: Vec<String> = Vec::new();
                for line in text.lines() {
                    let line = line.trim();
                    if !line.is_empty() && !out.contains(&line.to_string()) {
                        out.push(line.to_string());
                    }
                    if out.len() >= FRONTIER_MAX {
                        break;
                    }
                }
                Ok(out)
            }
            MockDraftSurface::Delegated(inner) => inner.plan_subquestions(question).await,
        }
    }

    async fn alignment_decision(
        &self,
        _plan: &Plan,
        run_dir: &Path,
    ) -> Result<AlignmentDecision, String> {
        // STEER 2: the gym's alignment gate is the STAGED INPUT — the
        // drill stages `<run_dir>/alignment-input.json` (ReframeInput
        // shape) to exercise the redirect path; the shared reader
        // consumes the file (one decider, mock and CLI alike). Absent
        // → Proceed, byte-identical to a run without the gate.
        read_staged_alignment(run_dir).map(|staged| staged.unwrap_or(AlignmentDecision::Proceed))
    }
}

// ---------------------------------------------------------------------------
// The F-table — the typed representation of the spec's FMEA table.
// Every row is watched (a fixture below fires its detection) or named
// (the reason is carried in the table itself).
// ---------------------------------------------------------------------------

/// The row status: watched (a fixture exercises the detection) or
/// named (the reason the detection is not exercised is carried here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    Watched,
    /// The reason this row is not fixture-watched (a named row is never
    /// silently absent — §18.1's four verdicts applied to the table).
    Named(&'static str),
}

/// One FMEA row, typed.
#[derive(Debug, Clone, Copy)]
pub struct FRow {
    pub id: &'static str,
    pub component: &'static str,
    pub mode: &'static str,
    /// The detection point — the instrument that fires the row.
    pub detection: &'static str,
    /// The rehearsed response.
    pub response: &'static str,
    pub status: RowStatus,
}

/// THE F-table: 28 rows, F1-F28, matching the spec's FMEA table
/// (spec text is the render of this table — one enumeration, one name).
pub const FTABLE: [FRow; 28] = [
    FRow { id: "F1", component: "R4", mode: "backend 0-results", detection: "orchestrator result empty, backend id logged", response: "try next backend in preference order; all dry → gap stays open, recorded `unsearchable` this round", status: RowStatus::Watched },
    FRow { id: "F2", component: "R6", mode: "fetch 404 / timeout / paywall stub", detection: "HTTP status; extracted-text-length floor", response: "source recorded absent in manifest with reason; never silently dropped", status: RowStatus::Watched },
    FRow { id: "F3", component: "R6", mode: "fetched page is boilerplate/garbage", detection: "extraction yield below floor", response: "chunk not ingested; source marked `low-yield`", status: RowStatus::Named("the extraction yield floor lives upstream of the port (the CLI extractor); the gym injects post-extraction text, so the floor is exercised by the real fetch path, never by the deck") },
    FRow { id: "F4", component: "R5/R8", mode: "prompt injection in fetched content", detection: "(contained, not detected)", response: "FR containment corollary: outputs are typed data; worst case one round's wasted budget", status: RowStatus::Watched },
    FRow { id: "F5", component: "R7", mode: "enrichment fabricates", detection: "derived-vs-primary tag + faithful-mode verify (precondition 1)", response: "derived evidence discounted/excluded at gate; fabrication caught by R9 dual-string", status: RowStatus::Named("the derived-vs-primary tag exists (enrich_window); the GATE-side discount of derived evidence is the T2/GAP-2-adjacent shape, not yet wired — the tag is watched by enrich's own tests") },
    FRow { id: "F6", component: "R8", mode: "synthesist invents a citation", detection: "URL/citation not in evidence window (C check)", response: "citation stripped + reported; claim re-gated without it", status: RowStatus::Watched },
    FRow { id: "F7", component: "R8", mode: "frontier key dies / cap hit mid-synthesis", detection: "provider error / spend meter", response: "fall back to local synthesis, report the substitution by name (never silent)", status: RowStatus::Named("the local-only loop has no frontier key; the fallback path is the CLI/desktop layer's, exercised there") },
    FRow { id: "F8", component: "R3/R9", mode: "judge timeout or malformed verdict", detection: "typed verdict parse; watchdog", response: "verdict = could-not-judge, never defaulted to pass or fail", status: RowStatus::Watched },
    FRow { id: "F9", component: "R9", mode: "dual-string disagreement", detection: "agreement check", response: "could-not-judge → claim lands in Open questions", status: RowStatus::Named("v1 runs the FR-6 redesigned single-string + C-class witness posture (dr-instrument-validated met); the two-register agreement check is T2") },
    FRow { id: "F10", component: "R10", mode: "payload contains non-public chunk", detection: "custody-class scan over exact payload", response: "typed refusal naming withheld chunks; R8 splits local/remote", status: RowStatus::Named("R10 egress refusal is dr-egress's T2 territory; the custody scan is a build-gate shape, not deck-injectable") },
    FRow { id: "F11", component: "R11", mode: "daemon death / harness kill mid-run", detection: "job status; stale heartbeat", response: "resume from last ICD artifact; launchd one-shot relaunch per convention", status: RowStatus::Named("the abort landing is watched by the loop's abort tests; the job-status relaunch is the launchd/daemon layer, outside the port") },
    FRow { id: "F12", component: "R11", mode: "budget exhausted before convergence", detection: "spend meters vs. charter caps", response: "DONE-PARTIAL: gated report + truncation declared, never presented as complete", status: RowStatus::Watched },
    FRow { id: "F13", component: "R2", mode: "estate index stale/corrupt", detection: "corpus meta validation at survey", response: "loud degradation: run proceeds web-first with the estate absence reported", status: RowStatus::Watched },
    FRow { id: "F14", component: "R7", mode: "circular evidence: a derived chunk becomes the evidence for the claim that produced it", detection: "derived-vs-primary tag on every derived chunk; gate eligibility checks the tag", response: "derived evidence discounted at the gate; a claim resting solely on derived support is re-gated against primary evidence", status: RowStatus::Named("the gate-side derived discount is the same T2 wire as F5") },
    FRow { id: "F15", component: "R6/R7", mode: "unstamped derived chunk reaches the gate", detection: "custody stamped at derivation — lattice join over the inputs, computed at creation", response: "unknown/partial provenance refuses — typed refusal naming the withheld chunk, never a silent pass", status: RowStatus::Named("the custody refusal's unknown path is watched by tests/custody_reds.rs (R-3); the unstamped-DERIVED shape is the T2 enrichment join") },
    FRow { id: "F16", component: "R2", mode: "estate-unsearchable reads as no evidence", detection: "empty-estate precondition is a searchability assert at survey", response: "run proceeds web-first with the estate absence reported loud — never an unlabeled no evidence", status: RowStatus::Watched },
    FRow { id: "F17", component: "R6", mode: "ingest laundering: content written to the estate without its custody/source stamp", detection: "custody stamped by the fetcher; ingest asserts the stamp on every write", response: "unstamped write is a loud error — the chunk does not enter the estate silently", status: RowStatus::Named("the estate WRITE (ingested_into) is the T2 compounding surface; the fetch-side stamp is watched by fetch.rs's custody tests") },
    FRow { id: "F18", component: "R7", mode: "dead-inference enrichment silently yields nothing", detection: "enrichment faithfulness asserts; a zero-yield enrich round is an error", response: "loud degradation: the round's yield recorded and the failure reported by name", status: RowStatus::Named("enrich_window is C-class tags in v1 (no inference to die); the faithful-mode asserts are the T2 R7 regime") },
    FRow { id: "F19", component: "R11", mode: "run collisions: two runs against the same run dir race", detection: "flock on `<run_dir>/lock` at acquisition; lifecycle in the manifest", response: "second opener refuses — a typed refusal, never a silent second writer", status: RowStatus::Named("watched by state.rs lock_refuses_second_run (the lock is state-layer; the deck has no lock surface)") },
    FRow { id: "F20", component: "R11", mode: "budget-meter drift: meter and decider disagree", detection: "one decider one name: the meter is the decider's own record; drift asserted", response: "meter/decider disagreement is a loud error; spend never trusted from two sources", status: RowStatus::Named("the decider IS the meter (single journal); the drift assert lands with dr-budget-one-decider's T1 search half") },
    FRow { id: "F21", component: "R11/R2", mode: "stale evidence past the charter's freshness horizon", detection: "charter freshness horizon checked at survey; stale chunks flagged", response: "stale chunks excluded from the window and reported; fresh search prioritized", status: RowStatus::Named("v1's charter has no freshness horizon — the horizon is T2") },
    FRow { id: "F22", component: "R3/R9", mode: "near-duplicate inflation: coverage counts chunks, so five copies of one source look corroborated", detection: "coverage counts distinct origins, never chunks — the derivation DAG's distinct provenance components", response: "the corroboration floor (GAP-2): a claim whose support set has <2 distinct origins caps at could-not-judge", status: RowStatus::Watched },
    FRow { id: "F23", component: "R4", mode: "result-SET poisoning: the planted source appears in force", detection: "results are untrusted typed data (containment corollary); the gym deck injects sets, not single plants", response: "worst case one wasted round — the corroboration floor keeps any single-origin claim from passing", status: RowStatus::Watched },
    FRow { id: "F24", component: "R1/R2", mode: "mis-framed plan: so broad or unanswerable that no gap can fail it", detection: "plan sub-questions are typed data with acceptance shapes; the coverage key authorable without consulting system output", response: "a plan whose sub-questions are not search-actionable is refused at planning — a typed refusal, never a pass", status: RowStatus::Named("the acceptance-shape coverage key is the bank-mint shape (dr-local-loop's P4); not deck-injectable in v1") },
    FRow { id: "F25", component: "R5", mode: "systematic triage bias: the ranker excludes a class invisibly", detection: "skip-ledger records every exclusion; ε-quota reserves below-cut fetches", response: "bias is auditable from the ledger — every exclusion is on the record, never silent", status: RowStatus::Watched },
    FRow { id: "F26", component: "R10", mode: "boundary bypass: a remote client construction outside the egress boundary", detection: "F26 census: every remote client construction routes through the boundary, enforced as a build gate", response: "a bypass is a build failure, not a runtime surprise", status: RowStatus::Named("dr-egress's build gate; a build-shape, not a deck shape") },
    FRow { id: "F27", component: "R2/R7", mode: "foreign embedding spaces retrieved incoherently", detection: "embedding space stamped at ingestion; cross-space retrieval refused or reported", response: "a mixed-space window is refused loudly; mesh sharing stays behind this (SearchPrivacy::Mesh is a placeholder)", status: RowStatus::Named("the estate-layer embed-space stamp is corpus-engine's; not a loop-port shape") },
    FRow { id: "F28", component: "R4/R3", mode: "instrument unavailable ≠ could-not-judge", detection: "empty search results are Ok(empty) records, never Err; an empty window never enters the judge", response: "instrument absence reported by name; could-not-judge stays a verdict about the evidence, never the instrument", status: RowStatus::Watched },
];

/// Look a row up by id.
pub fn frow(id: &str) -> Option<&'static FRow> {
    FTABLE.iter().find(|r| r.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row of the table is watched or named — a row whose
    /// detection never fires and whose absence has no stated reason is
    /// a silent hole in the table.
    #[test]
    fn f_table_every_row_is_watched_or_named() {
        assert_eq!(FTABLE.len(), 28, "the table must carry F1-F28");
        let mut prev = 0u8;
        for row in FTABLE.iter() {
            let n: u8 = row.id[1..]
                .parse()
                .unwrap_or_else(|_| panic!("row id malformed: {}", row.id));
            assert_eq!(n, prev + 1, "rows must be sequential F1..F28");
            prev = n;
            match row.status {
                RowStatus::Watched => {
                    assert!(
                        FIXTURES.iter().any(|(id, _)| *id == row.id),
                        "watched row {} has no fixture — a row whose detection never fires is named, not silent",
                        row.id
                    );
                }
                RowStatus::Named(reason) => {
                    assert!(
                        !reason.is_empty(),
                        "row {} is named with an empty reason",
                        row.id
                    );
                }
            }
        }
        // The fixture ids are all real rows.
        for (id, _) in FIXTURES.iter() {
            assert!(frow(id).is_some(), "fixture for unknown row {id}");
        }
    }

    /// The fixture registry: every watched row's detection, as a deck
    /// (or, for the audit-shaped rows, the shape the deck produces).
    /// Adding a watched row without its fixture fails the coverage test
    /// above.
    const FIXTURES: &[(&str, fn() -> Deck)] = &[
        ("F1", f1_deck),
        ("F2", f2_deck),
        ("F4", f4_deck),
        ("F6", f6_deck),
        ("F8", f8_deck),
        ("F12", f12_deck),
        ("F13", f13_deck),
        ("F16", f16_deck),
        ("F22", f22_deck),
        ("F23", f23_deck),
        ("F25", f25_deck),
        ("F28", f28_deck),
    ];

    fn deck(toml: &str, bodies: &[(&str, &str)]) -> Deck {
        Deck::parse(toml, bodies).expect("fixture deck must build")
    }

    fn f1_deck() -> Deck {
        deck(
            "version = 1\n# no hits — the backend 0-results shape (F1/F28)\n",
            &[],
        )
    }

    fn f2_deck() -> Deck {
        deck(
            "version = 1\n\
             [[hit]]\n\
             match = [\"bridge\"]\n\
             url = \"https://gym.example/bridge\"\n\
             title = \"The bridge page\"\n\
             snippet = \"A snippet about the bridge.\"\n\
             body = \"bridge.md\"\n\
             [[fail]]\n\
             url = \"https://gym.example/bridge\"\n\
             reason = \"404\"\n\
             f_row = \"F2\"\n",
            &[("bridge.md", "The Meridian Bridge was completed in 1873.")],
        )
    }

    fn f4_deck() -> Deck {
        deck(
            "version = 1\n\
             [[hit]]\n\
             match = [\"bridge\"]\n\
             url = \"https://gym.example/plant\"\n\
             title = \"A planted page\"\n\
             snippet = \"The bridge page.\"\n\
             body = \"plant.md\"\n\
             f_row = \"F4\"\n\
             [[hit]]\n\
             match = [\"bridge\"]\n\
             url = \"https://gym.example/plant2\"\n\
             title = \"Another planted page\"\n\
             snippet = \"More of the same.\"\n\
             body = \"plant2.md\"\n\
             f_row = \"F23\"\n",
            &[
                ("plant.md", "The Meridian Bridge was completed in 1873."),
                ("plant2.md", "The Meridian Bridge was completed in 1873."),
            ],
        )
    }

    /// F22 — the corroboration floor's shape: the planted page is ONE
    /// origin, however many copies of it the window carries. The deck
    /// carries a single plant (the minimal near-duplicate-inflation
    /// surface); a claim that would pass on it alone is exactly what the
    /// floor caps at could-not-judge.
    fn f22_deck() -> Deck {
        deck(
            "version = 1\n\
             [[hit]]\n\
             match = [\"bridge\"]\n\
             url = \"https://gym.example/plant\"\n\
             title = \"A single planted page\"\n\
             snippet = \"The bridge page.\"\n\
             body = \"plant.md\"\n\
             f_row = \"F22\"\n",
            &[("plant.md", "The Meridian Bridge was completed in 1873.")],
        )
    }

    fn f6_deck() -> Deck {
        deck(
            "version = 1\n\
             [[hit]]\n\
             match = [\"bridge\"]\n\
             url = \"https://gym.example/bridge\"\n\
             title = \"The bridge page\"\n\
             snippet = \"A snippet about the bridge.\"\n\
             body = \"bridge.md\"\n",
            &[("bridge.md", "The Meridian Bridge was completed in 1873.")],
        )
    }

    fn f8_deck() -> Deck {
        deck(
            "version = 1\n\
             [[hit]]\n\
             match = [\"bridge\"]\n\
             url = \"https://gym.example/bridge\"\n\
             title = \"The bridge page\"\n\
             snippet = \"A snippet about the bridge.\"\n\
             body = \"bridge.md\"\n",
            &[("bridge.md", "The Meridian Bridge was completed in 1873.")],
        )
    }

    fn f12_deck() -> Deck {
        f1_deck()
    }

    fn f13_deck() -> Deck {
        deck(
            "version = 1\n\
             [[corpus]]\n\
             corpus_id = \"broken\"\n\
             kind = \"documents\"\n\
             chunks_count = 42\n\
             searchable = false\n\
             custody = \"public-web\"\n",
            &[],
        )
    }

    fn f16_deck() -> Deck {
        f13_deck()
    }

    fn f23_deck() -> Deck {
        f4_deck()
    }

    fn f25_deck() -> Deck {
        deck(
            "version = 1\n\
             [[hit]]\n\
             match = [\"bridge\"]\n\
             url = \"https://gym.example/high\"\n\
             title = \"The high hit\"\n\
             snippet = \"High.\"\n\
             body = \"high.md\"\n\
             score = 0.9\n\
             [[hit]]\n\
             match = [\"bridge\"]\n\
             url = \"https://gym.example/low\"\n\
             title = \"The low hit\"\n\
             snippet = \"Low.\"\n\
             body = \"low.md\"\n\
             score = 0.1\n",
            &[("high.md", "High content."), ("low.md", "Low content.")],
        )
    }

    fn f28_deck() -> Deck {
        f1_deck()
    }

    // ------------------------------------------------------------------
    // Deck mechanics
    // ------------------------------------------------------------------

    #[test]
    fn deck_load_refuses_missing_body() {
        let err = Deck::parse("version = 1\n[[hit]]\nmatch = [\"x\"]\nurl = \"https://gym.example/x\"\ntitle = \"t\"\nsnippet = \"s\"\nbody = \"nope.md\"\n", &[])
            .expect_err("a missing body must refuse loudly");
        assert!(err.contains("missing"), "got: {err}");
    }

    #[test]
    fn deck_load_refuses_wrong_version() {
        let err = Deck::parse("version = 99\n", &[]).expect_err("bad version refuses");
        assert!(err.contains("version 99"), "got: {err}");
    }

    #[tokio::test]
    async fn deck_url_in_hit_and_fail_parses_and_fail_wins_at_fetch() {
        // F2's shape: search returns the page, the fetch refuses. A url
        // in both sets is legal, and the fail wins.
        let d = deck(
            "version = 1\n\
             [[hit]]\n\
             match = [\"x\"]\n\
             url = \"https://gym.example/u\"\n\
             title = \"t\"\n\
             snippet = \"s\"\n\
             body = \"b.md\"\n\
             [[fail]]\n\
             url = \"https://gym.example/u\"\n\
             reason = \"404\"\n",
            &[("b.md", "content")],
        );
        let port = MockBackendImpl::new(d, MockDraftSurface::Scripted("x".to_string()));
        let err = port
            .web_fetch("https://gym.example/u")
            .await
            .expect_err("the fail wins at fetch");
        assert_eq!(err, "404");
    }

    #[test]
    fn deck_load_refuses_duplicate_fail_url() {
        let err = Deck::parse(
            "version = 1\n\
             [[fail]]\n\
             url = \"https://gym.example/u\"\n\
             reason = \"404\"\n\
             [[fail]]\n\
             url = \"https://gym.example/u\"\n\
             reason = \"500\"\n",
            &[],
        )
        .expect_err("a url's failure reason is unique");
        assert!(err.contains("twice"), "got: {err}");
    }

    /// T1.9: the retrieval decider is TERM OVERLAP over the hit's full
    /// indexed surface (match tokens + title + snippet + body), not
    /// substring matching over the curated tokens — case-insensitive by
    /// construction (the one tokenizer). "bridges" does not match
    /// "bridge" (no stemming — a term is a term); a word in the BODY
    /// matches without being any match token.
    #[test]
    fn query_match_is_term_overlap_case_insensitive() {
        let d = f4_deck();
        // The match token is part of the indexed surface — still a hit.
        assert!(d.query_matches(0, "Why did the bridge fail?"));
        assert!(d.query_matches(0, "BRIDGE HISTORY"));
        // No overlapping term: no hit.
        assert!(!d.query_matches(0, "nothing about railways"));
        // Term semantics, not substring: "bridges" != "bridge".
        assert!(!d.query_matches(0, "railways and bridges"));
        // A BODY term matches without being any match token — the
        // concept-query shape (the value document is reachable through
        // its own words).
        assert!(d.query_matches(0, "meridian bridge"));
        // An empty or punctuation-only query matches nothing.
        assert!(!d.query_matches(0, ""));
        assert!(!d.query_matches(0, "?!"));
    }

    #[tokio::test]
    async fn web_search_zero_results_is_a_record() {
        let d = f1_deck();
        let port = MockBackendImpl::new(d, MockDraftSurface::Scripted("x".to_string()));
        let port: &dyn ResearchPort = &port;
        let hits = port.web_search("mock", "anything at all", 10).await;
        assert!(
            hits.expect("empty results are Ok(empty), never Err")
                .is_empty(),
            "empty results are a record, never an Err"
        );
    }

    #[tokio::test]
    async fn web_search_refuses_non_mock_backend() {
        let port = MockBackendImpl::new(f1_deck(), MockDraftSurface::Scripted("x".to_string()));
        let port: &dyn ResearchPort = &port;
        let err = port
            .web_search("duckduckgo", "q", 10)
            .await
            .expect_err("a misspelled backend must not silently route");
        assert!(err.contains("closed set"), "got: {err}");
    }

    #[tokio::test]
    async fn web_fetch_unknown_url_is_loud() {
        let port = MockBackendImpl::new(f4_deck(), MockDraftSurface::Scripted("x".to_string()));
        let port: &dyn ResearchPort = &port;
        let err = port
            .web_fetch("https://other.example/not-in-deck")
            .await
            .expect_err("unknown urls must be loud, never silent empties");
        assert!(err.contains("not in deck"), "got: {err}");
    }

    #[tokio::test]
    async fn web_fetch_fail_is_an_err_with_the_deck_reason() {
        let port = MockBackendImpl::new(f2_deck(), MockDraftSurface::Scripted("x".to_string()));
        let port: &dyn ResearchPort = &port;
        let err = port
            .web_fetch("https://gym.example/bridge")
            .await
            .expect_err("a deck fail must refuse the fetch");
        assert_eq!(err, "404");
    }

    // ------------------------------------------------------------------
    // The corpus surface (t1g rung 2) — port-level halves of the
    // loop-level red-first test (mod.rs):
    //   1. a concept query retrieves the value-bearing chunk through
    //      the corpus source, locator `estate:<corpus>:<chunk>`;
    //   2. web_fetch resolves the estate scheme from the chunk store;
    //   3. the closed set refuses loudly: an estate url with no
    //      surface, a corpus id the surface does not serve, a
    //      malformed locator — never a silent empty.

    async fn fixture_corpus_surface(dir: &Path, corpus_id: &str) -> CorpusSurface {
        use corpus_engine::index::{InsertChunk, InsertCodeMeta};
        const EMBED_DIM: usize = 8;
        fn embedding(seed: f32) -> Vec<f32> {
            (0..EMBED_DIM).map(|i| seed + i as f32 * 0.1).collect()
        }
        struct FakeEmbed;
        #[async_trait::async_trait]
        impl CorpusEmbed for FakeEmbed {
            async fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
                let seed = text.bytes().fold(0f32, |a, b| a + b as f32) % 100.0;
                Ok(embedding(seed))
            }
        }
        let index = corpus_engine::CorpusIndex::create(
            dir,
            corpus_id,
            "Gym fixture",
            "test-embed",
            EMBED_DIM,
            true,
            "MIT",
        )
        .await
        .expect("fixture corpus creates");
        let rows = [
            (
                "New York City's Gini coefficient of income inequality reached 0.5469 in \
                 2019 — the highest of any large American city.",
                "NYC inequality",
            ),
            (
                "The municipal zoning commission voted on a parks bond on Tuesday.",
                "Distractor",
            ),
        ];
        let payload: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(i, (content, title))| {
                (
                    InsertChunk {
                        content: content.to_string(),
                        title: Some(title.to_string()),
                        url: None,
                        metadata: None,
                        content_hash: None,
                        source_doc_id: None,
                        source_file: None,
                        code: InsertCodeMeta::default(),
                        unit_id: None,
                    },
                    embedding(i as f32),
                )
            })
            .collect();
        index.insert_batch(&payload).await.expect("chunks insert");
        index
            .build_indexes(true, true, None)
            .await
            .expect("indexes build");
        index.mark_indexes_built().expect("marked built");
        index.mark_ingestion_complete().expect("ingestion complete");
        CorpusSurface {
            indexes: vec![index],
            embed: Box::new(FakeEmbed),
        }
    }

    #[tokio::test]
    async fn corpus_surface_retrieves_and_fetches_the_value_bearing_chunk() {
        let dir = std::env::temp_dir().join(format!("dr-gym-corpus-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let deck_toml = format!(
            "version = 1\n\
             [[corpus]]\n\
             corpus_id = \"dr-fixture\"\n\
             kind = \"documents\"\n\
             chunks_count = 2\n\
             searchable = true\n\
             custody = \"personal\"\n"
        );
        let deck = Deck::parse(&deck_toml, &[]).expect("corpus deck builds");
        let port = MockBackendImpl::with_corpus(
            deck,
            MockDraftSurface::Scripted("x".to_string()),
            fixture_corpus_surface(&dir, "dr-fixture").await,
        );
        let port: &dyn ResearchPort = &port;

        // The concept query — no figure named — retrieves the
        // value-bearing chunk with a chunk-level estate locator.
        let hits = port
            .estate_search(
                &["dr-fixture".to_string()],
                "how unequal is New York's largest city",
                5,
            )
            .await
            .expect("corpus search ok");
        assert!(
            !hits.is_empty(),
            "the corpus source must answer with real hits"
        );
        let value = hits
            .iter()
            .find(|h| h.url.starts_with("estate:dr-fixture:"))
            .expect("hits carry the estate:<corpus>:<chunk> locator");
        assert!(
            value.url.matches(':').count() == 2,
            "the locator carries the chunk id (the dedup fix): {}",
            value.url
        );
        assert_eq!(value.source, "estate:dr-fixture");
        assert_eq!(value.custody, Custody::Personal);

        // The estate scheme resolves from the chunk store — the corpus
        // IS the evidence store.
        let body = port
            .web_fetch(&value.url)
            .await
            .expect("the estate fetch resolves");
        assert!(
            body.contains("0.5469"),
            "the fetched content is the value-bearing chunk's own: {}",
            body.chars().take(120).collect::<String>()
        );

        // A corpus id the surface does not serve refuses loudly.
        let err = port
            .estate_search(&["other-corpus".to_string()], "any query", 5)
            .await
            .expect_err("an unserved corpus id must refuse");
        assert!(
            err.contains("not on the mock's surface"),
            "the refusal names the mismatch: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn estate_urls_without_a_surface_refuse_loudly() {
        let port = MockBackendImpl::new(f1_deck(), MockDraftSurface::Scripted("x".to_string()));
        let port: &dyn ResearchPort = &port;
        let err = port
            .web_fetch("estate:dr-fixture:3")
            .await
            .expect_err("an estate url with no surface must refuse — never a silent empty");
        assert!(
            err.contains("no corpus surface"),
            "the refusal names the missing surface: {err}"
        );
    }

    #[tokio::test]
    async fn scripted_draft_is_verbatim_and_terminal_poll_is_ok() {
        let port = MockBackendImpl::new(
            f1_deck(),
            MockDraftSurface::Scripted("The canned draft.".to_string()),
        );
        let port: &dyn ResearchPort = &port;
        let text = port.draft("p", None, &[]).await.unwrap();
        assert_eq!(text, "The canned draft.");
        assert!(port.terminal_poll().await.is_ok());
    }

    #[test]
    fn deck_fixture_bodies_are_present() {
        for (id, fixture) in FIXTURES.iter() {
            let d = fixture();
            for hit in &d.hits {
                assert!(
                    d.bodies.contains_key(&hit.body),
                    "fixture {id}: hit {} body {} missing",
                    hit.url,
                    hit.body
                );
                assert!(
                    d.url_bodies.contains_key(&hit.url),
                    "fixture {id}: hit {} has no url-keyed fetch index",
                    hit.url
                );
            }
        }
    }

    /// RED-first (order deep-research-t1f — T1.9 realistic mock
    /// retrieval): a query for a CONCEPT retrieves the deck document
    /// whose BODY carries the value, without the query naming the
    /// value — the exact-match mock cannot do this.
    ///
    /// The t1e cap (journaled in pre-registration.md): the v1 deck's
    /// value-bearing hits (wikipedia-states' "Gini index 0.5469") were
    /// retrievable only through match tokens an honest loop cannot
    /// produce (bank vocabulary never enters a prompt), so P4-v1 sat
    /// at 3/16 loop vs 7/16 one-shot with the residual gap named "the
    /// deck's SPECIFIC values". Real search retrieves by TERM
    /// relevance: a query naming the concept ("how unequal is New
    /// York's largest city") hits the document containing "New York
    /// City (Gini index 0.5469)" without the loop ever knowing the
    /// value. The fixture mirrors that shape: match tokens are
    /// deliberately value-free AND concept-free (no substring of the
    /// query appears among them), while the body carries both the
    /// concept words and the value.
    ///
    /// Watch-it-fail: at HEAD the exact-value matcher returns zero
    /// hits for this query (no match token in it) and the retrieval
    /// assertion fails; the term-ranked instrument retrieves the
    /// value-bearing document with its term relevance.
    #[tokio::test]
    async fn concept_query_retrieves_value_document() {
        let d = deck(
            "version = 1\n\
             [[hit]]\n\
             match = [\"state inequality tables\"]\n\
             url = \"https://gym.example/inequality\"\n\
             title = \"List of U.S. states by income inequality\"\n\
             snippet = \"New York is the state with the highest inequality.\"\n\
             body = \"ineq.md\"\n",
            &[(
                "ineq.md",
                "The economy of New York City (Gini index 0.5469) relies on high-salary earners.",
            )],
        );
        let port = MockBackendImpl::new(d, MockDraftSurface::Scripted("x".to_string()));
        let port: &dyn ResearchPort = &port;
        // The concept query: no figure, no match-token substring.
        let hits = port
            .web_search("mock", "how unequal is New York's largest city", 10)
            .await
            .expect("search is Ok(empty) at worst, never Err");
        let hit = hits
            .iter()
            .find(|h| h.url == "https://gym.example/inequality")
            .unwrap_or_else(|| {
                panic!(
                    "a concept query must retrieve the document whose body carries the value \
                     (the exact-value mock cannot do this): {hits:?}"
                )
            });
        assert!(
            hit.score > 0.0,
            "the retrieved hit carries its term relevance, not a flat default"
        );
    }

    /// T1.9: the search leg ranks by term relevance — more overlap
    /// ranks first, the deck's declared score breaks ties, a
    /// zero-overlap query returns Ok(empty) (the F1/F28 record), and
    /// the returned scores ARE the relevance counts.
    #[tokio::test]
    async fn term_ranking_orders_hits_by_relevance() {
        let d = deck(
            "version = 1\n\
             [[hit]]\n\
             match = [\"bridge\"]\n\
             url = \"https://gym.example/bridge\"\n\
             title = \"The bridge page\"\n\
             snippet = \"A snippet about the bridge.\"\n\
             body = \"bridge.md\"\n\
             score = 0.9\n\
             [[hit]]\n\
             match = [\"railway\"]\n\
             url = \"https://gym.example/railway\"\n\
             title = \"The railway page\"\n\
             snippet = \"A snippet about the railway.\"\n\
             body = \"railway.md\"\n\
             score = 0.9\n",
            &[
                ("bridge.md", "The Meridian Bridge was completed in 1873."),
                ("railway.md", "The Great Western Railway opened in 1841."),
            ],
        );
        let port = MockBackendImpl::new(d, MockDraftSurface::Scripted("x".to_string()));
        let port: &dyn ResearchPort = &port;

        // "meridian bridge" overlaps ONLY bridge.md (its body terms —
        // not match tokens): the concept-query shape.
        let hits = port
            .web_search("mock", "meridian bridge", 10)
            .await
            .expect("search ok");
        assert_eq!(
            hits.iter().map(|h| h.url.as_str()).collect::<Vec<_>>(),
            vec!["https://gym.example/bridge"],
            "a query must retrieve the document whose TERMS it shares"
        );

        // Three query terms: bridge.md shares 2 (meridian, bridge),
        // railway.md shares 1 (railway) — relevance decides the rank,
        // and the returned scores are the relevance counts.
        let hits = port
            .web_search("mock", "meridian bridge railway", 10)
            .await
            .expect("search ok");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://gym.example/bridge");
        assert_eq!(hits[0].score, 2.0);
        assert_eq!(hits[1].url, "https://gym.example/railway");
        assert_eq!(hits[1].score, 1.0);

        // Zero overlap: Ok(empty), never Err (F1/F28).
        let hits = port
            .web_search("mock", "zipper", 10)
            .await
            .expect("empty results are a record, never Err");
        assert!(hits.is_empty());
    }

    /// T1.9: the deck's declared score breaks retrieval ties only — a
    /// relevance difference always outranks it (the F25 fixture's two
    /// identical-overlap hits rank by the deck prior; a higher-overlap
    /// low-prior hit still ranks first).
    #[tokio::test]
    async fn deck_score_breaks_ties_never_overrides_relevance() {
        let d = deck(
            "version = 1\n\
             [[hit]]\n\
             match = [\"bridge\"]\n\
             url = \"https://gym.example/high\"\n\
             title = \"The high page\"\n\
             snippet = \"A snippet about the bridge.\"\n\
             body = \"high.md\"\n\
             score = 0.9\n\
             [[hit]]\n\
             match = [\"bridge\"]\n\
             url = \"https://gym.example/low\"\n\
             title = \"The low page\"\n\
             snippet = \"A snippet about the bridge.\"\n\
             body = \"low.md\"\n\
             score = 0.1\n",
            &[
                ("high.md", "A page about the Meridian Bridge."),
                ("low.md", "The Meridian Bridge was completed in 1873."),
            ],
        );
        let port = MockBackendImpl::new(d, MockDraftSurface::Scripted("x".to_string()));
        let port: &dyn ResearchPort = &port;

        // Same match token, same snippet; the bodies differ only in
        // which terms they carry. A one-term query ("bridge") overlaps
        // both equally: the tie breaks on the deck's declared score —
        // high before low.
        let hits = port
            .web_search("mock", "bridge", 10)
            .await
            .expect("search ok");
        assert_eq!(
            hits.iter().map(|h| h.url.as_str()).collect::<Vec<_>>(),
            vec!["https://gym.example/high", "https://gym.example/low"],
            "the deck's declared score breaks retrieval ties"
        );

        // A query overlapping the LOW hit more (its body carries
        // completed/1873; the high hit's body does not) outranks the
        // deck prior: relevance decides, the prior only breaks ties.
        let hits = port
            .web_search("mock", "meridian bridge completed 1873", 10)
            .await
            .expect("search ok");
        assert!(
            hits.iter().position(|h| h.url == "https://gym.example/low")
                < hits
                    .iter()
                    .position(|h| h.url == "https://gym.example/high"),
            "a relevance difference overrides the deck prior: {hits:?}"
        );
    }
}
