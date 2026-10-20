// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one background worker: each queued corpus's pending sentences embedded
//! on the embed slot, a batch per call, the embedder's identity probe riding
//! along in the same call so every vector written is stamped by the model
//! that made it. A sentence already embedded anywhere in the corpus is
//! copied, not embedded again.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kernel_types::Sha256Hash;

use super::store::{same_embedder, unit, SentenceRow, PROBE_TEXT};
use super::{read, write, Entry, SentenceIndex, TARGET};
use crate::state::AppState;

/// Attempts at one batch before the corpus is left pending until it is
/// queued again.
const ATTEMPTS: u32 = 3;

pub(super) async fn run(state: AppState, index: SentenceIndex) {
    loop {
        let Some(corpus) = index.next_queued() else {
            index.woken().await;
            continue;
        };
        let at = Instant::now();
        let outcome = index_corpus(&state, &index, &corpus).await;
        index.finished();
        match outcome {
            Ok(Outcome::Removed { embedded }) => tracing::info!(
                target: TARGET,
                corpus = %corpus,
                embedded_now = embedded,
                elapsed_ms = at.elapsed().as_millis() as u64,
                "sentence index: the corpus was removed while it embedded; stopped and forgotten"
            ),
            Ok(Outcome::Ready(done)) => tracing::info!(
                target: TARGET,
                corpus = %corpus,
                sentences = done.sentences,
                embedded_now = done.embedded,
                copied = done.copied,
                embed_calls = done.calls,
                resident_bytes = done.resident_bytes,
                elapsed_ms = at.elapsed().as_millis() as u64,
                "sentence index: ready"
            ),
            Err(why) => {
                tracing::warn!(target: TARGET, corpus = %corpus, %why, elapsed_ms = at.elapsed().as_millis() as u64, "sentence index: not finished; left pending")
            }
        }
    }
}

/// How a corpus's walk ended without an error.
enum Outcome {
    /// Every pending sentence embedded.
    Ready(Done),
    /// Its directory went away mid-walk (`svrn corpus remove`), so the walk
    /// stopped there rather than embed a library nobody can search.
    Removed { embedded: usize },
}

struct Done {
    sentences: usize,
    embedded: usize,
    copied: usize,
    calls: usize,
    resident_bytes: usize,
}

/// The next pending rows from `cursor` on, all of one text, at most `batch`.
fn next_batch(
    d: &super::store::CorpusSentences,
    cursor: usize,
    batch: usize,
) -> Option<(Sha256Hash, Vec<(usize, SentenceRow)>)> {
    let first = (cursor..d.rows.len()).find(|&r| !d.embedded[r])?;
    let text = d.texts.iter().find(|t| t.rows.contains(&first))?;
    let rows = (first..text.rows.end)
        .filter(|&r| !d.embedded[r])
        .take(batch)
        .map(|r| (r, d.rows[r]))
        .collect();
    Some((text.text, rows))
}

