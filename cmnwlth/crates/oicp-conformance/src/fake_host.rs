// SPDX-License-Identifier: AGPL-3.0-or-later
//! A host fake for the v0.5 checks (oicp-v0.5.md §6): a lawful host that
//! holds the fixture library, and one switch per law that breaks exactly
//! that law. Every v0.5 check is watched red against its switch.
//!
//! The lawful fake aligns with the reference aligner (`quote-align`) and
//! decides locality by the rule of §5.2, so a green run against it is the
//! checks agreeing with the spec, not with a lookup table.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use oicp_types::evidence::{reasons, texts_digest_preimage, DEFAULT_CONTEXT};
use oicp_types::transcript::reasons::NOT_A_TRANSCRIPT;
use oicp_types::{
    AlignRequest, Alignment, Difference, DifferenceKind, Document, HeadingOutline, KnowledgeResult,
    KnowledgeSearchRequest, SourceRef, Span, SpanLocator, TextSlice, TranscriptShape, TurnOutline,
};
use serde_json::{json, Value};

use crate::fixture::{code_points, sha256_hex, Library};
use crate::shelf::Shelf;

mod ingest;
mod records;
mod sentences;
mod verify;

/// The laws a fake can break, one switch each (oicp-v0.5.md §6's table).
#[derive(Debug, Clone, Copy, Default)]
pub struct Breaks {
    /// evidence.text: every span's `exact` is one character off its range.
    pub exact_off_by_one: bool,
    /// evidence.align: replies carry no `texts_digest`.
    pub omit_texts_digest: bool,
    /// evidence.verify: a claim's verdict is a kind outside the closed set.
    pub verify_unknown_verdict: bool,
    /// knowledge.scoped: the `documents` filter is ignored, so hits from
    /// unnamed texts come back.
    pub scope_leaks: bool,
    /// ingest.recipe: an install is missing from the first poll after it.
    pub late_listing: bool,
    /// evidence.verify_correction: a correction's witness is never read
    /// back; every one is `verified`.
    pub witness_unread: bool,
    /// ingest.documents: a returned install's entry lists no documents.
    pub omit_documents: bool,
    /// ingest.documents: each ready document names a text the host does not
    /// serve.
    pub unserved_text_names: bool,
    /// knowledge.document: hits carry their record with metadata emptied.
    pub empty_metadata: bool,
    /// ingest.recipe: a recipe that does not parse is 200 `spawned: false`.
    pub ok_on_bad_recipe: bool,
    /// ingest.recipe_test: the report omits its extract stage.
    pub drop_extract_stage: bool,
    /// auth.named_client: a loopback caller is admitted before its bearer is
    /// read.
    pub loopback_first: bool,
    /// auth.local_peer: locality is the peer address alone.
    pub address_only_locality: bool,
    /// auth.local_peer: every reply, preflights too, grants every origin.
    pub permissive_cors: bool,
    /// knowledge.sentences — a configuration, not a break: the fake's data
    /// file names `rerank` and it holds no rerank slot, so it refuses every
    /// sentence search by that scorer's name.
    pub rerank_without_slot: bool,
    /// knowledge.sentences: advertises `rerank`, holds no rerank slot, and
    /// answers with cosine anyway (`search_with_rerank`'s quiet fallback).
    pub rerank_falls_back: bool,
    /// knowledge.locator: a span is given the heading path of the part
    /// before the one it stands in.
    pub locator_previous_part: bool,
    /// knowledge.locator: a span under no heading is given the text's first
    /// line as its heading path.
    pub locator_invented: bool,
    /// ingest.transcript: a turn is given the turn before it (its header
    /// line read into the previous turn).
    pub turn_read_into_previous: bool,
    /// ingest.transcript: a Whisper segment is given a speaker.
    pub whisper_speaker_invented: bool,
    /// ingest.transcript: a declared transcript in neither shape is stored
    /// as plain text.
    pub non_transcript_stored: bool,
    /// evidence.verify_sentences: the fast tier's literal miss decides
    /// `unsupported`.
    pub fast_hint_decides: bool,
    /// evidence.verify_sentences: a year inside a citation, and a number
    /// inside a locator, are read as specifics.
    pub citation_year_specific: bool,
    /// evidence.verify_sentences: no number inside parentheses is read as a
    /// specific.
    pub parenthesis_dropped: bool,
    /// evidence.verify_sentences: a contradicted passage names no differing
    /// specific.
    pub contradicted_unnamed: bool,
    /// ingest.lookups: an install that did not allow lookups is looked up.
    pub lookup_without_allowance: bool,
    /// ingest.lookups: a resolver's field replaces a declared one.
    pub lookup_overwrites_declared: bool,
    /// ingest.lookups: an identifier that did not resolve is given a title.
    pub lookup_invents_unresolved: bool,
    /// knowledge.sentences: each sentence's range is one code point past
    /// the sentence its `exact` and `content` carry.
    pub sentence_range_off_by_one: bool,
    /// ingest.install_limit: the manifest states `max_install_bytes` and the
    /// install route reads bodies only to the surface's 8 MiB.
    pub install_at_surface_limit: bool,
    /// A configuration, not a break: the extractor trims each text's
    /// trailing whitespace, as the reference host's does, so a text's name
    /// is not the sha256 of its source's bytes.
    pub trims_texts: bool,
}

