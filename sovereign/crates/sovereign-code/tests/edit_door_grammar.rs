// SPDX-License-Identifier: AGPL-3.0-or-later
//! The editor door's grammar input (pb-meshapp-rest, phase-b-68): the host
//! supplies the extension → grammar lookup, and the door's syntax filter and
//! symbol lane run on it or say, by name, that they could not.
//!
//! The case is the syntax filter's own: the developer renamed
//! `console.log` → `console.debug` twice in a `.tsx` file, one `console.log`
//! remains in code and one in a comment. With a grammar the comment match is
//! not a site (1 edit); with none, standalone `svrn code`'s composition, both
//! survive and the debug block names the filter and the symbol lane
//! unjudged. The stock binary's own answer is asserted in sovereign-stock's
//! one_process_e2e against corpus-engine's registry.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[path = "common/next_edit_grammar.rs"]
mod next_edit_grammar;

const TSX: &str = "console.debug(1);\nconsole.debug(2);\nconsole.log(3);\n// console.log(4);\n";

fn request() -> serde_json::Value {
    let unit = serde_json::json!({
        "before": "log", "after": "debug",
        "left": "  console.", "right": "(\"x\");"
    });
    serde_json::json!({
        "history": [unit, unit],
        "text": TSX,
        "cursor": 0,
        "path": "/work/src/App.tsx",
        "debug": true,
        "symbol_lane": true,
        "corpus_id": "app",
        "workspace_root": "/work",
    })
}

async fn post(grammar: Option<sovereign_code::face::GrammarLookup>) -> serde_json::Value {
    // No serve behind the door: the model lane is not asked for here.
    let door =
        sovereign_code::edit_predictions::router(sovereign_code::edit_predictions::EditDoor::new(
            grammar,
            std::env::temp_dir(),
            "http://127.0.0.1:1".to_string(),
        ));
    let resp = door
        .oneshot(
            Request::post("/v1/edit_predictions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request()).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn a_host_grammar_drops_the_comment_site() {
    let body = post(Some(next_edit_grammar::grammar_for)).await;
    assert_eq!(body["engine"], "rule", "{body}");
    let edits = body["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 1, "the comment match is not a site: {body}");
    let start = edits[0]["start"].as_u64().unwrap() as usize;
    assert!(start < TSX.find("//").unwrap(), "{body}");
    assert!(
        body["sovereign_debug"].get("syntax_filter").is_none(),
        "{body}"
    );
    assert!(
        body["sovereign_debug"].get("symbol_lane").is_none(),
        "{body}"
    );
}

#[tokio::test]
async fn no_grammar_names_the_filter_and_the_symbol_lane_unjudged() {
    let body = post(None).await;
    assert_eq!(body["engine"], "rule", "{body}");
    assert_eq!(
        body["edits"].as_array().unwrap().len(),
        2,
        "without a grammar no site is judged: {body}"
    );
    assert_eq!(
        body["sovereign_debug"]["syntax_filter"],
        "unjudged:grammar_absent"
    );
    assert_eq!(
        body["sovereign_debug"]["symbol_lane"],
        "unjudged:grammar_absent"
    );
    assert_eq!(body["navigation"]["declined"], "grammar_absent");
}
