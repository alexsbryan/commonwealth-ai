// SPDX-License-Identifier: AGPL-3.0-or-later
//! The worker's queue, and what a sentence search reads of it (OICP v0.6
//! §2.11, `knowledge:sentence_queue`). [`Fifo`] is the one decider of the
//! order: the worker takes from it, a search only takes a [`QueueSnapshot`]
//! of it, and [`places`] turns that snapshot into the wire's places without
//! re-deriving any order.

use std::collections::{HashSet, VecDeque};

use oicp_types::{IndexPlace, QueuedCorpus, SentencePlace};

use super::TARGET;

#[derive(Default)]
pub(super) struct Fifo {
    order: VecDeque<String>,
    queued: HashSet<String>,
    embedding: Option<String>,
}

/// The queue at one instant: the corpus embedding now and those waiting, in
/// the order the worker will take them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueueSnapshot {
    pub embedding: Option<String>,
    pub waiting: Vec<String>,
}

impl Fifo {
    /// Whether `corpus` joined the line (it was not already waiting).
    pub(super) fn push(&mut self, corpus: &str) -> bool {
        let fresh = self.queued.insert(corpus.to_string());
        if fresh {
            self.order.push_back(corpus.to_string());
        }
        fresh
    }

    /// The worker takes the next corpus; it is embedding until [`finish`].
    pub(super) fn take(&mut self) -> Option<String> {
        let next = self.order.pop_front()?;
        self.queued.remove(&next);
        self.embedding = Some(next.clone());
        Some(next)
    }

    /// The worker is done with the corpus it took, ready or left pending.
    pub(super) fn finish(&mut self) {
        self.embedding = None;
    }

    pub(super) fn snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            embedding: self.embedding.clone(),
            waiting: self
                .order
                .iter()
                .filter(|c| self.embedding.as_ref() != Some(*c))
                .cloned()
                .collect(),
        }
    }
}

impl QueueSnapshot {
    /// The corpora the worker takes before `corpus`, the one embedding now
    /// first; `None` when `corpus` is in neither list.
    fn ahead_of(&self, corpus: &str) -> Option<Vec<&str>> {
        let at = self.waiting.iter().position(|c| c == corpus)?;
        Some(
            self.embedding
                .iter()
                .chain(&self.waiting[..at])
                .map(String::as_str)
                .collect(),
        )
    }

    fn rank(&self, corpus: &str) -> usize {
        if self.embedding.as_deref() == Some(corpus) {
            0
        } else {
            self.waiting
                .iter()
                .position(|c| c == corpus)
                .map_or(usize::MAX, |i| i + 1)
        }
    }
}

/// The places of the scoped corpora that wait (`scoped`: each with its own
/// pending count), in the worker's order. `pending_of` counts a corpus ahead
/// that is not in scope, `None` when the host has not opened it.
pub fn places(
    snapshot: &QueueSnapshot,
    scoped: &[(String, u64)],
    pending_of: impl Fn(&str) -> Option<u64>,
) -> Vec<SentencePlace> {
    let mut out: Vec<SentencePlace> = scoped
        .iter()
        .map(|(corpus, pending)| {
            let (place, ahead) = if snapshot.embedding.as_deref() == Some(corpus) {
                (IndexPlace::Embedding, Vec::new())
            } else if let Some(ahead) = snapshot.ahead_of(corpus) {
                // Nothing ahead and the worker idle: it takes this one at
                // once (the enqueue woke it), so it is embedding, not queued
                // behind nothing.
                match ahead.is_empty() {
                    true => (IndexPlace::Embedding, ahead),
                    false => (IndexPlace::Queued, ahead),
                }
            } else {
                // Enqueued by the search that asked, and gone from both lists
                // by now: the worker took it and finished between the two.
                // Named queued behind whatever remains, never embedding.
                tracing::warn!(target: TARGET, %corpus, "sentence index: pending and in no queue list; placed behind the whole line");
                let all = snapshot.embedding.iter().chain(&snapshot.waiting);
                (
                    IndexPlace::Queued,
                    all.map(String::as_str).collect::<Vec<_>>(),
                )
            };
            let behind = ahead
                .into_iter()
                .map(|c| QueuedCorpus {
                    corpus: c.to_string(),
                    pending: scoped
                        .iter()
                        .find(|(s, _)| s == c)
                        .map(|(_, n)| *n)
                        .or_else(|| pending_of(c)),
                })
                .collect();
            tracing::debug!(target: TARGET, %corpus, ?place, pending, "sentence index: placed");
            SentencePlace {
                corpus: corpus.clone(),
                pending: *pending,
                place,
                behind,
            }
        })
        .collect();
    out.sort_by_key(|p| snapshot.rank(&p.corpus));
    out
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
