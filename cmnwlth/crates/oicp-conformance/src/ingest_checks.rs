// SPDX-License-Identifier: AGPL-3.0-or-later
//! v0.5's ingest checks: install with a recipe (§4), which also installs the
//! fixture library the evidence checks read, and the recipe test v0.4 §10
//! claimed and the suite never had (§6, appendix defect 3).

use std::time::Duration;

use oicp_types::{features, CorpusInstallResponse, ProviderManifest, RecipeTestReport};
use serde_json::json;

use crate::args::Args;
use crate::checks::Host;
use crate::fixture::{sha256_hex, Library};
use crate::report::{Check, Level};

/// Whether the fixture library is installed on the host, and if not, why:
/// the evidence checks cannot judge without it and say so by this reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixtureState {
    /// Installed as this corpus.
    Installed(String),
    /// Not installed, and why.
    Unavailable(String),
}

/// A recipe no host can load.
const BAD_RECIPE: &str = "this is [not a recipe = = at all";
const BAD_CORPUS: &str = "oicp-conformance-bad-recipe";

/// How long the fixture's ingest may take to reach a terminal phase.
const INSTALL_POLLS: u32 = 180;
/// v0.4 §5.3 rule 4: no progress entry within this many polls means the
/// corpus was already installed.
const GRACE_POLLS: u32 = 15;

/// The recipe this run installs: inline, or from `--fixture-dir` after
/// writing the documents there.
pub fn fixture_recipe(lib: &Library, args: &Args) -> Result<String, String> {
    match &args.fixture_dir {
        Some(dir) => {
            let dir = std::path::Path::new(dir);
            lib.write_dir(dir)
                .map_err(|e| format!("could not write the fixture to {}: {e}", dir.display()))?;
            Ok(lib.dir_recipe(dir))
        }
        None => Ok(lib.inline_recipe()),
    }
}

/// `ingest.recipe` (v0.5 §4): a bad recipe is a 400; the fixture recipe
/// installs and reaches a terminal phase; the reply names the recipe's
/// sha256; an identical second install is `spawned: false`.
pub async fn check_ingest_recipe(
    host: &Host,
    m: &ProviderManifest,
    args: &Args,
    lib: &Library,
) -> (Check, FixtureState) {
    let id = "ingest.recipe";
    let unavailable = |why: String| FixtureState::Unavailable(why);
    if !m.has_feature(features::INGEST_RECIPE) {
        let why = "ingest:recipe not advertised".to_string();
        return (Check::skip(id, Level::Feature, &why), unavailable(why));
    }
    let Some(ing) = m.knowledge.as_ref().and_then(|k| k.ingest.as_ref()) else {
        let why = "ingest:recipe advertised but no knowledge.ingest".to_string();
        return (Check::fail(id, Level::Feature, &why), unavailable(why));
    };
    let fail = |why: String| {
        (
            Check::fail(id, Level::Feature, &why),
            FixtureState::Unavailable(format!("ingest.recipe failed: {why}")),
        )
    };
    let recipe = match fixture_recipe(lib, args) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };

    let bad = json!({"corpus_id": BAD_CORPUS, "recipe_toml": BAD_RECIPE});
    match host.post(&ing.install_endpoint, &bad).await {
        Ok(r) if r.status == 400 => {}
        Ok(r) => {
            return fail(format!(
                "a recipe that does not parse must be 400, got {}",
                r.brief()
            ))
        }
        Err(e) => return fail(e),
    }

    let install = json!({"corpus_id": lib.corpus_id, "recipe_toml": recipe});
    let want_sha = sha256_hex(recipe.as_bytes());
    let mut replies = Vec::new();
    for _ in 0..2 {
        let r = match host.post(&ing.install_endpoint, &install).await {
            Ok(r) if r.status == 200 => r,
            Ok(r) => return fail(format!("install with recipe_toml: {}", r.brief())),
            Err(e) => return fail(e),
        };
        match serde_json::from_value::<CorpusInstallResponse>(r.body.clone()) {
            Ok(resp) => replies.push(resp),
            Err(e) => return fail(format!("install reply does not decode: {e}: {}", r.brief())),
        }
        // v0.6 §2.9: an install that spawned is listed on the very next poll.
        if replies.len() == 1 && replies[0].spawned && m.has_feature(features::INGEST_DOCUMENTS) {
            let r = match host.get(&ing.progress_endpoint).await {
                Ok(r) => r,
                Err(e) => return fail(e),
            };
            if r.body
                .get("progress")
                .and_then(|p| p.get(&lib.corpus_id))
                .is_none()
            {
                return fail(format!(
                    "install answered spawned=true and the next poll did not list {}",
                    lib.corpus_id
                ));
            }
        }
    }
    for r in &replies {
        if r.recipe_sha256.as_deref() != Some(want_sha.as_str()) {
            return fail(format!(
                "recipe_sha256 must be sha256 of the recipe_toml bytes ({want_sha}), got {:?}",
                r.recipe_sha256
            ));
        }
    }
    if replies[1].spawned {
        return fail("an identical second install returned spawned=true (not idempotent)".into());
    }
    if let Err(e) = wait_terminal(host, &ing.progress_endpoint, &lib.corpus_id).await {
        return fail(e);
    }
    (
        Check::pass(
            id,
            Level::Feature,
            format!(
                "400 on a bad recipe; {} installed (first spawned={}), identical re-install \
                 spawned=false, recipe_sha256 matches",
                lib.corpus_id, replies[0].spawned
            ),
        ),
        FixtureState::Installed(lib.corpus_id.clone()),
    )
}

