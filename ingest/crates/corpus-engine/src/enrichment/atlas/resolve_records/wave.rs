// SPDX-License-Identifier: AGPL-3.0-or-later
//! Forced-choice waves as ingest asks them: several closed questions that
//! need nothing from one another, sent as ONE request the engine answers off
//! one decoded prompt (`oicp_types::forced_choice::wave`).
//!
//! A wave is made from the single prompts the questions would be asked as,
//! and splits back into exactly those prompts ([`singles`]), so a wave asks
//! nothing a single prompt would not and every answer keeps its single
//! prompt's key in the answer store (`asker.rs`): a run recorded one way
//! replays the other. [`compose`] refuses to make a wave whenever the round
//! trip would not give the same prompts back, and the caller then asks one
//! by one. A port that cannot answer a wave natively (a scripted oracle, a
//! store) answers it through [`one_by_one`].

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use oicp_types::forced_choice::{self, Branch};
use tracing::{debug, warn};

use super::select::decision_call;
use super::Refusal;
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::InferenceFn;

/// The most questions one wave request carries. A transport bound, not a
/// reading choice: every question is answered as its single request would be
/// whatever wave it rides in, and this keeps a request (one branch at a time,
/// ~0.3 s each on the 35B, 2026-10-11) well inside the engine's 300 s
/// per-request deadline, past which the remaining branches would be refused.
const WAVE_MAX_QUESTIONS: usize = 64;

