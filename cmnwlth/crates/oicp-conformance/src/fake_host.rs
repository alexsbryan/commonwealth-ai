// SPDX-License-Identifier: AGPL-3.0-or-later
//! A host fake for the v0.5 checks (oicp-v0.5.md §6): a lawful host that
//! holds the fixture library, and one switch per law that breaks exactly
//! that law. Every v0.5 check is watched red against its switch.
//!
//! The lawful fake aligns with the reference aligner (`quote-align`) and
//! decides locality by the rule of §5.2, so a green run against it is the
//! checks agreeing with the spec, not with a lookup table.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use oicp_types::evidence::{reasons, texts_digest_preimage, DEFAULT_CONTEXT};
use oicp_types::knowledge::reasons::SCORER_UNAVAILABLE;
use oicp_types::{
    AlignRequest, Alignment, Difference, DifferenceKind, Document, KnowledgeResult,
    KnowledgeSearchRequest, SourceRef, Span, TextSlice,
};
use serde_json::{json, Value};

use crate::fixture::{code_points, sha256_hex, Library};

mod ingest;

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
    /// knowledge.sentences: each sentence's range is one code point past
    /// the sentence its `exact` and `content` carry.
    pub sentence_range_off_by_one: bool,
    /// ingest.install_limit: the manifest states `max_install_bytes` and the
    /// install route reads bodies only to the surface's 8 MiB.
    pub install_at_surface_limit: bool,
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
    name: String,
    text: String,
    metadata: Option<Value>,
}

struct Fake {
    breaks: Breaks,
    corpus: String,
    texts: Vec<Stored>,
    installed: Mutex<BTreeMap<String, String>>,
    /// Polls of progress since each corpus's last install.
    polls: Mutex<BTreeMap<String, u32>>,
}