async fn index_corpus(
    state: &AppState,
    index: &SentenceIndex,
    corpus: &str,
) -> Result<Outcome, String> {
    let cfg = index.config()?;
    let engine = state
        .inner
        .node
        .corpus_engine
        .clone()
        .ok_or("no corpus engine on this node")?;
    let local = state
        .inner
        .serving
        .local_inference
        .clone()
        .ok_or("no local inference to embed with")?;
    let ix = crate::oicp_evidence::open_for_reading(engine.as_ref(), corpus).await?;
    let chunks = ix.chunk_count().await.map_err(|e| e.to_string())?;
    if chunks > cfg.index_ceiling_chunks {
        return Err(format!(
            "{chunks} chunks, past the data file's index_ceiling_chunks ({})",
            cfg.index_ceiling_chunks
        ));
    }
    let entry = index.prepare(engine.index_dir(), corpus, &ix).await?;
    let sidecar = corpus_index::corpus::Corpus::named(engine.index_dir(), corpus)
        .ok_or("not a corpus id")?
        .sentences_path();
    // Vectors already held are this embedder's, or every row is pending
    // again: one probe before the walk, since a walk over rows that all read
    // embedded would otherwise never see the change.
    if read(&entry.data).done > 0 {
        let probe = embed(local.as_ref(), &[PROBE_TEXT.to_string()]).await?;
        let probe = unit(probe.into_iter().next().unwrap_or_default());
        let mut d = write(&entry.data);
        let current = same_embedder(&d.probe, &probe);
        tracing::debug!(target: TARGET, corpus, current, held = d.done, "sentence index: the held vectors' embedder checked");
        d.stamp(probe);
    }
    let mut known: HashMap<[u8; 32], usize> = read(&entry.data).by_key();
    let (mut cursor, mut embedded, mut copied, mut calls) = (0usize, 0usize, 0usize, 0usize);
    let mut text: Option<(Sha256Hash, String)> = None;
    let mut saved = Instant::now();
    let checkpoint = Duration::from_secs(cfg.checkpoint_secs);
    loop {
        // The corpus's directory is the only record that it is still
        // installed: removal deletes it and tells no one here.
        if !sidecar.parent().is_some_and(std::path::Path::is_dir) {
            index.forget(corpus);
            return Ok(Outcome::Removed { embedded });
        }
        let Some((name, rows)) = next_batch(&read(&entry.data), cursor, cfg.embed_batch) else {
            break;
        };
        if text.as_ref().is_none_or(|(n, _)| *n != name) {
            match ix.text(&name).await {
                Ok(Ok(stored)) => text = Some((name, stored.text)),
                Ok(Err(absence)) => {
                    return Err(format!("text {name} is no longer held ({absence:?})"))
                }
                Err(e) => return Err(format!("text {name} does not read: {e}")),
            }
        }
        let body = text.as_ref().map(|(_, t)| t.as_str()).unwrap_or_default();
        // Each key once: a sentence held already is copied, a repeat in this
        // batch shares its first's vector.
        let mut inputs: Vec<String> = Vec::new();
        let mut slots: Vec<Vec<usize>> = Vec::new();
        let mut copies: Vec<(usize, usize)> = Vec::new();
        let mut in_batch: HashMap<[u8; 32], usize> = HashMap::new();
        for (row, r) in &rows {
            if let Some(&from) = known.get(&r.key) {
                copies.push((*row, from));
            } else if let Some(&i) = in_batch.get(&r.key) {
                slots[i].push(*row);
            } else {
                in_batch.insert(r.key, inputs.len());
                inputs.push(body[r.byte_start as usize..r.byte_end as usize].to_string());
                slots.push(vec![*row]);
            }
        }
        let vectors = if inputs.is_empty() {
            None
        } else {
            inputs.push(PROBE_TEXT.to_string());
            calls += 1;
            Some(embed(local.as_ref(), &inputs).await?)
        };
        {
            let mut d = write(&entry.data);
            let first = rows.first().map_or(cursor, |(r, _)| *r);
            cursor = first;
            if let Some(mut vs) = vectors {
                // `embed` returned one per input; the probe's is last.
                let probe = unit(vs.pop().unwrap_or_default());
                // Another embedder than the vectors': every row goes pending
                // and the corpus is walked again from its first.
                if !d.probe.is_empty() && !same_embedder(&d.probe, &probe) {
                    known.clear();
                    cursor = 0;
                }
                d.stamp(probe);
                for (v, rows) in vs.into_iter().zip(&slots) {
                    let v = unit(v);
                    for &row in rows {
                        d.put(row, &v);
                    }
                    known.insert(d.rows[rows[0]].key, rows[0]);
                    embedded += rows.len();
                }
            }
            for (row, from) in copies {
                if d.embedded[from] {
                    let v = d.vector(from).to_vec();
                    d.put(row, &v);
                    copied += 1;
                }
            }
        }
        if saved.elapsed() >= checkpoint {
            save(&entry, &sidecar, corpus).await;
            saved = Instant::now();
        }
        tokio::task::yield_now().await;
    }
    save(&entry, &sidecar, corpus).await;
    let d = read(&entry.data);
    Ok(Outcome::Ready(Done {
        sentences: d.rows.len(),
        embedded,
        copied,
        calls,
        resident_bytes: d.resident_bytes(),
    }))
}

/// One batch embedded in the document space, retried; the last vector is the
/// probe's. `Err` after the last attempt names the embedder's refusal.
async fn embed(
    local: &dyn crate::state::LocalInferenceService,
    inputs: &[String],
) -> Result<Vec<Vec<f32>>, String> {
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        match local.embed_batch(inputs).await {
            Ok(v) if v.len() == inputs.len() => return Ok(v),
            Ok(v) => last = format!("{} vectors for {} inputs", v.len(), inputs.len()),
            Err(e) => last = e.to_string(),
        }
        tracing::warn!(target: TARGET, attempt, error = %last, "sentence index: an embed call failed");
        tokio::time::sleep(Duration::from_secs(u64::from(attempt) * 2)).await;
    }
    Err(format!("the embed slot refused {ATTEMPTS} times: {last}"))
}

async fn save(entry: &Arc<Entry>, sidecar: &std::path::Path, corpus: &str) {
    let at = Instant::now();
    let (entry, path) = (Arc::clone(entry), sidecar.to_path_buf());
    let saved = tokio::task::spawn_blocking(move || read(&entry.data).save(&path)).await;
    match saved {
        Ok(Ok(())) => {
            tracing::debug!(target: TARGET, corpus, elapsed_ms = at.elapsed().as_millis() as u64, "sentence index: saved")
        }
        Ok(Err(e)) => {
            tracing::warn!(target: TARGET, corpus, error = %e, "sentence index: the sidecar was not written; a restart embeds these again")
        }
        Err(e) => {
            tracing::warn!(target: TARGET, corpus, error = %e, "sentence index: the save panicked")
        }
    }
}