/// The wave asking exactly `singles`: their shared opening as its prompt,
/// each one's remainder and labels as a branch. `Err` when they differ in
/// anything but their user turn, or a single's body is not the plain
/// forced-choice body of its labels, since the wave could not split back into
/// the same prompts and keys.
pub(crate) fn compose(singles: &[ChatPrompt]) -> Result<ChatPrompt, String> {
    let Some(first) = singles.first() else {
        return Err("no questions".to_string());
    };
    let shell = |p: &ChatPrompt| ChatPrompt {
        user: String::new(),
        response_schema: None,
        ..p.clone()
    };
    let common = shell(first);
    let mut prefix = first.user.len();
    let mut labels = Vec::with_capacity(singles.len());
    for p in singles {
        if serde_json::to_value(shell(p)).ok() != serde_json::to_value(&common).ok() {
            return Err("the questions differ in more than their user turn".to_string());
        }
        let body = p
            .response_schema
            .as_ref()
            .ok_or("a question asks no forced choice")?;
        let l = forced_choice::candidates(body).ok_or("a question asks no forced choice")?;
        let refs: Vec<&str> = l.iter().map(String::as_str).collect();
        if forced_choice::schema(&refs) != *body {
            return Err("a question's body is not the plain forced-choice body".to_string());
        }
        labels.push(l);
        prefix = prefix.min(
            first
                .user
                .bytes()
                .zip(p.user.bytes())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    while !first.user.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let branches: Vec<Branch> = singles
        .iter()
        .zip(labels)
        .map(|(p, labels)| Branch {
            suffix: p.user[prefix..].to_string(),
            labels,
        })
        .collect();
    Ok(ChatPrompt {
        user: first.user[..prefix].to_string(),
        response_schema: Some(forced_choice::wave(&branches)),
        ..common
    })
}

/// The single prompts a wave asks, in branch order; `None` when `prompt` is
/// not a wave, `Err` when it declares one it does not spell.
pub fn singles(prompt: &ChatPrompt) -> Option<Result<Vec<ChatPrompt>, String>> {
    let branches = forced_choice::wave_branches(prompt.response_schema.as_ref()?)?;
    Some(branches.map(|branches| {
        branches
            .iter()
            .map(|b| ChatPrompt {
                user: format!("{}{}", prompt.user, b.suffix),
                response_schema: Some(forced_choice::branch_body(b)),
                ..prompt.clone()
            })
            .collect()
    }))
}

/// `inner`, answering a wave by asking each of its single prompts in turn
/// and every other prompt as it is. A branch `inner` fails is refused in the
/// wave's answer, never dropped.
pub fn one_by_one(inner: InferenceFn) -> InferenceFn {
    Arc::new(move |prompt: &ChatPrompt, max_tokens| {
        let inner = Arc::clone(&inner);
        let split = singles(prompt);
        let prompt = prompt.clone();
        Box::pin(async move {
            let Some(split) = split else {
                return inner(&prompt, max_tokens).await;
            };
            let split = split.map_err(|e| {
                crate::error::Error::InvalidInput(format!("forced_choice wave: {e}"))
            })?;
            let mut texts = Vec::with_capacity(split.len());
            for single in &split {
                texts.push(match inner(single, max_tokens).await {
                    Ok(text) => Ok(text),
                    Err(e) => Err(e.to_string()),
                });
            }
            Ok(wave_text(&texts))
        })
    })
}

/// A wave's answer from each branch's own answer text or refusal.
pub(crate) fn wave_text(texts: &[Result<String, String>]) -> String {
    let items: Vec<String> = texts
        .iter()
        .map(|t| match t {
            Ok(text) => text.trim().to_string(),
            Err(why) => serde_json::json!({ forced_choice::REFUSED: why }).to_string(),
        })
        .collect();
    format!("[{}]", items.join(","))
}

/// The census funnel for a wave: `prompts` are the single forced choices the
/// questions would be asked as, `items` what each is about, for the trace.
/// Each answer is checked and traced as [`decision_call`] checks and traces a
/// single one; one branch's refusal is that branch's alone. When the prompts
/// cannot make a wave ([`compose`]) they are asked one by one through
/// [`decision_call`], and the trace says so.
pub(crate) async fn decision_wave(
    infer: &InferenceFn,
    prompts: &[ChatPrompt],
    labels: &[Vec<&str>],
    document: &str,
    items: &[String],
) -> Vec<Result<BTreeMap<String, f64>, Refusal>> {
    debug_assert_eq!(prompts.len(), labels.len());
    debug_assert_eq!(prompts.len(), items.len());
    if prompts.len() > WAVE_MAX_QUESTIONS {
        let mut out = Vec::with_capacity(prompts.len());
        for ((p, l), i) in prompts
            .chunks(WAVE_MAX_QUESTIONS)
            .zip(labels.chunks(WAVE_MAX_QUESTIONS))
            .zip(items.chunks(WAVE_MAX_QUESTIONS))
        {
            out.extend(Box::pin(decision_wave(infer, p, l, document, i)).await);
        }
        return out;
    }
    if prompts.is_empty() {
        return Vec::new();
    }
    let phase = &prompts[0].phase_id;
    let wave = match compose(prompts) {
        Ok(wave) => wave,
        Err(why) => {
            debug!(document, ?phase, questions = prompts.len(), %why, "atlas/resolve: no wave; asked one by one");
            let mut out = Vec::with_capacity(prompts.len());
            for ((p, l), item) in prompts.iter().zip(labels).zip(items) {
                out.push(decision_call(infer, p, l, document, item).await);
            }
            return out;
        }
    };
    let started = Instant::now();
    let answered = infer(&wave, None).await;
    let ms = started.elapsed().as_millis() as u64;
    let texts = match answered {
        Ok(raw) => match forced_choice::wave_texts(&raw) {
            Some(texts) if texts.len() == prompts.len() => texts,
            other => {
                let head: String = raw.chars().take(120).collect();
                let reason = match other {
                    Some(t) => format!(
                        "the wave answered {} of {} questions",
                        t.len(),
                        prompts.len()
                    ),
                    None => format!("not a wave answer: {head:?}"),
                };
                warn!(document, ?phase, ms, %reason, "atlas/resolve: decision wave answered no wave");
                return (0..prompts.len())
                    .map(|_| {
                        Err(Refusal::NoAnswer {
                            reason: reason.clone(),
                        })
                    })
                    .collect();
            }
        },
        Err(e) => {
            warn!(document, ?phase, ms, error = %e, "atlas/resolve: decision wave failed");
            let reason = format!("call failed: {e:#}");
            return (0..prompts.len())
                .map(|_| {
                    Err(Refusal::NoAnswer {
                        reason: reason.clone(),
                    })
                })
                .collect();
        }
    };
    debug!(
        document,
        ?phase,
        ms,
        questions = prompts.len(),
        "atlas/resolve: decision wave"
    );
    texts
        .into_iter()
        .zip(labels)
        .zip(items)
        .map(|((text, labels), item)| {
            let text = text.map_err(|reason| Refusal::NoAnswer { reason })?;
            match forced_choice::parse(&text) {
                Some(d) if labels.iter().all(|l| d.contains_key(*l)) => {
                    debug!(document, statement = %item, ?phase, distribution = ?d, "atlas/resolve: decision call");
                    Ok(d)
                }
                Some(d) => {
                    let missing: Vec<&str> =
                        labels.iter().copied().filter(|l| !d.contains_key(*l)).collect();
                    warn!(document, statement = %item, ?phase, ?missing, "atlas/resolve: decision call left labels out");
                    Err(Refusal::NoAnswer {
                        reason: format!("distribution lacks label(s) {missing:?}"),
                    })
                }
                None => Err(Refusal::NoAnswer {
                    reason: format!("not a forced-choice distribution: {text:?}"),
                }),
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "wave_tests.rs"]
mod tests;
