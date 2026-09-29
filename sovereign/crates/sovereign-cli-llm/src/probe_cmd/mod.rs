// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn __probe --request <file> --output <file>` — svrn describes its own
//! internals for a bench to judge (phase-b-58; ARCH principle 12).
//!
//! Hidden from `svrn --help`: it is the wire between svrn and bench's
//! `svrn eval run --routing-only | --prod-pipeline | <raw-index>`, which exec
//! it and score what it writes. It runs ONE internal stage per question —
//! the router's classifier, the production retrieval pipeline, or a raw index
//! search — and writes a `sovereign_contracts::probe::ProbeEvidence`: raw
//! classifications and pools, no bank, no expectation, no verdict. svrn's
//! global flags (`--daemon`, `--data-dir`, models, `--temperature`) build the
//! session exactly as they do for any other verb.

mod prod;
mod retrieve;
mod routing;

use std::path::PathBuf;

use corpus_index::types::ScoredChunk;
use sovereign_contracts::probe::{PoolChunk, ProbeEvidence, ProbeMode, ProbeRequest};

use crate::chat_cmd::{
    bootstrap::{build_session, build_session_sealed, ChatSession},
    config::parse_globals,
};

pub use retrieve::load_atlas_context;
pub(crate) use retrieve::load_atlases;

/// A bench bank names the ONE corpus it questions, so the session it drives
/// is sealed to that corpus and the cross-corpus lane members are not loaded
/// (see `bootstrap::build_session_sealed` for the measured cost).
///
/// A bank that names no corpus is the honest exception: nothing has been
/// resolved to seal to, so the scope stays `All` rather than being sealed to
/// an empty id that matches nothing (ARCH §18.3 — never silently substitute).
pub(crate) async fn build_session_for_bank(
    globals: &crate::chat_cmd::config::ChatGlobals,
    corpus: &str,
) -> sovereign_core::error::Result<ChatSession> {
    if corpus.trim().is_empty() {
        // SAID, not silent. The scope widens back to every corpus here, and
        // a run that pays the cross-corpus startup should say why it did
        // (ARCH §18.3, §9.1).
        eprintln!(
            "lane scope: bank names no corpus, so the session loads every corpus \
             (cross-corpus enrichment members included)"
        );
        build_session(globals).await
    } else {
        build_session_sealed(globals, corpus).await
    }
}

/// One pool row as the evidence file carries it: the fields a judge reads.
fn pool_chunk(c: &ScoredChunk) -> PoolChunk {
    PoolChunk {
        corpus_id: c.corpus_id.clone(),
        title: c.title.clone(),
        url: c.url.clone(),
        score: c.score,
        content: c.content.clone(),
    }
}

/// The verb. Exit 0 with the evidence written, 1 when the stage could not
/// run, 2 on a malformed command line.
pub async fn run(args: &[String]) -> i32 {
    let (globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let (request_path, output_path) = match request_and_output(&rest) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: svrn __probe: {e}");
            return 2;
        }
    };
    let request: ProbeRequest = match std::fs::read(&request_path)
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: svrn __probe: request {}: {e}", request_path.display());
            return 2;
        }
    };
    sovereign_cli_shared::tracing_init::init_tracing(
        "sovereign_cli=info,sovereign_tools::atlas_context_manager=info,\
         sovereign_tools::knowledge_view=warn",
    );
    tracing::debug!(
        mode = request.mode.as_str(),
        questions = request.questions.len(),
        corpus = %request.corpus,
        "probe request"
    );
    let session = match build_session_for_bank(&globals, &request.corpus).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bootstrap failed: {e}");
            return 1;
        }
    };

    let evidence = match probe(&session, &request).await {
        Ok(ev) => ev,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let written = serde_json::to_vec(&evidence)
        .map_err(|e| e.to_string())
        .and_then(|b| std::fs::write(&output_path, b).map_err(|e| e.to_string()));
    if let Err(e) = written {
        eprintln!("error: svrn __probe: write {}: {e}", output_path.display());
        return 1;
    }
    0
}

/// Run the request's stage over its questions.
async fn probe(session: &ChatSession, request: &ProbeRequest) -> Result<ProbeEvidence, String> {
    // Atlases load in the two pool modes, and a bag that will not load fails
    // the run in both, as it did when these modes lived in `eval run`. Only
    // the retrieve probe walks them.
    let (atlases, graphs) = match (&request.atlas, request.mode) {
        (Some(atlas), ProbeMode::Prod | ProbeMode::Retrieve) => {
            load_atlases(session, atlas).await?
        }
        _ => (Vec::new(), Vec::new()),
    };
    Ok(match request.mode {
        ProbeMode::Routing => ProbeEvidence::Routing {
            rows: routing::probe(session, &request.questions).await,
        },
        ProbeMode::Prod => {
            let isolate_corpora = request.isolate.then(|| vec![request.corpus.clone()]);
            let rows = prod::probe(
                session,
                &request.questions,
                request.limit,
                isolate_corpora.as_deref(),
            )
            .await;
            ProbeEvidence::Prod {
                rows,
                ledger_violations: sovereign_core::runtime::retrieval_pipeline::ledger_violation_count()
                    as u64,
            }
        }
        ProbeMode::Retrieve => ProbeEvidence::Retrieve {
            rows: retrieve::probe(
                session,
                &request.corpus,
                &request.questions,
                request.limit,
                &atlases,
                &graphs,
                request.atlas.as_ref().map_or(sovereign_contracts::probe::SeedMode::Cosine, |a| a.seed),
            )
            .await?,
        },
    })
}

/// `--request <file> --output <file>`, both required, nothing else.
fn request_and_output(rest: &[String]) -> Result<(PathBuf, PathBuf), String> {
    let (mut request, mut output) = (None, None);
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let slot = match flag.as_str() {
            "--request" => &mut request,
            "--output" => &mut output,
            other => return Err(format!("unexpected argument `{other}`")),
        };
        let Some(v) = it.next() else {
            return Err(format!("{flag} needs a value"));
        };
        *slot = Some(PathBuf::from(v));
    }
    match (request, output) {
        (Some(r), Some(o)) => Ok((r, o)),
        _ => Err("usage: svrn __probe --request <file> --output <file>".into()),
    }
}
