//! `sovereign meshapp dev <id>` — a local dev loop for a mesh-app bundle.
//!
//! Serves the bundle's static files + the shared `_sdk/`, and injects a
//! `window.meshApp` shim that proxies the bridge ops over HTTP to the SAME pure
//! functions the desktop host calls ([`sovereign_meshapp`]), reading a local
//! corpus index. So you can iterate on a bundle against real data without
//! launching (or rebuilding) the desktop. Explorer ops only in v1 (the LVT
//! parcel ops still live in the desktop).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    extract::{Json, Path as AxPath, State},
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde::Deserialize;

struct DevCtx {
    index_path: PathBuf,
    bundle_dir: PathBuf,
    sdk_dir: PathBuf,
}

pub async fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("dev") => run_dev(&args[1..]).await,
        _ => {
            eprintln!(
                "usage: sovereign meshapp dev <app-id> [--dir <bundle-dir>] [--index <index-dir>] [--port <n>]\n\n\
                 Serves a mesh-app bundle with a live `window.meshApp` backed by a local corpus,\n\
                 so you can develop the bundle against real data without the desktop app."
            );
            2
        }
    }
}

async fn run_dev(args: &[String]) -> i32 {
    // Parse: <app-id> [--dir D] [--index I] [--port N]
    let mut app_id: Option<String> = None;
    let mut dir: Option<PathBuf> = None;
    let mut index: Option<PathBuf> = None;
    let mut port: u16 = 4317;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                dir = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--index" => {
                index = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--port" => {
                port = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(port);
                i += 2;
            }
            other if app_id.is_none() && !other.starts_with("--") => {
                app_id = Some(other.to_string());
                i += 1;
            }
            other => {
                eprintln!("meshapp dev: unexpected argument `{other}`");
                return 2;
            }
        }
    }
    let Some(app_id) = app_id else {
        eprintln!("meshapp dev: missing <app-id>");
        return 2;
    };

    // Bundle dir: --dir, else the in-repo default (run from repo root).
    let bundle_dir = dir.unwrap_or_else(|| {
        PathBuf::from("sovereign/crates/sovereign-desktop/public/meshapp").join(&app_id)
    });
    if !bundle_dir.join("index.html").is_file() {
        eprintln!(
            "meshapp dev: no index.html in {} — pass --dir <bundle-dir> (or run from the repo root)",
            bundle_dir.display()
        );
        return 1;
    }
    let sdk_dir = bundle_dir
        .parent()
        .map(|p| p.join("_sdk"))
        .unwrap_or_else(|| PathBuf::from("_sdk"));

    // Corpus from the bundle's manifest.
    let corpus = match read_manifest_corpus(&bundle_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("meshapp dev: {e}");
            return 1;
        }
    };

    // Index dir: --index, else ~/.sovereign/indexes/<corpus>.
    let index_path =
        index.unwrap_or_else(|| sovereign_cli_shared::dirs::sovereign_indexes().join(&corpus));
    if !index_path.is_dir() {
        eprintln!(
            "meshapp dev: corpus `{corpus}` not found at {} — install it first (`sovereign corpus install {corpus}`) or pass --index",
            index_path.display()
        );
        return 1;
    }

    let ctx = Arc::new(DevCtx {
        index_path,
        bundle_dir,
        sdk_dir,
    });

    let app = Router::new()
        .route("/__meshapp/{op}", post(op_handler))
        .route("/__meshapp_dev.js", get(shim_handler))
        .fallback(static_handler)
        .with_state(ctx.clone());

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("meshapp dev: bind {addr}: {e} (try --port)");
            return 1;
        }
    };
    println!("meshapp dev: serving `{app_id}` over corpus `{corpus}`");
    println!("  bundle : {}", ctx.bundle_dir.display());
    println!("  index  : {}", ctx.index_path.display());
    println!("  open   : http://{addr}/   (Ctrl-C to stop)");
    if let Err(e) = axum::serve(listener, app.into_make_service()).await {
        eprintln!("meshapp dev: server error: {e}");
        return 1;
    }
    0
}

