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
use oicp_types::{
    AlignRequest, Alignment, Difference, DifferenceKind, Document, KnowledgeResult, SourceRef,
    Span, TextSlice,
};
use serde_json::{json, Value};

use crate::fixture::{code_points, sha256_hex, Library};

/// The laws a fake can break, one switch each (oicp-v0.5.md §6's table).
#[derive(Debug, Clone, Copy, Default)]
pub struct Breaks {
    /// evidence.text: every span's `exact` is one character off its range.
    pub exact_off_by_one: bool,
    /// evidence.align: replies carry no `texts_digest`.
    pub omit_texts_digest: bool,
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
        .route("/oicp/v1/corpus/install", post(install))
        .route("/oicp/v1/corpus/progress", get(progress))
        .route("/oicp/v1/recipe/test", post(recipe_test))
        .route(
            "/mcp",
            post(|| async { Json(json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": []}})) }),
        )
        .layer(middleware::from_fn_with_state(fake.clone(), guard))
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

async fn capabilities() -> Json<Value> {
    Json(json!({
        "oicp_version": "0.4.0",
        "models": [],
        "features": [
            "ingest:v1", "ingest:recipe_test", "ingest:recipe", "evidence:text",
            "evidence:align", "knowledge:document", "auth:named_client"
        ],
        "knowledge": {
            "corpora": [],
            "search_endpoint": "/v1/knowledge/search",
            "ingest": {
                "install_endpoint": "/oicp/v1/corpus/install",
                "progress_endpoint": "/oicp/v1/corpus/progress",
                "test_endpoint": "/oicp/v1/recipe/test"
            },
            "evidence": {"text_endpoint": "/oicp/v1/text", "align_endpoint": "/oicp/v1/align"}
        }
    }))
}

async fn search(State(f): State<Arc<Fake>>, Json(req): Json<Value>) -> Json<Value> {
    let query = req.get("query").and_then(Value::as_str).unwrap_or("");
    let hits: Vec<KnowledgeResult> = f
        .texts
        .iter()
        .filter(|s| s.text.contains(query))
        .map(|s| KnowledgeResult {
            content: s.text.clone(),
            title: None,
            corpus_id: f.corpus.clone(),
            url: None,
            score: 1.0,
            metadata: Default::default(),
            chunk_id: None,
            source_doc_id: Some(s.name.clone()),
            custody: None,
            grain: None,
            peer_name: None,
            peer_node_id: None,
            document: Some(f.record(s)),
        })
        .collect();
    Json(json!({"results": hits, "corpora_searched": [f.corpus]}))
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

async fn install(State(f): State<Arc<Fake>>, Json(req): Json<Value>) -> Response {
    let corpus = req
        .get("corpus_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let Some(recipe) = req.get("recipe_toml").and_then(Value::as_str) else {
        return Json(json!({"corpus_id": corpus, "spawned": false})).into_response();
    };
    if !recipe.contains("[corpus]") {
        if f.breaks.ok_on_bad_recipe {
            return Json(json!({"corpus_id": corpus, "spawned": false})).into_response();
        }
        return refuse(
            StatusCode::BAD_REQUEST,
            json!({"error": "invalid recipe: no [corpus] table"}),
        );
    }
    let sha = sha256_hex(recipe.as_bytes());
    let previous = f
        .installed
        .lock()
        .unwrap()
        .insert(corpus.clone(), sha.clone());
    let spawned = previous.as_deref() != Some(sha.as_str());
    Json(json!({"corpus_id": corpus, "spawned": spawned, "recipe_sha256": sha})).into_response()
}

async fn progress(State(f): State<Arc<Fake>>) -> Json<Value> {
    let installed = f.installed.lock().unwrap();
    let progress: BTreeMap<&String, Value> = installed
        .keys()
        .map(|c| (c, json!({"phase": "complete"})))
        .collect();
    Json(json!({"progress": progress}))
}

async fn recipe_test(State(f): State<Arc<Fake>>) -> Json<Value> {
    let n = f.texts.len();
    let mut stages = vec![
        json!({"name": "validate", "docs_in": 0, "docs_out": 0}),
        json!({"name": "acquire", "docs_in": 0, "docs_out": n}),
        json!({"name": "extract", "docs_in": n, "docs_out": n}),
        json!({"name": "chunk", "docs_in": n, "docs_out": 2 * n}),
    ];
    if f.breaks.drop_extract_stage {
        stages.remove(2);
    }
    Json(json!({"stages": stages, "ok": true}))
}
