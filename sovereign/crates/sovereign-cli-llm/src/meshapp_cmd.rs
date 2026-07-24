// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn meshapp dev <id>` — a local dev loop for a mesh-app bundle.
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
        Some("new") => run_new(&args[1..]),
        Some("dev") => run_dev(&args[1..]).await,
        Some("publish") => crate::meshapp_registry::publish(&args[1..]),
        Some("install") => crate::meshapp_registry::install(&args[1..]).await,
        Some("list") => crate::meshapp_registry::list(&args[1..]),
        _ => {
            eprintln!(
                "usage:\n\
                 \x20 svrn meshapp new <id> --corpus <corpus-id> [--name <title>] [--dir <base>]\n\
                 \x20 svrn meshapp dev <id> [--dir <bundle-dir>] [--index <index-dir>] [--port <n>]\n\
                 \x20 svrn meshapp publish <id> [--dir <bundle-dir>] [--out <dir>]\n\
                 \x20 svrn meshapp install <id> [--from <path|url>]\n\
                 \x20 svrn meshapp list\n\n\
                 new      scaffold an SDK-composed bundle (index.html + app.js + meshapp.json).\n\
                 dev      serve a bundle with a live `window.meshApp` over a local corpus.\n\
                 publish  pack a bundle (+ _sdk) into a tar.zst + register it.\n\
                 install  fetch + verify + unpack an app into ~/.sovereign/meshapps/.\n\
                 list     show the registry + installed apps."
            );
            2
        }
    }
}

fn run_new(args: &[String]) -> i32 {
    let mut app_id: Option<String> = None;
    let mut corpus: Option<String> = None;
    let mut name: Option<String> = None;
    let mut base: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus" => {
                corpus = args.get(i + 1).cloned();
                i += 2;
            }
            "--name" => {
                name = args.get(i + 1).cloned();
                i += 2;
            }
            "--dir" => {
                base = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            other if app_id.is_none() && !other.starts_with("--") => {
                app_id = Some(other.to_string());
                i += 1;
            }
            other => {
                eprintln!("meshapp new: unexpected argument `{other}`");
                return 2;
            }
        }
    }
    let (Some(app_id), Some(corpus)) = (app_id, corpus) else {
        eprintln!("meshapp new: need <app-id> and --corpus <corpus-id>");
        return 2;
    };
    if !app_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        eprintln!("meshapp new: app-id must be a slug (a-z 0-9 - _)");
        return 2;
    }
    let name = name.unwrap_or_else(|| title_case(&app_id));
    let base =
        base.unwrap_or_else(|| PathBuf::from("sovereign/crates/sovereign-desktop/public/meshapp"));
    let dir = base.join(&app_id);
    if dir.exists() {
        eprintln!("meshapp new: {} already exists", dir.display());
        return 1;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("meshapp new: create {}: {e}", dir.display());
        return 1;
    }
    let render = |t: &str| {
        t.replace("{{ID}}", &app_id)
            .replace("{{NAME}}", &name)
            .replace("{{CORPUS}}", &corpus)
    };
    for (file, tmpl) in [
        ("index.html", STARTER_INDEX_HTML),
        ("app.js", STARTER_APP_JS),
        ("meshapp.json", STARTER_MANIFEST),
    ] {
        if let Err(e) = std::fs::write(dir.join(file), render(tmpl)) {
            eprintln!("meshapp new: write {file}: {e}");
            return 1;
        }
    }
    println!("scaffolded mesh app `{app_id}` → {}", dir.display());
    println!("  next:");
    println!("    svrn meshapp dev {app_id}      # serve it against your local `{corpus}` corpus");
    println!("  then, to ship it so others can one-click the data:");
    println!("    1. publish your corpus snapshot:  svrn corpus snapshot publish {corpus}");
    println!("    2. copy your recipe into the bundle as recipe.toml (it carries the [prebuilt] HF block)");
    println!("    3. add to meshapp.json:  \"corpus_data\": {{ \"size_indexed_gb\": <n>, \"recipe\": \"recipe.toml\" }}");
    0
}