/// Poll progress by v0.4 §5.3 until `corpus` is terminal: `complete`, an
/// entry that disappears after being seen, or none within the grace window
/// (already installed). `failed` and a time-out are errors.
async fn wait_terminal(host: &Host, progress: &str, corpus: &str) -> Result<(), String> {
    let mut seen = false;
    for poll in 0..INSTALL_POLLS {
        let r = host.get(progress).await?;
        let entry = r.body.get("progress").and_then(|p| p.get(corpus));
        match (
            entry,
            entry.and_then(|e| e.get("phase")).and_then(|p| p.as_str()),
        ) {
            (Some(_), Some("complete")) => return Ok(()),
            (Some(e), Some("failed")) => return Err(format!("the fixture's ingest failed: {e}")),
            (Some(_), _) => seen = true,
            (None, _) if seen => return Ok(()),
            (None, _) if poll >= GRACE_POLLS => return Ok(()),
            (None, _) => {}
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(format!(
        "the fixture's ingest did not reach a terminal phase in {INSTALL_POLLS}s"
    ))
}

/// The stages a recipe test must report, in order (v0.4 §5.4).
const STAGES: [&str; 3] = ["acquire", "extract", "chunk"];

/// `ingest.recipe_test` (v0.4 §5.4, owed since §10): the fixture recipe's
/// dry run reports acquire, extract and chunk in that order, each with
/// output, and `ok`. Other stages (the reference host's `validate`) may
/// appear among them.
pub async fn check_ingest_recipe_test(
    host: &Host,
    m: &ProviderManifest,
    args: &Args,
    lib: &Library,
) -> Check {
    let id = "ingest.recipe_test";
    if !m.has_feature(features::INGEST_RECIPE_TEST) {
        return Check::skip(id, Level::Feature, "ingest:recipe_test not advertised");
    }
    let Some(endpoint) = m
        .knowledge
        .as_ref()
        .and_then(|k| k.ingest.as_ref())
        .and_then(|i| i.test_endpoint.as_ref())
    else {
        return Check::fail(
            id,
            Level::Feature,
            "ingest:recipe_test advertised but no test_endpoint",
        );
    };
    // The inline form is v0.5 recipe format; a host that does not take
    // recipes over the wire was never promised it.
    if args.fixture_dir.is_none() && !m.has_feature(features::INGEST_RECIPE) {
        return Check::skip(
            id,
            Level::Feature,
            "could not judge: the fixture's inline documents are v0.5 recipe format and the \
             host does not advertise ingest:recipe; pass --fixture-dir on a local host",
        );
    }
    let recipe = match fixture_recipe(lib, args) {
        Ok(r) => r,
        Err(e) => return Check::fail(id, Level::Feature, e),
    };
    let body = json!({"recipe_toml": recipe, "options": {"sample_limit": lib.documents.len()}});
    let r = match host.post(endpoint, &body).await {
        Ok(r) if r.status == 200 => r,
        Ok(r) => return Check::fail(id, Level::Feature, format!("recipe test: {}", r.brief())),
        Err(e) => return Check::fail(id, Level::Feature, e),
    };
    let report: RecipeTestReport = match serde_json::from_value(r.body.clone()) {
        Ok(rep) => rep,
        Err(e) => return Check::fail(id, Level::Feature, format!("report does not decode: {e}")),
    };
    let names: Vec<&str> = report.stages.iter().map(|s| s.name.as_str()).collect();
    let mut at = 0;
    for want in STAGES {
        match names[at..].iter().position(|n| *n == want) {
            Some(i) => at += i + 1,
            None => {
                return Check::fail(
                    id,
                    Level::Feature,
                    format!("stage `{want}` missing or out of order: stages {names:?}"),
                )
            }
        }
    }
    let empty: Vec<&str> = report
        .stages
        .iter()
        .filter(|s| STAGES.contains(&s.name.as_str()) && s.docs_out == 0)
        .map(|s| s.name.as_str())
        .collect();
    if !empty.is_empty() {
        return Check::fail(
            id,
            Level::Feature,
            format!("stages with no output on the fixture: {empty:?}"),
        );
    }
    if !report.ok {
        return Check::fail(
            id,
            Level::Feature,
            "every stage produced output but ok=false",
        );
    }
    Check::pass(id, Level::Feature, format!("stages {names:?}, ok"))
}

/// `ingest.documents` (v0.6 §2.9): once the fixture's install has returned,
/// its progress entry lists every fixture document `ready` under its name,
/// and none `failed`, since every fixture text is readable. On a host with
/// `evidence:text`, each ready document's text name reads back a text that
/// holds the document's signature. A fixture the host held before this run
/// (its install `spawned: false`) is listed all the same.
pub async fn check_ingest_documents(
    host: &Host,
    m: &ProviderManifest,
    lib: &Library,
    state: &FixtureState,
) -> Check {
    let id = "ingest.documents";
    let fail = |why: String| Check::fail(id, Level::Feature, why);
    if !m.has_feature(features::INGEST_DOCUMENTS) {
        return Check::skip(id, Level::Feature, "ingest:documents not advertised");
    }
    let corpus = match crate::evidence_checks::fixture_corpus(id, state) {
        Ok(c) => c,
        Err(skip) => return skip,
    };
    let Some(ing) = m.knowledge.as_ref().and_then(|k| k.ingest.as_ref()) else {
        return fail("ingest:documents advertised but no knowledge.ingest".into());
    };
    let r = match host.get(&ing.progress_endpoint).await {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let reply: oicp_types::CorpusProgressResponse = match serde_json::from_value(r.body.clone()) {
        Ok(p) => p,
        Err(e) => return fail(format!("progress does not decode: {e}")),
    };
    let Some(entry) = reply.progress.get(corpus) else {
        return fail(format!(
            "the fixture {corpus} is held and progress does not list it: a held corpus is \
             listed with its documents, across the host's restarts"
        ));
    };
    if entry.documents.is_empty() {
        return fail(format!(
            "the fixture's install returned ({:?}) and its entry lists no documents",
            entry.phase
        ));
    }
    // A ready document's text name is the name `evidence:text` reads it by
    // (§2.9), the host's stored text, which a host may normalize: so it is
    // read back and judged by the document's signature, never by the sha256
    // of the fixture's bytes.
    let text = m
        .knowledge
        .as_ref()
        .and_then(|k| k.evidence.as_ref())
        .filter(|_| m.has_feature(features::EVIDENCE_TEXT))
        .map(|ev| ev.text_endpoint.as_str());
    let corpus_q = format!("?corpus={corpus}");
    for doc in &lib.documents {
        let listed = entry
            .documents
            .iter()
            .find(|d| d.source.as_deref() == Some(doc.name.as_str()));
        let sha = match listed.map(|d| &d.state) {
            None => {
                return fail(format!(
                    "fixture document `{}` is not listed by its name",
                    doc.name
                ))
            }
            Some(oicp_types::IngestDocumentState::Failed { reason }) => {
                return fail(format!(
                    "fixture document `{}` is listed failed: {reason}",
                    doc.name
                ))
            }
            Some(oicp_types::IngestDocumentState::Ready { text_sha256 }) => text_sha256,
        };
        let (Some(endpoint), Some(sha)) = (text, sha) else {
            continue;
        };
        let whole = match crate::evidence_checks::read(host, endpoint, sha, &corpus_q).await {
            Ok(w) => w,
            Err(e) => {
                return fail(format!(
                    "fixture document `{}` is listed ready as {}… and its text does not read: {e}",
                    doc.name,
                    &sha[..12.min(sha.len())]
                ))
            }
        };
        let signature = lib.signatures.get(&doc.name).map_or("", String::as_str);
        if !whole.text.contains(signature) {
            return fail(format!(
                "fixture document `{}` is listed ready as {}… and that text does not hold its \
                 signature",
                doc.name,
                &sha[..12.min(sha.len())]
            ));
        }
    }
    if let Some(d) = entry
        .documents
        .iter()
        .find(|d| matches!(d.state, oicp_types::IngestDocumentState::Failed { .. }))
    {
        return fail(format!(
            "a readable fixture document is listed failed: {d:?}"
        ));
    }
    Check::pass(
        id,
        Level::Feature,
        format!(
            "all {} fixture documents listed ready by name, each text name reading back its signature",
            lib.documents.len()
        ),
    )
}
