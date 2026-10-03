// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bundle-escape guard every program that serves a page bundle uses:
//! svrn's guest door and `svrn meshapp dev`, cmnwlth's `svrn ring show`
//! (FIVE_PROGRAMS §12 3a rung 4; moved from `sovereign_mesh::guest_pages`
//! in pb-meshapp-apps).

use std::path::Path;

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

/// Serve `rel` from inside `root`, or 404 — **never from outside it**.
///
/// The guard is not "reject `..`": a request path can spell an escape many
/// ways, and a check on the spelling is a check on what the caller authored
/// (ARCH §18.1). Both sides are canonicalized and the result must still be
/// under the root, so what is asserted is where the file actually IS.
///
/// Until this landed, both dev servers joined the request path onto the
/// bundle directory and read whatever came out.
pub fn serve_under(root: &Path, rel: &str, shim_src: Option<&str>) -> Response {
    let joined = root.join(rel);
    let Ok(real_root) = std::fs::canonicalize(root) else {
        return (StatusCode::NOT_FOUND, "bundle directory is gone").into_response();
    };
    let Ok(real) = std::fs::canonicalize(&joined) else {
        return (StatusCode::NOT_FOUND, format!("not found: {rel}")).into_response();
    };
    if !real.starts_with(&real_root) {
        // Say nothing about what is out there. A 404 and a refusal look the
        // same to a caller who should not have asked.
        tracing::warn!(rel, root = %real_root.display(), "dev server: refused a path outside the bundle");
        return (StatusCode::NOT_FOUND, format!("not found: {rel}")).into_response();
    }
    serve_file(&real, shim_src)
}

fn serve_file(file: &Path, shim_src: Option<&str>) -> Response {
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
    if let Some(src) = shim_src {
        let html = String::from_utf8_lossy(&bytes);
        let tag = format!("<script src=\"{src}\"></script>");
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