/// The fake's live named credential.
pub fn named_token() -> String {
    format!("svrn_{}", "1".repeat(64))
}

/// A credential the fake revoked.
pub fn revoked_token() -> String {
    format!("svrn_{}", "2".repeat(64))
}

struct Stored {
    /// The corpus that holds it.
    corpus: String,
    name: String,
    /// sha256 of the source's bytes, the fixture document's text.
    source_sha256: String,
    text: String,
    metadata: Option<Value>,
    /// Where its spans stand, as its extraction found them.
    places: Places,
}

/// The places a stored text's extraction found.
enum Places {
    /// A plain text's headings (v0.6 §2.13).
    Headings(HeadingOutline),
    /// A transcript's turns (v0.6 §2.14).
    Turns(TurnOutline),
}

struct Fake {
    breaks: Breaks,
    corpus: String,
    texts: Vec<Stored>,
    /// Documents an install read and could not store: corpus, name, why.
    failed: Vec<(String, String, &'static str)>,
    installed: Mutex<BTreeMap<String, String>>,
    /// The corpora whose last install allowed lookups (v0.6 §2.16).
    allowed: Mutex<BTreeSet<String>>,
    /// The fake resolver's answers, by identifier.
    resolved: BTreeMap<String, Value>,
    /// Polls of progress since each corpus's last install.
    polls: Mutex<BTreeMap<String, u32>>,
}

impl Fake {
    fn record(&self, s: &Stored) -> Document {
        Document {
            text_sha256: sha256_hex(s.text.as_bytes()),
            extractor: "plaintext@fake".into(),
            source: SourceRef {
                id: s.name.clone(),
                sha256: Some(s.source_sha256.clone()),
            },
            metadata: if self.breaks.empty_metadata {
                None
            } else {
                s.metadata.clone()
            },
            record: self.merged(s),
        }
    }

    /// The sentence scorer the fake's data file names.
    fn scorer(&self) -> &'static str {
        if self.breaks.rerank_without_slot || self.breaks.rerank_falls_back {
            "rerank"
        } else {
            "cosine"
        }
    }

    /// Whether `s` is inside a `documents` scope (§2.8); `scope_leaks`
    /// ignores it.
    fn in_scope(&self, documents: &[String], s: &Stored) -> bool {
        self.breaks.scope_leaks
            || documents.is_empty()
            || documents.contains(&sha256_hex(s.text.as_bytes()))
    }

