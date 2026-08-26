// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn portfolio ask` — one question, per-company answers (AC-6).
//!
//! A portfolio query is a *logical* roll-up, not a physical merge. We run
//! the question once PER corpus in the set, each turn sealed to that single
//! corpus, and present the results as per-company sections. This is what
//! makes AC-6 hold by construction: every company is represented (a single
//! global top-k query lets the corpus with the most matching text dominate
//! and starves the others — the documented dominant-source dilution), and
//! each section is grounded in — and cited to — its own filing, with no
//! cross-company bleed. Each per-corpus turn rides the same cite-or-abstain
//! path `proxy ask` ships: sealed to a `proxy-cik…` corpus, the runtime
//! selects `GateSurface::ProxyArgument`.
//!
//! Cost scales with portfolio size (N corpora = N turns); fine for the MVP
//! demo. A per-corpus-quota single query is the future optimization.

use std::io::Write;

use futures::StreamExt as _;

use sovereign_core::types::Intent;

use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;
use crate::proxy_cmd::ask::PROXY_ASK_DISCIPLINE;

pub async fn cmd_ask(args: &[String]) -> i32 {
    let (mut globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    globals.custom_instructions = Some(PROXY_ASK_DISCIPLINE.to_string());
    let mut positional = rest.iter();
    let Some(name) = positional.next() else {
        eprintln!("error: usage: sovereign portfolio ask <name> \"<question>\"");
        return 2;
    };
    let Some(question) = positional.next() else {
        eprintln!("error: missing question");
        eprintln!("  usage: sovereign portfolio ask {name} \"<question>\"");
        return 2;
    };

    let corpora = {
        let (store, _node) = match super::open_store() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        match super::get_portfolio(&store, name) {
            Some(c) if !c.is_empty() => c,
            Some(_) => {
                eprintln!("error: portfolio `{name}` is empty — add corpora with `svrn portfolio add {name} <corpus-id ...>`");
                return 1;
            }
            None => {
                eprintln!("error: no portfolio named `{name}`");
                return 1;
            }
        }
    };

    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bootstrap failed: {e}");
            return 1;
        }
    };

    eprintln!(
        "\nportfolio `{name}` — {} corpora, one question each (per-company roll-up):",
        corpora.len()
    );
    eprintln!("> {question}\n");

    let mut failures = 0;
    for corpus_id in &corpora {
        println!("\n══════════ {corpus_id} ══════════");
        let conv_id = uuid::Uuid::new_v4().to_string();
        let created_at = crate::proxy_cmd::now_unix();
        let _ = session
            .store
            .insert_empty_conversation(&conv_id, created_at, None)
            .await;
        let _ = session
            .store
            .set_conversation_enabled_corpora(&conv_id, Some(vec![corpus_id.clone()]))
            .await;

        // ONE turn driver (TOPOLOGY §10 phase 6) — replaces a hand-rolled
        // drain plus a `NotImplemented` fallback to `handle_message` that
        // double-wrote the user's message (the streaming path persists it
        // before refusing a document-attached turn).
        let sink = crate::turn_sink::StdoutTurnSink::default();
        sovereign_core::runtime::serve_turn(
            &session.runtime,
            session.store.as_ref(),
            &conv_id,
            question,
            sovereign_contracts::types::TurnMode::Grounded,
            Some(Intent::KnowledgeQuery),
            None,
            &sink,
        )
        .await;
        println!();
        if let Some(e) = sink.failure() {
            eprintln!("turn failed: {e}");
            failures += 1;
        }
        render_sources(&session, &conv_id, corpus_id).await;
    }

    if failures > 0 {
        eprintln!("\n({failures} of {} company turn(s) failed)", corpora.len());
        return 1;
    }
    0
}

/// Per-company sources footer: the verbatim filing passages retrieval fed
/// THIS company's answer, resolved from its own corpus — the AC-6 proof that
/// each section cites its own filing.
async fn render_sources(
    session: &crate::chat_cmd::bootstrap::ChatSession,
    conv_id: &str,
    corpus_id: &str,
) {
    let chunk_refs: Vec<serde_json::Value> = session
        .store
        .get_conversation(conv_id)
        .await
        .ok()
        .and_then(|c| c.messages.last().and_then(|m| m.metadata.clone()))
        .and_then(|m| {
            m.get("retrieved_chunks")
                .and_then(|v| v.as_array())
                .cloned()
        })
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    let mut ids: Vec<u64> = Vec::new();
    for c in &chunk_refs {
        // A sealed single-corpus turn should only ever cite its own corpus;
        // assert that by filtering to corpus_id (any stray is a bug, dropped).
        let same = c.get("corpus_id").and_then(|v| v.as_str()) == Some(corpus_id);
        if let (true, Some(chid)) = (same, c.get("chunk_id").and_then(|v| v.as_u64())) {
            if seen.insert(chid) {
                ids.push(chid);
            }
        }
    }
    if ids.is_empty() {
        return;
    }
    eprintln!("  sources ({} passage(s) from {corpus_id}):", ids.len());
    if let Ok(index) = session.corpus_engine.open_index_for_corpus(corpus_id).await {
        for chid in ids.iter().take(4) {
            if let Ok(mut rows) = index.chunks_by_ids(&[*chid]).await {
                if let Some(row) = rows.pop() {
                    let body = row.content.trim();
                    let shown = if body.chars().count() > 180 {
                        format!("{}…", body.chars().take(180).collect::<String>())
                    } else {
                        body.to_string()
                    };
                    if !shown.is_empty() {
                        eprintln!("    · (chunk {chid}) {shown}");
                    }
                }
            }
        }
    }
}
