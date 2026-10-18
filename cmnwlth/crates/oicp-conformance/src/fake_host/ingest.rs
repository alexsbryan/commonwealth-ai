// SPDX-License-Identifier: AGPL-3.0-or-later
//! The fake's ingest routes: install, progress and the recipe test (v0.4 §5,
//! v0.5 §4, v0.6 §2.9), and the install route's own body limit (v0.6 §2.12).
//! A child of `fake_host`, so it reads the fake's private state.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{post, MethodRouter};
use axum::Json;
use oicp_types::ingest::reasons::INSTALL_TOO_LARGE;
use serde_json::{json, Value};

use super::{refuse, Fake};
use crate::fixture::sha256_hex;

/// The bound the fake's whole surface puts on a request body: the reference
/// host's client surface's 8 MiB, which refused an inline library of 22.6 MB
/// `413` before §2.12 (a model of that host, not its decider, which this
/// crate's dependency budget keeps out of reach).
pub(super) const SURFACE_LIMIT: usize = 8 * 1024 * 1024;

/// The largest install the fake states (`max_install_bytes`): past the
/// surface's bound, small enough that a check sending a body this size stays
/// quick.
pub(super) const MAX_INSTALL_BYTES: usize = 9 * 1024 * 1024;

/// The install route with its own body limit, as §2.12 states it;
/// `install_at_surface_limit` leaves the surface's bound on it while the
/// manifest still states the larger one.
pub(super) fn install_route(f: &Fake) -> MethodRouter<Arc<Fake>> {
    let limit = if f.breaks.install_at_surface_limit {
        SURFACE_LIMIT
    } else {
        MAX_INSTALL_BYTES
    };
    post(install).layer(DefaultBodyLimit::max(limit))
}

/// The lawful install: a body past the route's limit is `413 install too
/// large` naming the limit (§2.12), a recipe without a `[corpus]` table is
/// `400`, and the same recipe twice is `spawned: false`.
async fn install(State(f): State<Arc<Fake>>, req: Result<Json<Value>, JsonRejection>) -> Response {
    let req = match req {
        Ok(Json(v)) => v,
        Err(r)
            if r.status() == StatusCode::PAYLOAD_TOO_LARGE
                && !f.breaks.install_at_surface_limit =>
        {
            return refuse(
                StatusCode::PAYLOAD_TOO_LARGE,
                json!({"error": INSTALL_TOO_LARGE, "max_install_bytes": MAX_INSTALL_BYTES}),
            )
        }
        Err(r) => return r.into_response(),
    };
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
    // v0.6 §2.16: the allowance is part of the install, so the same recipe
    // with another allowance is a new install.
    let allow = req.get("allow_lookups").and_then(Value::as_bool) == Some(true);
    let held = {
        let mut allowed = f.allowed.lock().unwrap();
        let same = allowed.contains(&corpus) == allow;
        if allow {
            allowed.insert(corpus.clone());
        } else {
            allowed.remove(&corpus);
        }
        same.then_some(sha.as_str())
    };
    let previous = f
        .installed
        .lock()
        .unwrap()
        .insert(corpus.clone(), sha.clone());
    let spawned = previous.as_deref() != held;
    if spawned && f.queueable.contains(&corpus) {
        let mut queue = f.queue.lock().unwrap();
        if !queue.contains(&corpus) {
            queue.push(corpus.clone());
        }
    }
    if spawned {
        f.polls.lock().unwrap().remove(&corpus);
        if previous.is_some() {
            f.reinstalled.lock().unwrap().insert(corpus.clone());
        }
    }
    Json(json!({"corpus_id": corpus, "spawned": spawned, "recipe_sha256": sha})).into_response()
}

/// The lawful progress: every install listed from its first poll, complete,
/// with each fixture text `ready` under its name (v0.6 §2.9).
/// `late_listing` hides an install from the first poll after it;
/// `stale_reinstall` lists a re-install's first poll as the last install's
/// `complete` with no documents; `omit_documents` lists none.
pub(super) async fn progress(State(f): State<Arc<Fake>>) -> Json<Value> {
    let installed = f.installed.lock().unwrap();
    let mut polls = f.polls.lock().unwrap();
    let documents =
        |corpus: &str| -> Vec<Value> {
            if f.breaks.omit_documents {
                return Vec::new();
            }
            f.of(&[corpus.to_string()])
                .map(|s| {
                    let text = if f.breaks.unserved_text_names {
                        s.name.as_str()
                    } else {
                        s.text.as_str()
                    };
                    let mut entry = json!({"source": s.name, "state": "ready",
                                           "text_sha256": sha256_hex(text.as_bytes())});
                    if let Some(r) = f.merged(s) {
                        entry["record"] = json!(r);
                    }
                    entry
                })
                .chain(f.failed.iter().filter(|(c, ..)| c == corpus).map(
                    |(_, name, why)| json!({"source": name, "state": "failed", "reason": why}),
                ))
                .collect()
        };
    let mut progress: BTreeMap<&String, Value> = BTreeMap::new();
    for c in installed.keys() {
        let n = polls.entry(c.clone()).or_insert(0);
        *n += 1;
        if f.breaks.late_listing && *n == 1 {
            continue;
        }
        if f.reinstalled.lock().unwrap().remove(c.as_str()) && f.breaks.stale_reinstall {
            progress.insert(c, json!({"phase": "complete", "documents": []}));
            continue;
        }
        progress.insert(c, json!({"phase": "complete", "documents": documents(c)}));
    }
    Json(json!({"progress": progress}))
}

pub(super) async fn recipe_test(State(f): State<Arc<Fake>>) -> Json<Value> {
    let n = f.of(&[f.corpus.clone()]).count();
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