impl Fake {
    fn record(&self, s: &Stored) -> Document {
        let sha = sha256_hex(s.text.as_bytes());
        Document {
            text_sha256: sha.clone(),
            extractor: "plaintext@fake".into(),
            source: SourceRef {
                id: s.name.clone(),
                sha256: Some(sha),
            },
            metadata: if self.breaks.empty_metadata {
                None
            } else {
                s.metadata.clone()
            },
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

    /// A hit cut from `s` at code points `[start, end)`; `exact_off_by_one`
    /// shifts its span's `exact`.
    fn hit(&self, s: &Stored, start: u64, end: u64, score: f32) -> KnowledgeResult {
        let shift = u64::from(self.breaks.exact_off_by_one);
        let len = s.text.chars().count() as u64;
        let span = Span {
            corpus_id: self.corpus.clone(),
            document: self.record(s),
            start,
            end,
            exact: code_points(&s.text, start + shift, (end + shift).min(len)).unwrap_or_default(),
            prefix: String::new(),
            suffix: String::new(),
        };
        KnowledgeResult {
            content: code_points(&s.text, start, end).unwrap_or_default(),
            title: None,
            corpus_id: self.corpus.clone(),
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

    fn digest(&self) -> String {
        let records: Vec<Document> = self.texts.iter().map(|s| self.record(s)).collect();
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

/// A running fake: its base URL.
pub struct Running {
    /// `http://127.0.0.1:<port>`.
    pub url: String,
}

/// Start a fake that breaks `breaks`.
pub async fn spawn(breaks: Breaks) -> Running {
    let lib = Library::load().expect("fixture loads");
    let fake = Arc::new(Fake {
        breaks,
        corpus: lib.corpus_id.clone(),
        texts: lib
            .documents
            .iter()
            .map(|d| Stored {
                name: d.name.clone(),
                text: d.text.clone(),
                metadata: d.metadata.clone(),
            })
            .collect(),
        installed: Mutex::new(BTreeMap::new()),
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
        .route("/oicp/v1/verify", post(verify))
        .route("/oicp/v1/verify/correction", post(verify_correction))
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
            "ingest:v1", "ingest:recipe_test", "ingest:recipe", "ingest:documents", "evidence:text",
            "evidence:align", "evidence:verify", "evidence:verify_corrections", "knowledge:document", "knowledge:scoped",
            "knowledge:range", "knowledge:sentences", "auth:named_client"
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
    let documents: Vec<String> = req
        .get("documents")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    if documents
        .iter()
        .any(|d| !oicp_types::evidence::is_sha256_hex(d))
    {
        return refuse(StatusCode::BAD_REQUEST, json!({"results": []}));
    }
    if req.get("granularity").and_then(Value::as_str) == Some("sentence") {
        let limit = req.get("limit").and_then(Value::as_u64);
        let limit = limit.unwrap_or(KnowledgeSearchRequest::DEFAULT_LIMIT.into()) as usize;
        return sentences(&f, query, &documents, limit);
    }
    let hits: Vec<KnowledgeResult> = f
        .texts
        .iter()
        .filter(|s| {
            let lower = s.text.to_lowercase();
            s.text.contains(query) || words.iter().any(|w| lower.contains(w.as_str()))
        })
        .filter(|s| f.in_scope(&documents, s))
        .map(|s| f.hit(s, 0, s.text.chars().count() as u64, 1.0))
        .collect();
    Json(json!({"results": hits, "corpora_searched": [f.corpus]})).into_response()
}

/// The lawful sentence search (§2.11), every sentence indexed: each scoped
/// text's sentences (`.`, `!` or `?` before whitespace or the end, trimmed)
/// scored by cosine over word counts, best first, at most `limit`, those
/// sharing no word with the query left out. `rerank_without_slot` refuses by
/// name; `rerank_falls_back` answers with cosine anyway.
fn sentences(f: &Fake, query: &str, documents: &[String], limit: usize) -> Response {
    if f.breaks.rerank_without_slot && !f.breaks.rerank_falls_back {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": SCORER_UNAVAILABLE, "scorer": "rerank"}),
        );
    }
    let q = word_counts(query);
    let mut found: Vec<(f32, &Stored, u64, u64)> = Vec::new();
    for s in f.texts.iter().filter(|s| f.in_scope(documents, s)) {
        let chars: Vec<char> = s.text.chars().collect();
        let mut start = 0;
        for i in 0..chars.len() {
            let stop = matches!(chars[i], '.' | '!' | '?')
                && chars.get(i + 1).is_none_or(|c| c.is_whitespace());
            if !stop && i + 1 < chars.len() {
                continue;
            }
            let (mut a, mut b) = (start, i + 1);
            start = i + 1;
            while a < b && chars[a].is_whitespace() {
                a += 1;
            }
            while b > a && chars[b - 1].is_whitespace() {
                b -= 1;
            }
            let sentence: String = chars[a..b].iter().collect();
            let score = cosine(&q, &word_counts(&sentence));
            if score > 0.0 {
                found.push((score, s, a as u64, b as u64));
            }
        }
    }
    found.sort_by(|x, y| y.0.total_cmp(&x.0));
    found.truncate(limit);
    let hits: Vec<KnowledgeResult> = found
        .into_iter()
        .map(|(score, s, a, b)| {
            let mut hit = f.hit(s, a, b, score);
            if let (true, Some(span)) = (f.breaks.sentence_range_off_by_one, hit.span.as_mut()) {
                (span.start, span.end) = (a + 1, b + 1);
            }
            hit
        })
        .collect();
    Json(json!({
        "results": hits, "corpora_searched": [f.corpus],
        "scorer": {"name": "cosine", "model": "fake-word-counts"}, "route": "index",
    }))
    .into_response()
}

fn word_counts(s: &str) -> BTreeMap<String, f32> {
    let mut m = BTreeMap::new();
    for w in s
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        *m.entry(w.to_lowercase()).or_insert(0.0) += 1.0;
    }
    m
}

fn cosine(a: &BTreeMap<String, f32>, b: &BTreeMap<String, f32>) -> f32 {
    let dot: f32 = a.iter().map(|(w, n)| n * b.get(w).unwrap_or(&0.0)).sum();
    let norm = |m: &BTreeMap<String, f32>| m.values().map(|n| n * n).sum::<f32>().sqrt();
    if dot == 0.0 {
        0.0
    } else {
        dot / (norm(a) * norm(b))
    }
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
    let texts: Vec<&str> = f.texts.iter().map(|s| s.text.as_str()).collect();
    let found = quote_align::align(&req.quote, &texts, &cfg);
    let ctx = u64::from(req.effective_context());
    let alignments: Vec<Alignment> = found
        .alignments
        .iter()
        .map(|a| {
            let s = &f.texts[a.text];
            let (start, end) = (a.source.start as u64, a.source.end as u64);
            let shift = u64::from(f.breaks.exact_off_by_one);
            Alignment {
                span: Span {
                    corpus_id: f.corpus.clone(),
                    document: f.record(s),
                    start,
                    end,
                    exact: code_points(&s.text, start + shift, end + shift).unwrap_or_default(),
                    prefix: code_points(&s.text, start.saturating_sub(ctx), start)
                        .unwrap_or_default(),
                    suffix: code_points(&s.text, end, end + ctx).unwrap_or_default(),
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
    let corpus = if f.breaks.omit_texts_digest {
        json!({"corpus_id": f.corpus})
    } else {
        json!({"corpus_id": f.corpus, "texts_digest": f.digest()})
    };
    Json(json!({
        "alignments": alignments,
        "aligner": quote_align::ALIGNER_ID,
        "corpora": [corpus],
    }))
}

/// The lawful verify: each sentence of the unit is a claim, `supported` by
/// the stored text it appears in verbatim (that occurrence is its passage),
/// `unsupported` otherwise; the judge tier is not served. Spans take
/// `exact_off_by_one` like align's.
async fn verify(
    State(f): State<Arc<Fake>>,
    Json(req): Json<oicp_types::verify::VerifyRequest>,
) -> Response {
    use oicp_types::verify::{reasons as vr, VerifyTier, UNCALIBRATED};
    if req.tier == VerifyTier::Judge {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": vr::TIER_NOT_OFFERED}),
        );
    }
    let unit: Vec<char> = req.unit.chars().collect();
    let mut claims = Vec::new();
    let mut start = 0usize;
    for i in 0..unit.len() {
        let stop = unit[i] == '.' && unit.get(i + 1).is_none_or(|c| c.is_whitespace());
        if !stop {
            continue;
        }
        let exact: String = unit[start..=i].iter().collect();
        let lead = exact.len() - exact.trim_start().len();
        let (a, b) = ((start + lead) as u64, (i + 1) as u64);
        let exact = exact.trim_start().to_string();
        start = i + 1;
        let found = f.texts.iter().find_map(|s| {
            s.text
                .find(&exact)
                .map(|byte| (s, s.text[..byte].chars().count() as u64))
        });
        let shift = u64::from(f.breaks.exact_off_by_one);
        let (verdict, evidence) = match found {
            Some((s, at)) => {
                let end = at + exact.chars().count() as u64;
                let span = Span {
                    corpus_id: f.corpus.clone(),
                    document: f.record(s),
                    start: at,
                    end,
                    exact: code_points(&s.text, at + shift, end + shift).unwrap_or_default(),
                    prefix: String::new(),
                    suffix: String::new(),
                };
                (
                    json!({"kind": "supported"}),
                    json!([{"corpus_id": f.corpus, "span": span, "verdict": {"kind": "supported"}}]),
                )
            }
            None => (json!({"kind": "unsupported"}), json!([])),
        };
        let verdict = if f.breaks.verify_unknown_verdict {
            json!({"kind": "plausible"})
        } else {
            verdict
        };
        claims.push(json!({
            "start": a, "end": b, "exact": exact, "class": "checkable",
            "verdict": verdict, "evidence": evidence, "rung": "fake/verbatim",
            "operating_point": UNCALIBRATED,
            "episode_id": format!("00000000-0000-4000-8000-{:012x}", claims.len()),
        }));
    }
    Json(json!({
        "claims": claims,
        "verifier": "fake-verify/1",
        "corpora": [{"corpus_id": f.corpus, "texts_digest": f.digest()}],
    }))
    .into_response()
}

/// A UUID's shape: 8-4-4-4-12 hex digits.
fn uuid_shaped(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|g| g.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The lawful correction (v0.6 §2.10): a malformed episode is 400, and the
/// witness is read back against the fake's texts. `witness_unread` calls
/// every witness `verified`.
async fn verify_correction(
    State(f): State<Arc<Fake>>,
    Json(req): Json<oicp_types::verify::VerifyCorrection>,
) -> Response {
    use oicp_types::verify::{reasons as vr, WitnessCheck};
    if !uuid_shaped(&req.episode_id) {
        return refuse(
            StatusCode::BAD_REQUEST,
            json!({"error": vr::NOT_AN_EPISODE}),
        );
    }
    let witness = match &req.witness {
        None => WitnessCheck::NotGiven,
        Some(_) if f.breaks.witness_unread => WitnessCheck::Verified,
        Some(w) => match f
            .texts
            .iter()
            .find(|s| sha256_hex(s.text.as_bytes()) == w.text_sha256)
        {
            None => WitnessCheck::NotHeld,
            Some(s) if w.start <= w.end && w.end <= s.text.chars().count() as u64 => {
                WitnessCheck::Verified
            }
            Some(_) => WitnessCheck::OutOfRange,
        },
    };
    Json(json!({"recorded": true, "witness": witness})).into_response()
}