    /// The stored texts of the named corpora, or of all when none is named.
    fn of<'a>(&'a self, corpora: &'a [String]) -> impl Iterator<Item = &'a Stored> + 'a {
        self.texts
            .iter()
            .filter(move |s| corpora.is_empty() || corpora.contains(&s.corpus))
    }

    /// The corpora a request reads: those it names that the fake holds, or
    /// every one.
    fn searched(&self, corpora: &[String]) -> Vec<String> {
        let mut held: Vec<String> = Vec::new();
        for s in self.of(corpora) {
            if !held.contains(&s.corpus) {
                held.push(s.corpus.clone());
            }
        }
        held
    }

    /// Where `start` stands in `s` (§2.13): the heading path open there.
    /// `locator_previous_part` names the heading before that one;
    /// `locator_invented` names the text's first line where none is open.
    fn locator(&self, s: &Stored, start: u64) -> Option<SpanLocator> {
        let outline = match &s.places {
            Places::Turns(o) => return self.turn(o, start),
            Places::Headings(o) => o,
        };
        let heads = outline.heads();
        let open = heads.iter().rposition(|(at, _)| *at <= start);
        let path = |i: usize| SpanLocator::HeadingPath {
            headings: heads[i].1.clone(),
        };
        match open {
            None if self.breaks.locator_invented => Some(SpanLocator::HeadingPath {
                headings: vec![s.text.lines().next()?.trim().to_string()],
            }),
            Some(i) if self.breaks.locator_previous_part => i.checked_sub(1).map(path),
            _ => outline.at(start),
        }
    }

    /// Where `start` stands in a transcript (§2.14): the turn it falls in.
    /// `turn_read_into_previous` names the turn before it;
    /// `whisper_speaker_invented` names a speaker for a Whisper segment.
    fn turn(&self, o: &TurnOutline, start: u64) -> Option<SpanLocator> {
        let b = &self.breaks;
        if !(b.turn_read_into_previous || b.whisper_speaker_invented) {
            return o.at(start);
        }
        let i = o.turns.iter().rposition(|t| t.at <= start)?;
        let t = &o.turns[if b.turn_read_into_previous {
            i.saturating_sub(1)
        } else {
            i
        }];
        let speaker = match o.shape {
            TranscriptShape::WhisperJson if b.whisper_speaker_invented => Some("SPEAKER_00".into()),
            _ => t.speaker.clone(),
        };
        Some(SpanLocator::Timestamp {
            milliseconds: t.milliseconds,
            speaker,
        })
    }

    /// A hit cut from `s` at code points `[start, end)`; `exact_off_by_one`
    /// shifts its span's `exact`.
    fn hit(&self, s: &Stored, start: u64, end: u64, score: f32) -> KnowledgeResult {
        let shift = u64::from(self.breaks.exact_off_by_one);
        let len = s.text.chars().count() as u64;
        let span = Span {
            corpus_id: s.corpus.clone(),
            document: self.record(s),
            start,
            end,
            exact: code_points(&s.text, start + shift, (end + shift).min(len)).unwrap_or_default(),
            prefix: String::new(),
            suffix: String::new(),
            locator: self.locator(s, start),
        };
        KnowledgeResult {
            content: code_points(&s.text, start, end).unwrap_or_default(),
            title: None,
            corpus_id: s.corpus.clone(),
            url: None,
            score,
            metadata: Default::default(),
            chunk_id: None,
            source_doc_id: Some(s.name.clone()),
            custody: None,
            grain: None,
            peer_name: None,
            peer_node_id: None,
            document: Some(self.record(s)),
            document_absent: None,
            span: Some(span),
            span_absent: None,
        }
    }

    fn digest(&self, corpus: &str) -> String {
        let records: Vec<Document> = self
            .texts
            .iter()
            .filter(|s| s.corpus == corpus)
            .map(|s| self.record(s))
            .collect();
        let pre = texts_digest_preimage(records.iter().map(|d| {
            (
                d.text_sha256.as_str(),
                d.source.sha256.as_deref(),
                d.extractor.as_str(),
            )
        }));
        sha256_hex(pre.as_bytes())
    }
}

/// The shelves the fake holds beside the fixture library, as if each had
/// been installed: the v0.6 checks that install their own read them.
fn shelves() -> Vec<Shelf> {
    [
        include_str!("../fixture/headings.json"),
        include_str!("../fixture/transcripts.json"),
        include_str!("../fixture/claims.json"),
        include_str!("../fixture/records.json"),
    ]
    .into_iter()
    .flat_map(|j| {
        let fixture: Value = serde_json::from_str(j).expect("shelf fixture parses");
        let shelf: Shelf = serde_json::from_value(fixture["shelf"].clone()).expect("shelf loads");
        let closed = fixture["closed_corpus_id"].as_str().map(|c| Shelf {
            corpus_id: c.to_string(),
            ..shelf.clone()
        });
        std::iter::once(shelf).chain(closed)
    })
    .collect()
}

/// A running fake: its base URL.
pub struct Running {
    /// `http://127.0.0.1:<port>`.
    pub url: String,
}