fn read_manifest_corpus(bundle_dir: &Path) -> Result<String, String> {
    let p = bundle_dir.join("meshapp.json");
    let bytes = std::fs::read(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
    #[derive(Deserialize)]
    struct Manifest {
        corpus: String,
    }
    let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", p.display()))?;
    Ok(m.corpus)
}

/// The args a bundle can pass — a superset across ops; all optional.
#[derive(Deserialize, Default)]
struct OpArgs {
    #[serde(default)]
    node_type: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    chunk_id: Option<String>,
}

async fn op_handler(
    AxPath(op): AxPath<String>,
    State(ctx): State<Arc<DevCtx>>,
    Json(a): Json<OpArgs>,
) -> Response {
    let idx = ctx.index_path.as_path();
    let result: Result<serde_json::Value, String> = match op.as_str() {
        "graph" => sovereign_meshapp::load_graph(idx)
            .map(|g| to_val(sovereign_meshapp::graph_nodes(&g, a.node_type.as_deref(), a.limit.unwrap_or(50).min(500)))),
        "subgraph" => sovereign_meshapp::load_graph(idx)
            .map(|g| to_val(sovereign_meshapp::subgraph(&g, a.node_type.as_deref(), a.limit.unwrap_or(30).min(80)))),
        "node" => sovereign_meshapp::load_graph(idx)
            .and_then(|g| sovereign_meshapp::node_detail(&g, a.id.as_deref().unwrap_or_default()).map(to_val)),
        "findings" => sovereign_meshapp::load_graph(idx)
            .map(|g| to_val(sovereign_meshapp::findings(&g, a.pattern.as_deref()))),
        "search" => sovereign_meshapp::load_graph(idx).map(|g| {
            to_val(sovereign_meshapp::search_entities(&g, a.query.as_deref().unwrap_or_default(), a.node_type.as_deref(), a.limit.unwrap_or(25).min(100)))
        }),
        "reconciliation" => Ok(to_val(sovereign_meshapp::reconciliation(idx))),
        "corpus_stats" => Ok(to_val(sovereign_meshapp::corpus_stats(idx))),
        "timeline" => sovereign_meshapp::timeline(idx).await.map(to_val),
        "read_chunk" => match a.chunk_id.as_deref().unwrap_or_default().trim().parse::<u64>() {
            Ok(id) => sovereign_meshapp::read_chunk(idx, id).await.map(to_val),
            Err(_) => Err(format!("chunk id is not numeric: {:?}", a.chunk_id)),
        },
        other => Err(format!("unknown op `{other}`")),
    };
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

fn to_val<T: serde::Serialize>(v: T) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

async fn shim_handler() -> Response {
    ([(header::CONTENT_TYPE, "text/javascript")], DEV_SHIM).into_response()
}

async fn static_handler(State(ctx): State<Arc<DevCtx>>, uri: Uri) -> Response {
    let path = uri.path();
    // `/_sdk/x` → the shared SDK; everything else → the bundle dir.
    if let Some(rel) = path.strip_prefix("/_sdk/") {
        return serve_file(&ctx.sdk_dir.join(rel), false);
    }
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let inject = rel == "index.html";
    serve_file(&ctx.bundle_dir.join(rel), inject)
}

fn serve_file(file: &Path, inject_shim: bool) -> Response {
    let bytes = match std::fs::read(file) {
        Ok(b) => b,
        Err(_) => return (StatusCode::NOT_FOUND, format!("not found: {}", file.display())).into_response(),
    };
    let ct = content_type(file);
    if inject_shim {
        let html = String::from_utf8_lossy(&bytes);
        let tag = "<script src=\"/__meshapp_dev.js\"></script>";
        let injected = if let Some(idx) = html.find("</head>") {
            format!("{}{}{}", &html[..idx], tag, &html[idx..])
        } else {
            format!("{tag}{html}")
        };
        return ([(header::CONTENT_TYPE, ct)], injected).into_response();
    }
    ([(header::CONTENT_TYPE, ct)], bytes).into_response()
}

fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

/// The dev `window.meshApp`: same method surface as `meshapp_shim.js`, but over
/// `fetch('/__meshapp/<op>')` instead of Tauri IPC. The corpus id the bundle
/// passes is ignored — the server is bound to one corpus (the bundle's). LVT
/// parcel ops aren't proxied in v1.
const DEV_SHIM: &str = r#"(function () {
  const call = async (op, args) => {
    const r = await fetch('/__meshapp/' + op, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify(args || {}),
    });
    const t = await r.text();
    if (!r.ok) throw new Error(t || ('meshapp dev: ' + op + ' failed'));
    return t ? JSON.parse(t) : null;
  };
  window.meshApp = {
    capabilities: async () => ({ mesh_store_read: true, mesh_store_write: false, inference_access: false, knowledge_access: false }),
    graph: (c, nodeType, limit) => call('graph', { node_type: nodeType, limit }),
    subgraph: (c, nodeType, limit) => call('subgraph', { node_type: nodeType, limit }),
    node: (c, id) => call('node', { id }),
    findings: (c, pattern) => call('findings', { pattern }),
    searchEntities: (c, query, nodeType, limit) => call('search', { query, node_type: nodeType, limit }),
    reconciliation: (c) => call('reconciliation', {}),
    corpusStats: (c) => call('corpus_stats', {}),
    timeline: (c) => call('timeline', {}),
    readChunk: (c, chunkId) => call('read_chunk', { chunk_id: String(chunkId) }),
  };
})();
"#;