fn title_case(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
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

    // Bundle dir: --dir, else the in-repo default (run from repo root), else an
    // installed app under ~/.sovereign/meshapps/<id>/.
    let bundle_dir = match dir {
        Some(d) => d,
        None => {
            let in_repo =
                PathBuf::from("sovereign/crates/sovereign-desktop/public/meshapp").join(&app_id);
            if in_repo.join("index.html").is_file() {
                in_repo
            } else {
                sovereign_cli_shared::dirs::sovereign_meshapps().join(&app_id)
            }
        }
    };
    if !bundle_dir.join("index.html").is_file() {
        eprintln!(
            "meshapp dev: no index.html in {} — `svrn meshapp install {app_id}` first, or pass --dir",
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
            "meshapp dev: corpus `{corpus}` not found at {} — install it first (`svrn corpus install {corpus}`) or pass --index",
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
    let m: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", p.display()))?;
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
        "graph" => sovereign_meshapp::load_graph(idx).map(|g| {
            to_val(sovereign_meshapp::graph_nodes(
                &g,
                a.node_type.as_deref(),
                a.limit.unwrap_or(50).min(500),
            ))
        }),
        "subgraph" => sovereign_meshapp::load_graph(idx).map(|g| {
            to_val(sovereign_meshapp::subgraph(
                &g,
                a.node_type.as_deref(),
                a.limit.unwrap_or(30).min(80),
            ))
        }),
        "node" => sovereign_meshapp::load_graph(idx).and_then(|g| {
            sovereign_meshapp::node_detail(&g, a.id.as_deref().unwrap_or_default()).map(to_val)
        }),
        "findings" => sovereign_meshapp::load_graph(idx)
            .map(|g| to_val(sovereign_meshapp::findings(&g, a.pattern.as_deref()))),
        "search" => sovereign_meshapp::load_graph(idx).map(|g| {
            to_val(sovereign_meshapp::search_entities(
                &g,
                a.query.as_deref().unwrap_or_default(),
                a.node_type.as_deref(),
                a.limit.unwrap_or(25).min(100),
            ))
        }),
        "document_feed" => {
            let limit = a.limit.unwrap_or(14).clamp(1, 90);
            sovereign_meshapp::document_feed(idx, limit)
                .await
                .map(to_val)
        }
        "reconciliation" => Ok(to_val(sovereign_meshapp::reconciliation(idx))),
        "corpus_stats" => Ok(to_val(sovereign_meshapp::corpus_stats(idx))),
        "timeline" => sovereign_meshapp::timeline(idx).await.map(to_val),
        "read_chunk" => match a
            .chunk_id
            .as_deref()
            .unwrap_or_default()
            .trim()
            .parse::<u64>()
        {
            Ok(id) => sovereign_meshapp::read_chunk(idx, id).await.map(to_val),
            Err(_) => Err(format!("chunk id is not numeric: {:?}", a.chunk_id)),
        },
        // Same load-or-build-and-cache path the desktop host runs, so the
        // dev loop exercises staleness + the verbatim audit identically.
        "wrapped_artifact" => {
            let state_db = sovereign_cli_shared::dirs::sovereign_root().join("sovereign.db");
            sovereign_meshapp::wrapped::wrapped_artifact(idx, Some(state_db.as_path()))
                .await
                .map(to_val)
        }
        // Host navigation has no dev-server analogue — the bundle catches
        // this and renders its fallback copy.
        "open_outer_work" => Err(
            "Outer Work lives in the desktop app — the dev server has no chat to open".to_string(),
        ),
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
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                format!("not found: {}", file.display()),
            )
                .into_response()
        }
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
        Some("woff2") => "font/woff2",
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
    documentFeed: (c, limitDocs) => call('document_feed', { limit: limitDocs }),
    wrappedArtifact: (c) => call('wrapped_artifact', {}),
    openOuterWork: (c) => call('open_outer_work', {}),
  };
})();
"#;

// ─── `meshapp new` scaffold templates ────────────────────────────────