/// Start a fake that breaks `breaks`.
pub async fn spawn(breaks: Breaks) -> Running {
    let lib = Library::load().expect("fixture loads");
    let trims = breaks.trims_texts;
    let stored = |corpus: &str, name: &str, source: &str, metadata: &Option<Value>| {
        let text = if trims { source.trim_end() } else { source };
        Stored {
            corpus: corpus.to_string(),
            name: name.to_string(),
            source_sha256: sha256_hex(source.as_bytes()),
            text: text.to_string(),
            metadata: metadata.clone(),
            places: Places::Headings(HeadingOutline::of(text)),
        }
    };
    let mut texts: Vec<Stored> = lib
        .documents
        .iter()
        .map(|d| stored(&lib.corpus_id, &d.name, &d.text, &d.metadata))
        .collect();
    let mut failed = Vec::new();
    for shelf in shelves() {
        for d in &shelf.documents {
            let mut s = stored(&shelf.corpus_id, &d.name, &d.text, &d.metadata);
            if d.format.as_deref() == Some("transcript") {
                match TurnOutline::read(&d.text) {
                    Some(o) => {
                        s.text = o.text.clone();
                        s.places = Places::Turns(o);
                    }
                    None if breaks.non_transcript_stored => {}
                    None => {
                        failed.push((s.corpus, s.name, NOT_A_TRANSCRIPT));
                        continue;
                    }
                }
            }
            texts.push(s);
        }
    }
    let fake = Arc::new(Fake {
        breaks,
        corpus: lib.corpus_id.clone(),
        texts,
        failed,
        installed: Mutex::new(BTreeMap::new()),
        allowed: Mutex::new(BTreeSet::new()),
        resolved: records::resolver_table(),
        polls: Mutex::new(BTreeMap::new()),
    });
    let app = Router::new()
        .route("/oicp/v1/capabilities", get(capabilities))
        .route(
            "/v1/models",
            get(|| async { Json(json!({"object": "list", "data": []})) }),
        )
        .route("/v1/knowledge/search", post(search))
        .route("/oicp/v1/text/{sha}", get(text))
        .route("/oicp/v1/align", post(align))
        .route("/oicp/v1/verify", post(verify::verify))
        .route(
            "/oicp/v1/verify/correction",
            post(verify::verify_correction),
        )
        .route("/oicp/v1/corpus/install", ingest::install_route(&fake))
        .route("/oicp/v1/corpus/progress", get(ingest::progress))
        .route("/oicp/v1/recipe/test", post(ingest::recipe_test))
        .route(
            "/mcp",
            post(|| async { Json(json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": []}})) }),
        )
        .layer(middleware::from_fn_with_state(fake.clone(), guard))
        .layer(axum::extract::DefaultBodyLimit::max(ingest::SURFACE_LIMIT))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Running { url }
}

// ── Who is asking ───────────────────────────────────────────────────────────

enum Locality {
    Local,
    CrossOrigin(String),
    ForeignHost,
}

fn names_loopback(authority: &str) -> bool {
    let name = authority.rsplit_once(':').map_or(authority, |(n, _)| n);
    matches!(name, "localhost" | "127.0.0.1" | "[::1]")
}

/// §5.2 rule 4, for a peer the test harness always connects from loopback.
fn locality(h: &HeaderMap) -> Locality {
    let header = |k: &str| h.get(k).and_then(|v| v.to_str().ok());
    let host = header("host");
    if host.is_some_and(|x| !names_loopback(x)) {
        return Locality::ForeignHost;
    }
    if let Some(origin) = header("origin") {
        let authority = origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"));
        if authority.is_none() || authority != host {
            return Locality::CrossOrigin(origin.to_string());
        }
    }
    match header("sec-fetch-site") {
        None | Some("same-origin") | Some("none") => Locality::Local,
        Some(site) => Locality::CrossOrigin(format!("sec-fetch-site: {site}")),
    }
}

/// The credential form of §5.1: `svrn_` and 64 lowercase hex.
fn in_form(b: &str) -> bool {
    b.strip_prefix("svrn_")
        .is_some_and(oicp_types::evidence::is_sha256_hex)
}

fn refuse(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

async fn guard(State(f): State<Arc<Fake>>, req: Request, next: Next) -> Response {
    if f.breaks.permissive_cors && req.method() == Method::OPTIONS {
        let mut r = StatusCode::OK.into_response();
        r.headers_mut()
            .insert("access-control-allow-origin", HeaderValue::from_static("*"));
        return r;
    }
    let mut resp = if req.uri().path() == "/oicp/v1/capabilities" {
        next.run(req).await
    } else {
        admit(&f, req, next).await
    };
    if f.breaks.permissive_cors {
        resp.headers_mut()
            .insert("access-control-allow-origin", HeaderValue::from_static("*"));
    }
    resp
}

async fn admit(f: &Fake, req: Request, next: Next) -> Response {
    let loc = if f.breaks.address_only_locality {
        Locality::Local
    } else {
        locality(req.headers())
    };
    if f.breaks.loopback_first && matches!(loc, Locality::Local) {
        return next.run(req).await;
    }
    let bearer = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    match bearer {
        Some(b) if in_form(&b) && b == named_token() => next.run(req).await,
        Some(b) if in_form(&b) => refuse(
            StatusCode::UNAUTHORIZED,
            json!({"error": "invalid credential"}),
        ),
        _ => match loc {
            Locality::Local => next.run(req).await,
            Locality::CrossOrigin(origin) => refuse(
                StatusCode::FORBIDDEN,
                json!({"error": "cross-origin", "origin": origin}),
            ),
            Locality::ForeignHost => {
                refuse(StatusCode::UNAUTHORIZED, json!({"error": "missing bearer"}))
            }
        },
    }
}

// ── Routes ──────────────────────────────────────────────────────────────────

async fn capabilities(State(f): State<Arc<Fake>>) -> Json<Value> {
    Json(json!({
        "oicp_version": "0.4.0",
        "models": [],
        "features": [
            "ingest:v1", "ingest:recipe_test", "ingest:recipe", "ingest:documents", "ingest:lookups", "evidence:text",
            "evidence:align", "evidence:verify", "evidence:verify_corrections", "evidence:verify_sentences", "knowledge:document", "knowledge:scoped",
            "knowledge:range", "knowledge:sentences", "knowledge:locator", "ingest:transcripts", "auth:named_client"
        ],
        "knowledge": {
            "corpora": [],
            "search_endpoint": "/v1/knowledge/search",
            "sentence_scorer": f.scorer(),
            "ingest": {
                "install_endpoint": "/oicp/v1/corpus/install",
                "progress_endpoint": "/oicp/v1/corpus/progress",
                "test_endpoint": "/oicp/v1/recipe/test",
                "max_install_bytes": ingest::MAX_INSTALL_BYTES
            },
            "evidence": {
                "text_endpoint": "/oicp/v1/text",
                "align_endpoint": "/oicp/v1/align",
                "verify_endpoint": "/oicp/v1/verify",
                "verify_tiers": ["fast", "checker"],
                "verify_correction_endpoint": "/oicp/v1/verify/correction"
            }
        }
    }))
}

/// The lawful search: a text is a hit when it holds the query, or any of the
/// query's words of four letters or more, so a query of several sentences
/// reaches several texts (`knowledge.scoped`). Each hit is its whole text, so
/// its span is `[0, len)`; `exact_off_by_one` shifts it, `scope_leaks`
/// ignores `documents`, and a malformed name is 400 as §2.8 says. At
/// `granularity: sentence` it answers [`sentences`].
async fn search(State(f): State<Arc<Fake>>, Json(req): Json<Value>) -> Response {
    let query = req.get("query").and_then(Value::as_str).unwrap_or("");
    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| w.chars().count() >= 4)
        .collect();
    let strings = |key: &str| -> Vec<String> {
        req.get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    };
    let (documents, corpora) = (strings("documents"), strings("corpora"));
    if documents
        .iter()
        .any(|d| !oicp_types::evidence::is_sha256_hex(d))
    {
        return refuse(StatusCode::BAD_REQUEST, json!({"results": []}));
    }
    if req.get("granularity").and_then(Value::as_str) == Some("sentence") {
        let limit = req.get("limit").and_then(Value::as_u64);
        let limit = limit.unwrap_or(KnowledgeSearchRequest::DEFAULT_LIMIT.into()) as usize;
        return sentences::search(&f, query, &corpora, &documents, limit);
    }
    let hits: Vec<KnowledgeResult> = f
        .of(&corpora)
        .filter(|s| {
            let lower = s.text.to_lowercase();
            s.text.contains(query) || words.iter().any(|w| lower.contains(w.as_str()))
        })
        .filter(|s| f.in_scope(&documents, s))
        .map(|s| f.hit(s, 0, s.text.chars().count() as u64, 1.0))
        .collect();
    Json(json!({"results": hits, "corpora_searched": f.searched(&corpora)})).into_response()
}

async fn text(
    State(f): State<Arc<Fake>>,
    Path(sha): Path<String>,
    Query(raw): Query<BTreeMap<String, String>>,
) -> Response {
    // `corpus` is a hint this fake, holding one corpus, does not need.
    let q: BTreeMap<&str, u64> = raw
        .iter()
        .filter_map(|(k, v)| v.parse().ok().map(|n| (k.as_str(), n)))
        .collect();
    let Some(s) = f
        .texts
        .iter()
        .find(|s| sha256_hex(s.text.as_bytes()) == sha)
    else {
        return refuse(
            StatusCode::NOT_FOUND,
            json!({"error": reasons::TEXT_NOT_HELD}),
        );
    };
    let len = s.text.chars().count() as u64;
    let ranged = q.contains_key("start") || q.contains_key("end");
    let (start, end) = (
        q.get("start").copied().unwrap_or(0),
        q.get("end").copied().unwrap_or(len),
    );
    let Some(slice) = code_points(&s.text, start, end) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            json!({"error": reasons::RANGE_OUTSIDE_TEXT}),
        );
    };
    let ctx = if ranged {
        q.get("context").copied().unwrap_or(DEFAULT_CONTEXT as u64)
    } else {
        0
    };
    let before = code_points(&s.text, start.saturating_sub(ctx), start).unwrap_or_default();
    let after = code_points(&s.text, end, (end + ctx).min(len)).unwrap_or_default();
    Json(TextSlice {
        document: f.record(s),
        start,
        end,
        text: slice,
        before,
        after,
    })
    .into_response()
}

fn kind(k: quote_align::QuoteEditKind) -> DifferenceKind {
    match k {
        quote_align::QuoteEditKind::Substituted => DifferenceKind::Substituted,
        quote_align::QuoteEditKind::Added => DifferenceKind::Added,
        quote_align::QuoteEditKind::Omitted => DifferenceKind::Omitted,
        quote_align::QuoteEditKind::Elided => DifferenceKind::Elided,
        quote_align::QuoteEditKind::Bracketed => DifferenceKind::Bracketed,
    }
}

async fn align(State(f): State<Arc<Fake>>, Json(req): Json<AlignRequest>) -> Json<Value> {
    let cfg = quote_align::AlignConfig::shipped().expect("align.toml loads");
    let held: Vec<&Stored> = f.of(&req.corpora).collect();
    let texts: Vec<&str> = held.iter().map(|s| s.text.as_str()).collect();
    let found = quote_align::align(&req.quote, &texts, &cfg);
    let ctx = u64::from(req.effective_context());
    let alignments: Vec<Alignment> = found
        .alignments
        .iter()
        .map(|a| {
            let s = held[a.text];
            let (start, end) = (a.source.start as u64, a.source.end as u64);
            let shift = u64::from(f.breaks.exact_off_by_one);
            Alignment {
                span: Span {
                    corpus_id: s.corpus.clone(),
                    document: f.record(s),
                    start,
                    end,
                    exact: code_points(&s.text, start + shift, end + shift).unwrap_or_default(),
                    prefix: code_points(&s.text, start.saturating_sub(ctx), start)
                        .unwrap_or_default(),
                    suffix: code_points(&s.text, end, end + ctx).unwrap_or_default(),
                    locator: f.locator(s, start),
                },
                differences: a
                    .edits
                    .iter()
                    .map(|e| Difference {
                        kind: kind(e.kind),
                        quote: [e.quote.start as u64, e.quote.end as u64],
                        source: [e.source.start as u64, e.source.end as u64],
                    })
                    .collect(),
                coverage: a.coverage,
            }
        })
        .collect();
    let corpora: Vec<Value> = f
        .searched(&req.corpora)
        .into_iter()
        .map(|c| {
            if f.breaks.omit_texts_digest {
                json!({"corpus_id": c})
            } else {
                json!({"corpus_id": c, "texts_digest": f.digest(&c)})
            }
        })
        .collect();
    Json(json!({
        "alignments": alignments,
        "aligner": quote_align::ALIGNER_ID,
        "corpora": corpora,
    }))
}
