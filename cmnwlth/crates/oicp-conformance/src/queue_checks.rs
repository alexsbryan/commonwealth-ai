// SPDX-License-Identifier: AGPL-3.0-or-later
//! v0.6's sentence-queue check (oicp-v0.6.md §2.11, §6):
//! `knowledge.sentence_queue`. It installs two corpora of its own, one
//! behind the other, and searches both at sentence granularity until the
//! host answers on the `index` route; every `passages` reply on the way must
//! name each waiting corpus's place, embedding now or queued behind the
//! corpora ahead of it, and name no corpus ahead that has finished.

use std::time::Duration;

use oicp_types::{features, IndexPlace, KnowledgeSearchResponse, ProviderManifest, SentenceRoute};
use serde::Deserialize;
use serde_json::json;

use crate::checks::Host;
use crate::report::{Check, Level};
use crate::shelf::Shelf;

/// Polls before the host is judged to have never finished indexing.
const POLLS: usize = 240;
const WAIT: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueShelf {
    shelf: Shelf,
    behind_corpus_id: String,
    query: String,
}

/// What a `passages` reply over `scope` says wrongly about its queue.
pub fn sentence_queue_failures(scope: &[String], reply: &KnowledgeSearchResponse) -> Vec<String> {
    let mut f = Vec::new();
    let places = match (reply.route, &reply.sentence_queue) {
        (Some(SentenceRoute::Passages), None) => {
            return vec!["route `passages` without `sentence_queue`".into()]
        }
        (Some(SentenceRoute::Index), Some(_)) => {
            return vec!["route `index` with `sentence_queue`".into()]
        }
        (_, Some(p)) => p,
        _ => return f,
    };
    let embedding: Vec<&str> = places
        .iter()
        .filter(|p| p.place == IndexPlace::Embedding)
        .map(|p| p.corpus.as_str())
        .collect();
    if embedding.len() > 1 {
        f.push(format!(
            "{} corpora answered embedding now ({}); the host embeds one at a time",
            embedding.len(),
            embedding.join(", ")
        ));
    }
    for (i, p) in places.iter().enumerate() {
        let c = &p.corpus;
        if !scope.contains(c) {
            f.push(format!("`{c}` is placed and was not in the request"));
        }
        if places[..i].iter().any(|q| &q.corpus == c) {
            f.push(format!("`{c}` is placed twice"));
        }
        if p.pending == 0 {
            f.push(format!("`{c}` is placed with no sentence pending"));
        }
        match (p.place, p.behind.is_empty()) {
            (IndexPlace::Queued, true) => f.push(format!("`{c}` is queued behind nothing")),
            (IndexPlace::Embedding, false) => f.push(format!(
                "`{c}` is embedding now and names {} corpora ahead of it",
                p.behind.len()
            )),
            _ => {}
        }
        if let (IndexPlace::Queued, Some(e), Some(first)) =
            (p.place, embedding.first(), p.behind.first())
        {
            if first.corpus != *e {
                f.push(format!(
                    "`{c}` is queued behind `{}` first; `{e}` is embedding now",
                    first.corpus
                ));
            }
        }
        for (j, b) in p.behind.iter().enumerate() {
            if &b.corpus == c || p.behind[..j].iter().any(|x| x.corpus == b.corpus) {
                f.push(format!("`{c}` names `{}` ahead twice or itself", b.corpus));
            }
            // A corpus of this request named ahead must itself be waiting.
            let own = places.iter().find(|q| q.corpus == b.corpus);
            if scope.contains(&b.corpus) && own.is_none() {
                f.push(format!(
                    "`{c}` is queued behind `{}`, which has no sentence pending in this reply \
                     (it has finished)",
                    b.corpus
                ));
            }
            if let (Some(own), Some(n)) = (own, b.pending) {
                if own.pending != n {
                    f.push(format!(
                        "`{}` has {} pending as placed and {n} as named ahead of `{c}`",
                        b.corpus, own.pending
                    ));
                }
            }
        }
    }
    f
}

/// `knowledge.sentence_queue` (§2.11).
pub async fn check_knowledge_sentence_queue(host: &Host, m: &ProviderManifest) -> Check {
    let id = "knowledge.sentence_queue";
    let fail = |why: String| Check::fail(id, Level::Feature, why);
    if !m.has_feature(features::KNOWLEDGE_SENTENCE_QUEUE) {
        return Check::skip(
            id,
            Level::Feature,
            "knowledge:sentence_queue not advertised",
        );
    }
    let (Some(k), true) = (m.knowledge.as_ref(), m.has_feature(features::INGEST_RECIPE)) else {
        return Check::skip(
            id,
            Level::Feature,
            "could not judge: the queue shelves are installed by ingest:recipe",
        );
    };
    let fx: QueueShelf = match serde_json::from_str(include_str!("../fixture/queue.json")) {
        Ok(x) => x,
        Err(e) => return fail(format!("fixture/queue.json does not load: {e}")),
    };
    let behind = Shelf {
        corpus_id: fx.behind_corpus_id.clone(),
        ..fx.shelf.clone()
    };
    for shelf in [&fx.shelf, &behind] {
        if let Err(e) = shelf.install(host, m, &[]).await {
            return fail(e);
        }
    }
    let scope = vec![fx.shelf.corpus_id.clone(), fx.behind_corpus_id.clone()];
    let body = json!({"query": fx.query, "corpora": scope, "limit": 20, "granularity": "sentence"});
    let mut passages = 0usize;
    for _ in 0..POLLS {
        let r = match host.post(&k.search_endpoint, &body).await {
            Ok(r) => r,
            Err(e) => return fail(e),
        };
        if r.status == 503 {
            return Check::skip(
                id,
                Level::Feature,
                format!("could not judge: {}", r.brief()),
            );
        }
        if r.status != 200 {
            return fail(format!("sentence search: {}", r.brief()));
        }
        let reply: KnowledgeSearchResponse = match serde_json::from_value(r.body.clone()) {
            Ok(x) => x,
            Err(e) => return fail(format!("the sentence reply does not decode: {e}")),
        };
        let bad = sentence_queue_failures(&scope, &reply);
        if !bad.is_empty() {
            return fail(bad.join("; "));
        }
        match reply.route {
            Some(SentenceRoute::Index) if passages == 0 => {
                return Check::skip(
                    id,
                    Level::Feature,
                    "could not judge: the host answered `index` at the first search, with \
                     nothing waiting to place",
                )
            }
            Some(SentenceRoute::Index) => {
                return Check::pass(
                    id,
                    Level::Feature,
                    format!(
                        "{passages} `passages` repl(ies) placed every waiting corpus, none \
                         embedding twice or behind one that had finished; then `index`"
                    ),
                )
            }
            _ => passages += 1,
        }
        tokio::time::sleep(WAIT).await;
    }
    Check::skip(
        id,
        Level::Feature,
        format!("could not judge: still `passages` after {POLLS} searches"),
    )
}