const STARTER_INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>{{NAME}}</title>
    <link rel="stylesheet" href="../_sdk/meshapp.css" />
  </head>
  <body>
    <main>
      <h1>{{NAME}}</h1>
      <p class="sub">
        An explorer over the <span class="src">{{CORPUS}}</span> corpus — every
        link cites its source. <span class="src" id="source"></span>
      </p>

      <section id="loading">Loading…</section>

      <section id="app" hidden>
        <div id="banner" class="banner"></div>

        <div class="card">
          <div class="label">The graph <span class="chip">atlas edges</span></div>
          <div id="map-toggle" class="toggle"></div>
          <div id="map" class="map"></div>
          <div id="map-msg" class="meta"></div>
        </div>

        <div class="card">
          <div class="label">Find anything <span class="chip">search</span></div>
          <div id="search-host"></div>
        </div>

        <div id="detail" class="card" hidden></div>
      </section>

      <section id="error" hidden class="error"></section>
    </main>
    <script type="module" src="./app.js"></script>
  </body>
</html>
"#;

const STARTER_APP_JS: &str = r#"// {{NAME}} — scaffolded by `svrn meshapp new`.
// Edit me, then run `svrn meshapp dev {{ID}}` to see changes against real
// data. Everything is composed from the MeshApp SDK; the only host channel is
// the permission-gated `window.meshApp` bridge (no inference).
import {
  $, connect, hasBridge, emsg, fmtInt,
  scaleBanner, typeToggle, forceGraph, searchBox, entityDetail,
} from "../_sdk/meshapp.js";

const CORPUS = "{{CORPUS}}";
let bridge;

async function main() {
  if (!hasBridge()) return fail("window.meshApp is not available.");
  bridge = connect(CORPUS);
  $("source").textContent = "Source: " + CORPUS;
  try {
    await bridge.subgraph(null, 1); // probe — fails if the corpus isn't present/granted
  } catch (e) {
    return fail("Bridge failed: " + emsg(e) + "  (is `" + CORPUS + "` installed and granted?)");
  }
  $("loading").hidden = true;
  $("app").hidden = false;

  loadBanner();
  loadMap(null);
  typeToggle($("map-toggle"), [
    { type: "all", label: "All" },
    { type: "person", label: "People" },
    { type: "institution", label: "Orgs" },
  ], { initial: "all", onChange: loadMap });
  searchBox($("search-host"), bridge, { placeholder: "search…", ariaLabel: "Search", onPick: openEntity });
}

async function loadBanner() {
  let s;
  try { s = await bridge.corpusStats(); } catch { return; }
  scaleBanner($("banner"), [
    { num: fmtInt(s.entities), cap: "entities" },
    { num: fmtInt(s.edges), cap: "relationships" },
    { num: fmtInt(s.claims), cap: "claims" },
    { num: fmtInt(s.documents), cap: "documents" },
  ]);
}

async function loadMap(type) {
  const msg = $("map-msg");
  msg.textContent = "";
  let g;
  try { g = await bridge.subgraph(type, 40); } catch (e) { msg.textContent = "map failed: " + emsg(e); return; }
  if (!(g.nodes || []).length) { $("map").replaceChildren(); msg.textContent = "no graph for this type."; return; }
  forceGraph($("map"), g, { onNodeClick: openEntity });
  msg.textContent = g.nodes.length + " nodes · " + (g.edges || []).length + " links · drag, click to open";
}

async function openEntity(id) {
  let node;
  try { node = await bridge.node(id); } catch (e) { return; }
  entityDetail($("detail"), node, { bridge, onOpen: openEntity, citationLabel: "the source" });
}

function fail(msg) {
  $("loading").hidden = true;
  const e = $("error");
  e.hidden = false;
  e.textContent = msg;
}

main();
"#;

const STARTER_MANIFEST: &str = r#"{
  "id": "{{ID}}",
  "name": "{{NAME}}",
  "version": "0.1.0",
  "blurb": "An explorer over the {{CORPUS}} corpus.",
  "corpus": "{{CORPUS}}",
  "entry": "index.html",
  "grants": { "mesh_store_read": true, "mesh_store_write": false, "inference_access": false, "knowledge_access": false },
  "trust": "unsigned"
}
"#;
