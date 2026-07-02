// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn proxy ask` — answer a question about a company's ballot from
//! its SEC proxy statement, cite-or-abstain over the filing's verbatim text.
//!
//! A turn sealed to the corpus (retrieval restricted via `enabled_corpora`,
//! mirroring the governance/bench live-runner). The runtime selects
//! `GateSurface::ProxyArgument` because the sealed corpus is in the
//! `proxy-cik` family, so the cite-or-abstain gate is judged on the proxy
//! red lines: RL-1 (no confabulated opposition for a management item that
//! carries only the board's recommendation) and RL-2 (both sides cited for
//! a shareholder proposal). The answering discipline below is the persona
//! layer that keeps open-ended answers honest; the gate is the structural
//! backstop.

use std::io::Write;

use futures::StreamExt as _;

use sovereign_core::types::Intent;

use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;

/// Answering discipline for proxy Q&A, injected as the session's
/// custom-instructions (the general persona layer). Mirrors the
/// constitutional principle of the corpus: present the sides, never
/// editorialize, never manufacture a side the filing does not contain.
pub(crate) const PROXY_ASK_DISCIPLINE: &str = "\
You are answering questions about a public company's shareholder ballot, drawn ONLY from its SEC proxy statement (DEF 14A). \
For each matter to be voted on, state plainly what is being voted on and the SIDES as the filing presents them: for a shareholder proposal, the proponent's supporting statement AND the board's recommendation and statement in opposition; for a management proposal, the board's recommendation (almost always FOR). \
Quote or closely paraphrase the filing and attribute each side to who said it (the proponent vs the board). \
CRITICAL: a management proposal carries ONLY the board's recommendation — the filing contains no opposing case against it. If asked for 'the case against' such an item, say plainly that the filing carries only the board's recommendation and does not present an opposing statement; do NOT invent or infer one. \
Never tell the user how to vote — present the sides and stop.";

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
    let Some(corpus_id) = positional.next() else {
        eprintln!("error: usage: sovereign proxy ask <corpus-id> \"<question>\"");
        return 2;
    };
    let Some(question) = positional.next() else {
        eprintln!("error: missing question");
        eprintln!("  usage: sovereign proxy ask {corpus_id} \"<question>\"");
        return 2;
    };

    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bootstrap failed: {e}");
            return 1;
        }
    };

    // Sealed turn: mint a conversation, seal retrieval to this one corpus,
    // run the real chat path. The `proxy-cik` corpus family routes the gate
    // to GateSurface::ProxyArgument inside the runtime (kq_gate_surface).
    let conv_id = uuid::Uuid::new_v4().to_string();
    let created_at = super::now_unix();
    let _ = session
        .store
        .insert_empty_conversation(&conv_id, created_at, None)
        .await;
    let _ = session
        .store
        .set_conversation_enabled_corpora(&conv_id, Some(vec![corpus_id.clone()]))
        .await;

    eprintln!("\nconversation {conv_id} — sealed to corpus `{corpus_id}`");
    eprintln!("> {question}\n");

    // Pin the intent: proxy questions are factual lookups over the sealed
    // corpus, so bypass the router (which can misclassify a lookup and skip
    // the gate).
    match session
        .runtime
        .handle_message_stream_as(question, &conv_id, Intent::KnowledgeQuery)
        .await
    {
        Ok(handle) => {
            let mut stream = handle.stream;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(chunk) => {
                        print!("{chunk}");
                        let _ = std::io::stdout().flush();
                    }
                    Err(e) => {
                        eprintln!("\n[stream error] {e}");
                        break;
                    }
                }
            }
            println!();
        }
        Err(sovereign_core::error::Error::NotImplemented(_)) => {
            match session.runtime.handle_message(question, &conv_id).await {
                Ok(resp) => println!("{}", resp.message.content),
                Err(e) => {
                    eprintln!("turn failed: {e}");
                    return 1;
                }
            }
        }
        Err(e) => {
            eprintln!("stream start failed: {e}");
            return 1;
        }
    }

    render_sources(&session, &conv_id).await;
    0
}

/// Glass-box "sources" footer: the verbatim passages retrieval actually fed
/// the answer, recovered from the persisted assistant message metadata and
/// resolved to full text via the corpus index. Independent of the model's
/// inline citation, so a garbled cite never hides the raw filing text the
/// reviewer needs to check the answer against. One line per distinct chunk,
/// in retrieval-rank order.
async fn render_sources(session: &crate::chat_cmd::bootstrap::ChatSession, conv_id: &str) {
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
    if chunk_refs.is_empty() {
        return;
    }

    const TOP: usize = 6;
    let mut seen = std::collections::HashSet::new();
    let mut distinct: Vec<(String, u64)> = Vec::new();
    for c in &chunk_refs {
        let (Some(cid), Some(chid)) = (
            c.get("corpus_id").and_then(|v| v.as_str()),
            c.get("chunk_id").and_then(|v| v.as_u64()),
        ) else {
            continue;
        };
        if seen.insert((cid.to_string(), chid)) {
            distinct.push((cid.to_string(), chid));
        }
    }
    if distinct.is_empty() {
        return;
    }

    eprintln!("\nsources — filing passages retrieved for this answer (most relevant first):");
    for (cid, chid) in distinct.iter().take(TOP) {
        let body = match session.corpus_engine.open_index_for_corpus(cid).await {
            Ok(index) => index
                .chunks_by_ids(&[*chid])
                .await
                .ok()
                .and_then(|mut rows| rows.pop())
                .map(|row| row.content),
            Err(_) => None,
        }
        .unwrap_or_default();
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        let shown = if body.chars().count() > 240 {
            format!("{}…", body.chars().take(240).collect::<String>())
        } else {
            body.to_string()
        };
        eprintln!("  · ({cid} chunk {chid}) {shown}");
    }
    if distinct.len() > TOP {
        eprintln!(
            "  … and {} more passage(s) retrieved (not shown)",
            distinct.len() - TOP
        );
    }
}
