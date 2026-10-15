// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Asker (ONTOLOGY_METHOD §Reading, campaign ontology-layer C5): the one
//! place a build's model answers come from. Every question the reader and
//! RESOLVE ask goes through the chat port, and every vector the atlas build
//! embeds through the embed port; the Asker stands in front of both and
//! answers each from where the [`Asker`] names:
//!
//! - `daemon`: the model answers, and every answer is recorded in the
//!   corpus's answer store, keyed by the question's content;
//! - `replay`: the store answers; a question it holds no answer for is
//!   refused, never sent to the daemon;
//! - `recorded`: the store answers what the model this run asks gave it
//!   (each answer is stamped with the model that gave it); the model answers
//!   the rest, recorded, so a run asks only the questions no earlier run of
//!   that model asked;
//! - `gold`: a store written from labels answers, refusing the same way.
//!
//! A question's key is the hash of everything that makes it that question:
//! the whole prompt (system, user, schema, phase, sampling, budget, so the
//! candidates a RESOLVE question shows are in it) and the per-call budget, or
//! the text embedded. The same question asked twice in one run gets the one
//! answer first given, so a replay of the run reproduces it exactly.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use kernel_types::ContentHash;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::enrichment::atlas::resolve_records::{compose_wave, wave_singles, wave_text};
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::error::{Error, Result};
use crate::types::EmbedFn;
use crate::InferenceFn;

/// Where a build's answers come from: the Asker the spec names. Closed: a new
/// source is a design change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Asker {
    /// The model answers; every answer is recorded.
    #[default]
    Daemon,
    /// The recorded answers of earlier runs answer; nothing reaches a model.
    Replay,
    /// The recorded answers the model this run asks gave answer; the model
    /// answers the rest, and every new answer is recorded.
    Recorded,
    /// Answers written from labels answer; nothing reaches a model.
    Gold,
}

impl Asker {
    pub const FLAG: &'static str = "--asker";

    pub fn parse(value: &str) -> std::result::Result<Self, String> {
        match value {
            "daemon" => Ok(Self::Daemon),
            "replay" => Ok(Self::Replay),
            "recorded" => Ok(Self::Recorded),
            "gold" => Ok(Self::Gold),
            other => Err(format!(
                "unknown asker `{other}`; expected daemon, replay, recorded or gold"
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Replay => "replay",
            Self::Recorded => "recorded",
            Self::Gold => "gold",
        }
    }

    /// Whether answering needs the model reachable.
    pub fn needs_daemon(self) -> bool {
        matches!(self, Self::Daemon | Self::Recorded)
    }

    /// Whether earlier answers in the store answer this run's questions.
    fn reads_store(self) -> bool {
        self != Self::Daemon
    }

    /// The store this source reads (`replay`, `gold`) or writes (`daemon`).
    pub fn store_path(self, enrichment_dir: &Path) -> PathBuf {
        enrichment_dir.join(match self {
            Self::Daemon | Self::Replay | Self::Recorded => ANSWERS_FILE,
            Self::Gold => GOLD_FILE,
        })
    }
}

/// The recorded answers of a corpus's runs, beside its enrichment.
pub const ANSWERS_FILE: &str = "answers.jsonl";
/// Answers written from labels, read by `--asker gold`.
pub const GOLD_FILE: &str = "answers.gold.jsonl";

/// One recorded answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Answer {
    pub key: String,
    /// `chat` or `embed`.
    pub port: String,
    /// The question's phase id, for reading a store by eye.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// The model that gave the answer (`AnsweredBy`); none in a store written
    /// from labels or recorded before answers were stamped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Chat: the text answered. Embed: the vector.
    pub answer: Value,
}

/// The key of a chat question: the whole prompt and the per-call budget.
pub fn chat_key(prompt: &ChatPrompt, max_tokens: Option<u32>) -> String {
    let content = serde_json::to_string(&(prompt, max_tokens))
        .expect("a ChatPrompt and a budget always serialize");
    ContentHash::of_str(&format!("chat\0{content}")).to_hex()
}

/// The key of an embedding: the text embedded.
pub fn embed_key(text: &str) -> String {
    ContentHash::of_str(&format!("embed\0{text}")).to_hex()
}

/// Which model answers a question, by its port (`chat`, `embed`) and phase
/// id: the model the daemon client routes it to. A new answer is stamped with
/// it, and under `recorded` a stored answer is reused only when it gave it.
pub type AnsweredBy = Arc<dyn Fn(&str, Option<&str>) -> String + Send + Sync>;

/// The answers this run has given or been given, and where new ones go.
struct Store {
    source: Asker,
    path: PathBuf,
    by: AnsweredBy,
    answers: Mutex<HashMap<String, Value>>,
    out: Option<Mutex<std::io::BufWriter<std::fs::File>>>,
}

impl Store {
    fn open(source: Asker, path: PathBuf, by: AnsweredBy) -> Result<Self> {
        let mut answers = HashMap::new();
        let mut other_model = 0usize;
        // A `daemon` run starts from nothing it has not asked: an answer from
        // an earlier run is never reused, so the model answers every question
        // this run asks and the store records it. Replay and gold read all; a
        // `recorded` run reads what this run's model gave, and a store not
        // yet written is an empty one.
        if source == Asker::Recorded && !path.exists() {
            info!(asker = source.label(), store = %path.display(), "asker: no store yet; every question goes to the model");
        } else if source.reads_store() {
            let text = std::fs::read_to_string(&path).map_err(|e| {
                Error::InvalidInput(format!(
                    "the {} asker reads {}: {e}",
                    source.label(),
                    path.display()
                ))
            })?;
            for (n, line) in text
                .lines()
                .enumerate()
                .filter(|(_, l)| !l.trim().is_empty())
            {
                let a: Answer = serde_json::from_str(line).map_err(|e| {
                    Error::Serialization(format!("{} line {}: {e}", path.display(), n + 1))
                })?;
                if source == Asker::Recorded
                    && a.model.as_deref() != Some(by(&a.port, a.phase.as_deref()).as_str())
                {
                    other_model += 1;
                    continue;
                }
                // The latest recording of a question is the one a replay gives.
                answers.insert(a.key, a.answer);
            }
        }
        let out = if source.needs_daemon() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            Some(Mutex::new(std::io::BufWriter::new(file)))
        } else {
            None
        };
        info!(asker = source.label(), store = %path.display(), answers = answers.len(), other_model, "asker: answering");
        Ok(Self {
            source,
            path,
            by,
            answers: Mutex::new(answers),
            out,
        })
    }

