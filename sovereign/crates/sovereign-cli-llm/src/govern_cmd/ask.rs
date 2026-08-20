// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn govern ask` — answer a question from a governance corpus's
//! *current law*.
//!
//! A turn sealed to the corpus (retrieval restricted via
//! `enabled_corpora`, mirroring the bench live-runner). Two governance
//! affordances apply automatically because the sealed corpus carries a
//! `governance_oplog.jsonl`:
//!   - the active-set retrieval filter drops superseded/retracted rules'
//!     evidence chunks (FR-9 RL-3 — the answer can't be grounded in dead
//!     law);
//!   - the cite-or-abstain gate runs as `GateSurface::Governance`
//!     (RL-1: no confabulated rule; RL-2: honest abstention).
//!
//! After the answer, we render *supersession provenance*: for any cited
//! rule that replaced an earlier one, a one-line "(replaced … — decision
//! <date>)" read from the `Supersede` op.

use std::io::Write;

use corpus_engine::enrichment::{GovernanceOpKind, RuleStatus};
use corpus_engine::oplog::Oplog;
use futures::StreamExt as _;

use sovereign_core::types::Intent;

use super::{atlas_dir, load_view};
use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;

/// Answering discipline for governance Q&A, injected as the session's
/// custom-instructions (the general persona layer). Keeps open-ended
/// answers honest + cited + supersession-aware. It lives HERE, in the CLI
/// verb — the runtime stays domain-agnostic; this is just a persona string.
pub(crate) const GOVERN_ASK_DISCIPLINE: &str = "\
You are answering questions about a community's governing rules: a founding charter plus dated decisions that amend it over time. \
Answer ONLY what the current rules and decisions actually address. \
If the rules do not cover the question, say so plainly in one sentence (for example: \"The house rules don't address that.\") and stop — do NOT pad the answer with tangentially-related rules. \
When you state a rule, cite the specific Article or Decision it comes from. \
If an earlier rule was changed by a later decision, give the CURRENT rule and note that it replaced the earlier one; never present a superseded rule as if it were current.";

pub async fn cmd_ask(args: &[String]) -> i32 {
    let (mut globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    // Governance answering discipline rides the general persona layer.
    globals.custom_instructions = Some(GOVERN_ASK_DISCIPLINE.to_string());
    let mut positional = rest.iter();
    let Some(corpus_id) = positional.next() else {
        eprintln!("error: usage: sovereign govern ask <corpus-id> \"<question>\"");
        return 2;
    };
    let Some(question) = positional.next() else {
        eprintln!("error: missing question");
        eprintln!("  usage: sovereign govern ask {corpus_id} \"<question>\"");
        return 2;
    };

    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bootstrap failed: {e}");
            return 1;
        }
    };

    // Sealed turn (mirrors bench live_runner): mint a conversation, seal
    // retrieval to this one corpus, run the real chat path.
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

    // Pin the intent: governance questions are always factual lookups over
    // the sealed corpus, so bypass the router (which can misclassify a
    // lookup as Conation/Simple and skip the active-set filter + gate).
    let mut answer = String::new();
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
                        answer.push_str(&chunk);
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
            // Non-streamable intent: take the one-shot path.
            match session.runtime.handle_message(question, &conv_id).await {
                Ok(resp) => {
                    println!("{}", resp.message.content);
                    answer = resp.message.content;
                }
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

    // Glass-box: surface the verbatim source passages retrieval actually fed
    // this answer — post active-set filter, so only *current* law — each with
    // a traceable section citation, resolved deterministically from the corpus
    // index. The model's inline citation can garble a date or title; this
    // footer can't, so the human always has the real rules to check the
    // synthesis against rather than trusting the model's cite.
    render_sources(&session, &conv_id, corpus_id).await;
    render_supersession_provenance(corpus_id, &answer);
    0
}

