// SPDX-License-Identifier: AGPL-3.0-or-later
//! The probe's `epistemic` mode (pb-cli-llm-ingest-move; it replaced the
//! `epistemic_demo` example): per question, the ledger's coverage verdict and
//! acquisition routes, from the real embed slot, svrn's
//! `runtime::epistemic::coverage_probe` and `acquisition::routes_for_gap`,
//! over the corpora the session reads through ingest's port. `RUST_LOG=
//! epistemic.ledger=debug` surfaces the resolver's ranking slate.

use std::time::Instant;

use sovereign_contracts::probe::{
    CoverageEvidence, EpistemicEvidence, ProbeEvidence, ProbeQuestion, ProbeRequest,
};
use sovereign_core::runtime::{acquisition, epistemic};

use crate::chat_cmd::bootstrap::ChatSession;

/// The mode. A session with no ingest composed fails the run with the named
/// absence: there is no corpus to probe, and an all-`TopicUncovered` slate
/// would read as a verdict.
pub(super) async fn probe(
    session: &ChatSession,
    request: &ProbeRequest,
) -> Result<ProbeEvidence, String> {
    let port = session.corpus()?;
    // An empty corpus is the all-installed scope, as the session's own.
    let scope = (!request.corpus.is_empty()).then(|| vec![request.corpus.clone()]);
    let mut rows = Vec::with_capacity(request.questions.len());
    for q in &request.questions {
        rows.push(one(session, port, scope.as_deref(), q).await);
    }
    Ok(ProbeEvidence::Epistemic { rows })
}

async fn one(
    session: &ChatSession,
    port: &std::sync::Arc<dyn corpus_index::source::CorpusReadPort>,
    scope: Option<&[String]>,
    q: &ProbeQuestion,
) -> EpistemicEvidence {
    let t = Instant::now();
    let embedding = match session.inference.embed_query(&q.question).await {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(id = %q.id, error = %e, "epistemic probe: embed failed");
            return EpistemicEvidence {
                id: q.id.clone(),
                error: Some(format!("embed: {e}")),
                coverage: None,
                routes: Vec::new(),
                embed_ms: t.elapsed().as_millis() as u64,
                probe_ms: 0,
                resolve_ms: 0,
            };
        }
    };
    let embed_ms = t.elapsed().as_millis() as u64;

    let t = Instant::now();
    let probe = epistemic::coverage_probe(Some(port), &embedding, scope).await;
    let probe_ms = t.elapsed().as_millis() as u64;

    // The probe's own Option, never a defaulted verdict (RouteContext's
    // contract): no probe, no authority claim.
    let ctx = acquisition::RouteContext {
        engine: Some(std::sync::Arc::clone(port)),
        coverage: probe.as_ref().map(|p| p.verdict),
    };
    let t = Instant::now();
    let routes = acquisition::routes_for_gap(session.inference.as_ref(), &ctx, &q.question).await;
    let resolve_ms = t.elapsed().as_millis() as u64;
    tracing::debug!(
        id = %q.id,
        probed = probe.is_some(),
        routes = routes.len(),
        embed_ms,
        probe_ms,
        resolve_ms,
        "epistemic probe"
    );
    EpistemicEvidence {
        id: q.id.clone(),
        error: None,
        coverage: probe.map(|p| CoverageEvidence {
            verdict: p.verdict,
            best_similarity: p.best_similarity,
            best_corpus: p.best_corpus,
        }),
        routes,
        embed_ms,
        probe_ms,
        resolve_ms,
    }
}