    fn get(&self, key: &str) -> Option<Value> {
        self.answers
            .lock()
            .expect("the answer map is never poisoned")
            .get(key)
            .cloned()
    }

    /// Keep `answer` as this run's answer to `key` and record it. A question
    /// answered concurrently keeps the first answer stored; the caller gets
    /// that one, so the run and its replay agree. An answer that cannot be
    /// recorded fails its question: a run that cannot replay is not C5's.
    fn keep(&self, key: String, port: &str, phase: Option<&str>, answer: Value) -> Result<Value> {
        let mut answers = self
            .answers
            .lock()
            .expect("the answer map is never poisoned");
        if let Some(first) = answers.get(&key) {
            return Ok(first.clone());
        }
        answers.insert(key.clone(), answer.clone());
        drop(answers);
        if let Some(out) = &self.out {
            let line = Answer {
                key,
                port: port.to_string(),
                phase: phase.map(str::to_string),
                model: Some((self.by)(port, phase)),
                answer: answer.clone(),
            };
            let mut out = out.lock().expect("the store writer is never poisoned");
            let written = serde_json::to_writer(&mut *out, &line)
                .map_err(std::io::Error::other)
                .and_then(|()| out.write_all(b"\n"))
                .and_then(|()| out.flush());
            if let Err(e) = written {
                warn!(store = %self.path.display(), error = %e, "asker: an answer could not be recorded");
                return Err(Error::Io(e));
            }
        }
        Ok(answer)
    }

    fn missing(&self, port: &str, phase: Option<&str>, key: &str) -> Error {
        warn!(
            asker = self.source.label(),
            port,
            ?phase,
            key,
            "asker: no recorded answer; refused"
        );
        Error::Extraction(format!(
            "the {} asker has no answer recorded for this {port} question (phase {}, key {key}) in {}; \
             it is refused, not sent to a model",
            self.source.label(),
            phase.unwrap_or("-"),
            self.path.display()
        ))
    }
}