/// Deterministic "sources" footer: the verbatim passages retrieval actually
/// fed the answer (recovered from the persisted assistant message metadata,
/// mirroring the bench live-runner), resolved to full text + a human section
/// title via the corpus index. Independent of the model's inline citation, so
/// a garbled cite never hides the raw material the reviewer needs to judge the
/// answer. One line per distinct section, in retrieval-rank order.
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
    if chunk_refs.is_empty() {
        return;
    }
    let index_root = crate::enrich_cmd::paths::index_root(corpus_id);
    let chunk_to_section =
        corpus_engine::enrichment::governance_view::chunk_to_section_map(&index_root);
    let titles = corpus_engine::enrichment::governance_view::section_titles(&index_root);

    // Distinct sections in retrieval-rank order. The persisted metadata is
    // relevance-ranked — the rule most relevant to the question comes first —
    // so the top entries are the ones the answer actually leans on. On a small
    // corpus retrieval can surface half the rules; we show the top few and say
    // how many more were retrieved (no silent truncation).
    const TOP: usize = 6;
    let mut seen = std::collections::HashSet::new();
    let mut distinct: Vec<(String, String, u64)> = Vec::new();
    for c in &chunk_refs {
        let (Some(cid), Some(chid)) = (
            c.get("corpus_id").and_then(|v| v.as_str()),
            c.get("chunk_id").and_then(|v| v.as_u64()),
        ) else {
            continue;
        };
        let title = chunk_to_section
            .get(&chid)
            .and_then(|s| titles.get(s))
            .cloned()
            .unwrap_or_else(|| format!("chunk {chid}"));
        if seen.insert(title.clone()) {
            distinct.push((title, cid.to_string(), chid));
        }
    }
    if distinct.is_empty() {
        return;
    }

    eprintln!("\nsources — rules retrieved for this answer (most relevant first):");
    for (title, cid, chid) in distinct.iter().take(TOP) {
        eprintln!("  · {title}");
        // FULL section text from the index (not the truncated metadata snippet),
        // mirroring the bench live-runner's resolution. Strip the section's own
        // title line so the body isn't printed twice.
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
        let body = body.strip_prefix(title.as_str()).unwrap_or(body).trim();
        if !body.is_empty() {
            let shown = if body.chars().count() > 240 {
                format!("{}…", body.chars().take(240).collect::<String>())
            } else {
                body.to_string()
            };
            eprintln!("      {shown}");
        }
    }
    if distinct.len() > TOP {
        eprintln!(
            "  … and {} more section(s) retrieved (not shown)",
            distinct.len() - TOP
        );
    }
}

/// Render the lineage of any *superseding* rule the answer actually relied
/// on: a one-line "(replaced <old rule> — <why>)". Keyed on the answer
/// citing the rule's section TITLE — so it fires for the decision the answer
/// used, never as ambient noise just because retrieval surfaced it. (Atoms
/// cite section ids; the model cites the section's human title, so we match
/// on the title.)
fn render_supersession_provenance(corpus_id: &str, answer: &str) {
    let Ok(view) = load_view(corpus_id) else {
        return;
    };
    let ops = match Oplog::<GovernanceOpKind>::new(atlas_dir(corpus_id)).read_all() {
        Ok(o) => o,
        Err(_) => return,
    };
    let titles = corpus_engine::enrichment::governance_view::section_titles(
        crate::enrich_cmd::paths::index_root(corpus_id),
    );
    let answer_lc = answer.to_lowercase();

    let mut lines = Vec::new();
    for rule in view
        .rules
        .iter()
        .filter(|r| matches!(r.status, RuleStatus::Active))
    {
        let Some(section) = rule.citation.as_ref().map(|c| &c.chunk_id) else {
            continue;
        };
        let answer_cites_rule = titles
            .get(section)
            .map(|t| answer_lc.contains(&t.to_lowercase()))
            .unwrap_or(false);
        if !answer_cites_rule {
            continue;
        }
        for op in &ops {
            if let GovernanceOpKind::Supersede {
                new_rule,
                old_rules,
                rationale,
            } = &op.kind
            {
                if new_rule != &rule.id {
                    continue;
                }
                let replaced: Vec<String> = old_rules
                    .iter()
                    .map(|oid| {
                        view.rules
                            .iter()
                            .find(|r| &r.id == oid)
                            .map(|r| quote_snippet(&r.text))
                            .unwrap_or_else(|| oid.as_str().to_string())
                    })
                    .collect();
                // Prefer the human rationale (it carries the real decision
                // date); fall back to the op's record date.
                let why = if rationale.trim().is_empty() {
                    chrono::DateTime::from_timestamp(op.ts_unix, 0)
                        .map(|d| format!("recorded {}", d.format("%Y-%m-%d")))
                        .unwrap_or_else(|| op.ts_unix.to_string())
                } else {
                    rationale.trim().to_string()
                };
                lines.push(format!("  (replaced {} — {why})", replaced.join("; ")));
            }
        }
    }
    if !lines.is_empty() {
        eprintln!("\nsupersession provenance:");
        for l in lines {
            eprintln!("{l}");
        }
    }
}

fn quote_snippet(s: &str) -> String {
    let s = s.trim();
    let shortened: String = if s.chars().count() > 80 {
        format!("{}…", s.chars().take(80).collect::<String>())
    } else {
        s.to_string()
    };
    format!("\"{shortened}\"")
}