/// Put the Asker in front of a build's two model ports. `chat` and `embed` are
/// the daemon's, `by` the models they route each question to; under `replay`
/// and `gold` they are never called.
pub fn answering(
    source: Asker,
    enrichment_dir: &Path,
    by: AnsweredBy,
    embed: EmbedFn,
    chat: InferenceFn,
) -> Result<(EmbedFn, InferenceFn)> {
    let store = Arc::new(Store::open(source, source.store_path(enrichment_dir), by)?);
    let chat_store = Arc::clone(&store);
    let asked_chat: InferenceFn = Arc::new(move |prompt: &ChatPrompt, max_tokens| {
        let store = Arc::clone(&chat_store);
        if let Some(split) = wave_singles(prompt) {
            return ask_wave(
                store,
                Arc::clone(&chat),
                prompt.phase_id.clone(),
                split,
                max_tokens,
            );
        }
        let key = chat_key(prompt, max_tokens);
        let phase = prompt.phase_id.clone();
        let recorded = store.get(&key);
        let call =
            (store.source.needs_daemon() && recorded.is_none()).then(|| chat(prompt, max_tokens));
        Box::pin(async move {
            if let Some(Value::String(text)) = recorded {
                debug!(asker = store.source.label(), ?phase, %key, "asker: answered from the store");
                return Ok(text);
            }
            let Some(call) = call else {
                return Err(store.missing("chat", phase.as_deref(), &key));
            };
            let text = call.await?;
            match store.keep(key, "chat", phase.as_deref(), Value::String(text))? {
                Value::String(text) => Ok(text),
                other => Err(Error::Serialization(format!(
                    "the store holds a non-text chat answer: {other}"
                ))),
            }
        })
    });
    let embed_store = store;
    let asked_embed: EmbedFn = Arc::new(move |text: &str| {
        let store = Arc::clone(&embed_store);
        let key = embed_key(text);
        let recorded = store.get(&key);
        let call = (store.source.needs_daemon() && recorded.is_none()).then(|| embed(text));
        Box::pin(async move {
            let vector = |v: Value| -> Result<Vec<f32>> {
                serde_json::from_value(v)
                    .map_err(|e| Error::Serialization(format!("a recorded embedding: {e}")))
            };
            if let Some(v) = recorded {
                return vector(v);
            }
            let Some(call) = call else {
                return Err(store.missing("embed", None, &key));
            };
            let v = call.await?;
            vector(store.keep(key, "embed", None, serde_json::json!(v))?)
        })
    });
    Ok((asked_embed, asked_chat))
}

/// A forced-choice wave, answered question by question: each by its single
/// prompt's key, so a wave and its single questions share one store and a run
/// recorded either way replays the other. The questions the store holds no
/// answer for go to the daemon as one smaller wave (`daemon`), or are refused
/// one by one (`replay`, `gold`); every new answer is recorded under its own
/// question's key. A refused question is that question's alone.
fn ask_wave(
    store: Arc<Store>,
    chat: InferenceFn,
    phase: Option<String>,
    split: std::result::Result<Vec<ChatPrompt>, String>,
    max_tokens: Option<u32>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>> {
    Box::pin(async move {
        let singles = split.map_err(|e| Error::InvalidInput(format!("forced_choice wave: {e}")))?;
        let keys: Vec<String> = singles.iter().map(|s| chat_key(s, max_tokens)).collect();
        let text = |v: Value| match v {
            Value::String(t) => Ok(t),
            other => Err(format!("the store holds a non-text chat answer: {other}")),
        };
        let mut answers: Vec<Option<std::result::Result<String, String>>> =
            keys.iter().map(|k| store.get(k).map(text)).collect();
        let missing: Vec<usize> = (0..keys.len()).filter(|&i| answers[i].is_none()).collect();
        debug!(
            asker = store.source.label(),
            ?phase,
            questions = keys.len(),
            recorded = keys.len() - missing.len(),
            "asker: a wave"
        );
        if !missing.is_empty() && !store.source.needs_daemon() {
            for &i in &missing {
                answers[i] = Some(Err(store
                    .missing("chat", phase.as_deref(), &keys[i])
                    .to_string()));
            }
        } else if !missing.is_empty() {
            let ask: Vec<ChatPrompt> = missing.iter().map(|&i| singles[i].clone()).collect();
            let asked: Vec<std::result::Result<String, String>> = match compose_wave(&ask) {
                Ok(wave) if ask.len() > 1 => match chat(&wave, max_tokens).await {
                    Ok(raw) => match oicp_types::forced_choice::wave_texts(&raw) {
                        Some(texts) if texts.len() == ask.len() => texts,
                        _ => {
                            let head: String = raw.chars().take(120).collect();
                            vec![Err(format!("not a wave answer: {head:?}")); ask.len()]
                        }
                    },
                    Err(e) => vec![Err(e.to_string()); ask.len()],
                },
                _ => {
                    let mut out = Vec::with_capacity(ask.len());
                    for single in &ask {
                        out.push(chat(single, max_tokens).await.map_err(|e| e.to_string()));
                    }
                    out
                }
            };
            for (&i, answer) in missing.iter().zip(asked) {
                answers[i] = Some(match answer {
                    Ok(t) => text(store.keep(
                        keys[i].clone(),
                        "chat",
                        phase.as_deref(),
                        Value::String(t),
                    )?),
                    Err(why) => Err(why),
                });
            }
        }
        let answers: Vec<std::result::Result<String, String>> = answers
            .into_iter()
            .map(|a| a.expect("every question answered or refused"))
            .collect();
        Ok(wave_text(&answers))
    })
}

#[cfg(test)]
#[path = "asker_tests.rs"]
mod tests;
