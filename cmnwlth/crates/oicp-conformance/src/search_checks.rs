// SPDX-License-Identifier: AGPL-3.0-or-later
//! v0.6's search checks (oicp-v0.6.md §2.8, §6) over the fixture library:
//! `knowledge.range`, every range a hit gives dereferences to its `exact`
//! through the text endpoint, and `knowledge.scoped`, a `documents` filter
//! returns hits from the named texts only and refuses what is not a text name;
//! and `knowledge.sentences` (§2.11), sentence hits read back the same way.

use std::collections::BTreeMap;

use oicp_types::evidence::reasons;
use oicp_types::knowledge::reasons::SCORER_UNAVAILABLE;
use oicp_types::{
    features, KnowledgeResult, KnowledgeSearchResponse, ProviderManifest, SentenceRoute,
    SentenceScorer,
};
use serde_json::{json, Value};

use crate::checks::Host;
use crate::evidence_checks::{fixture_corpus, read};
use crate::fixture::{code_points, sha256_hex, Library};
use crate::ingest_checks::FixtureState;
use crate::report::{Check, Level};

/// The v0.6 search checks over the fixture library, in order: `knowledge.range`,
/// `knowledge.scoped`, `knowledge.sentences`, `knowledge.priority`.
pub async fn search_checks(
    host: &Host,
    m: &ProviderManifest,
    lib: &Library,
    state: &FixtureState,
) -> Vec<Check> {
    vec![
        check_knowledge_range(host, m, lib, state).await,
        check_knowledge_scoped(host, m, lib, state).await,
        check_knowledge_sentences(host, m, lib, state).await,
        crate::priority_checks::check_knowledge_priority(host, m, lib, state).await,
    ]
}

/// `knowledge.search`'s ranged half (§2.8): with `knowledge:range`, every
/// hit carries a `span` into the text its `document` names, or names why not
/// with one of [`reasons::SPAN_ABSENCES`]; never both, never neither.
pub fn range_hit_failures(hits: &[KnowledgeResult]) -> Vec<String> {
    let mut f = Vec::new();
    for (i, hit) in hits.iter().enumerate() {
        match (&hit.span, hit.span_absent.as_deref()) {
            (Some(_), Some(r)) => f.push(format!("hit #{i} carries a span and names `{r}`")),
            (None, None) => f.push(format!("hit #{i} carries no span and names no reason")),
            (None, Some(r)) if !reasons::SPAN_ABSENCES.contains(&r) => {
                f.push(format!("hit #{i} names an unknown span absence `{r}`"))
            }
            (Some(s), None) => {
                if hit.document.as_ref().map(|d| &d.text_sha256) != Some(&s.document.text_sha256) {
                    f.push(format!(
                        "hit #{i}'s span points into a text its document is not"
                    ));
                }
            }
            (None, Some(_)) => {}
        }
    }
    f
}

/// [`range_hit_failures`] over a search reply's raw hits, when the host
/// advertises `knowledge:range`; nothing otherwise.
pub fn range_failures_if_advertised(m: &ProviderManifest, hits: &[Value]) -> Vec<String> {
    if !m.has_feature(features::KNOWLEDGE_RANGE) {
        return Vec::new();
    }
    match serde_json::from_value::<Vec<KnowledgeResult>>(Value::Array(hits.to_vec())) {
        Ok(typed) => range_hit_failures(&typed),
        Err(e) => vec![format!("hits do not decode: {e}")],
    }
}

pub(crate) async fn search_body(
    host: &Host,
    m: &ProviderManifest,
    body: &Value,
) -> Result<crate::wire::Reply, String> {
    let k = m.knowledge.as_ref().ok_or("no knowledge plane")?;
    host.post(&k.search_endpoint, body).await
}

pub(crate) async fn hits(
    host: &Host,
    m: &ProviderManifest,
    body: &Value,
) -> Result<Vec<KnowledgeResult>, String> {
    let r = search_body(host, m, body).await?;
    if r.status != 200 {
        return Err(format!("search: {}", r.brief()));
    }
    let results = r.body.get("results").cloned().unwrap_or(Value::Null);
    serde_json::from_value(results).map_err(|e| format!("search results do not decode: {e}"))
}

/// `knowledge.range` (§2.8): each fixture document's signature searched,
/// and every span on every hit read back through the text endpoint: the
/// text's `[start, end)` is the span's `exact`. The fixture's texts are
/// plain, so a host that ranges none of their hits fails.
pub async fn check_knowledge_range(
    host: &Host,
    m: &ProviderManifest,
    lib: &Library,
    state: &FixtureState,
) -> Check {
    let id = "knowledge.range";
    let fail = |why: String| Check::fail(id, Level::Feature, why);
    if !m.has_feature(features::KNOWLEDGE_RANGE) {
        return Check::skip(id, Level::Feature, "knowledge:range not advertised");
    }
    let Some(ev) = m.knowledge.as_ref().and_then(|k| k.evidence.as_ref()) else {
        return fail(
            "knowledge:range advertised but no knowledge.evidence to read a range back".into(),
        );
    };
    let corpus = match fixture_corpus(id, state) {
        Ok(c) => c,
        Err(skip) => return skip,
    };
    let corpus_q = format!("?corpus={corpus}");
    let (mut ranged, mut absent) = (0usize, 0usize);
    for sig in lib.signatures.values() {
        let found = match hits(
            host,
            m,
            &json!({"query": sig, "corpora": [corpus], "limit": 10}),
        )
        .await
        {
            Ok(h) => h,
            Err(e) => return fail(e),
        };
        let bad = range_hit_failures(&found);
        if !bad.is_empty() {
            return fail(bad.join("; "));
        }
        for hit in &found {
            let Some(span) = &hit.span else {
                absent += 1;
                continue;
            };
            let whole = match read(
                host,
                &ev.text_endpoint,
                &span.document.text_sha256,
                &corpus_q,
            )
            .await
            {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let there = code_points(&whole.text, span.start, span.end);
            if there.as_deref() != Some(span.exact.as_str()) {
                return fail(format!(
                    "a hit's span [{}, {}) says {:?}; the text there says {there:?}",
                    span.start, span.end, span.exact
                ));
            }
            ranged += 1;
        }
    }
    if ranged == 0 {
        return fail(format!(
            "no hit over the fixture's plain texts carried a range ({absent} named why not)"
        ));
    }
    Check::pass(
        id,
        Level::Feature,
        format!(
            "{ranged} hit span(s) dereference exactly; {absent} hit(s) named why they carry none"
        ),
    )
}

/// `knowledge.scoped` (§2.8): a query reaching several fixture texts,
/// searched again scoped to one of them, returns hits from that text only
/// and at least one; a `documents` entry that is not a text name is refused
/// with 400, never searched as no filter.
pub async fn check_knowledge_scoped(
    host: &Host,
    m: &ProviderManifest,
    lib: &Library,
    state: &FixtureState,
) -> Check {
    let id = "knowledge.scoped";
    let fail = |why: String| Check::fail(id, Level::Feature, why);
    if !m.has_feature(features::KNOWLEDGE_SCOPED) {
        return Check::skip(id, Level::Feature, "knowledge:scoped not advertised");
    }
    let corpus = match fixture_corpus(id, state) {
        Ok(c) => c,
        Err(skip) => return skip,
    };
    let query = lib
        .signatures
        .values()
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let all = match hits(
        host,
        m,
        &json!({"query": query, "corpora": [corpus], "limit": 20}),
    )
    .await
    {
        Ok(h) => h,
        Err(e) => return fail(e),
    };
    let mut texts: Vec<String> = Vec::new();
    for hit in &all {
        if let Some(d) = &hit.document {
            if !texts.contains(&d.text_sha256) {
                texts.push(d.text_sha256.clone());
            }
        }
    }
    // The second-ranked text, so the scope has to change what comes back.
    let Some(target) = texts.get(1).cloned() else {
        return Check::skip(
            id,
            Level::Feature,
            format!(
                "could not judge: the unscoped query reached {} text(s), not two",
                texts.len()
            ),
        );
    };
    let scoped = match hits(
        host,
        m,
        &json!({"query": query, "corpora": [corpus], "limit": 20, "documents": [target]}),
    )
    .await
    {
        Ok(h) => h,
        Err(e) => return fail(e),
    };
    if scoped.is_empty() {
        return fail(format!(
            "scoped to {}…, which the unscoped query reached, the search returned nothing",
            &target[..12]
        ));
    }
    for (i, hit) in scoped.iter().enumerate() {
        let named = hit.document.as_ref().map(|d| d.text_sha256.as_str());
        if named != Some(target.as_str()) {
            return fail(format!(
                "scoped to {}…, hit #{i} is from {}",
                &target[..12],
                named.map_or("no named text".to_string(), |n| format!(
                    "{}…",
                    &n[..12.min(n.len())]
                ))
            ));
        }
    }
    match search_body(
        host,
        m,
        &json!({"query": query, "corpora": [corpus], "documents": ["not a text name"]}),
    )
    .await
    {
        Ok(r) if r.status == 400 => {}
        Ok(r) => {
            return fail(format!(
                "a `documents` entry that is not a text name must be 400, got {}",
                r.brief()
            ))
        }
        Err(e) => return fail(e),
    }
    Check::pass(
        id,
        Level::Feature,
        format!(
            "scoped to one of {} text(s), {} hit(s) all from it; a malformed name is refused",
            texts.len(),
            scoped.len()
        ),
    )
}

/// What a sentence reply (§2.11) says wrongly about itself, judged without
/// reading a text back: it names the advertised scorer, a route, and
/// `sentences_pending` exactly on `passages`; at most `limit` hits, best
/// first; every hit a span (a sentence is always a slice) whose `exact` is
/// its `content`, from the one text the request was scoped to.
pub fn sentence_reply_failures(
    advertised: SentenceScorer,
    scope: &str,
    limit: usize,
    reply: &KnowledgeSearchResponse,
) -> Vec<String> {
    let mut f = Vec::new();
    match &reply.scorer {
        None => f.push("the reply names no scorer".to_string()),
        Some(s) if s.name != advertised => f.push(format!(
            "the manifest advertises `{}`; the reply was scored by `{}` ({}), which §2.11 \
             requires refused by name, never answered by another scorer",
            advertised.as_str(),
            s.name.as_str(),
            s.model
        )),
        Some(s) if s.model.trim().is_empty() => f.push("the reply's scorer names no model".into()),
        Some(_) => {}
    }
    match (reply.route, reply.sentences_pending) {
        (None, _) => f.push("the reply names no route".into()),
        (Some(SentenceRoute::Passages), None) => {
            f.push("route `passages` without `sentences_pending`".into())
        }
        (Some(SentenceRoute::Index), Some(n)) => {
            f.push(format!("route `index` with `sentences_pending: {n}`"))
        }
        _ => {}
    }
    if let Some(t) = reply.truncated {
        if reply.route != Some(SentenceRoute::Index) || t.scanned >= t.in_scope {
            f.push(format!(
                "`truncated` ({} of {}) must be on the index route and below the scope",
                t.scanned, t.in_scope
            ));
        }
    }
    if reply.results.len() > limit {
        f.push(format!(
            "{} hits for a limit of {limit}",
            reply.results.len()
        ));
    }
    if reply.results.windows(2).any(|w| w[1].score > w[0].score) {
        f.push("the hits are not best first".into());
    }
    for (i, hit) in reply.results.iter().enumerate() {
        let Some(span) = &hit.span else {
            f.push(format!("sentence #{i} carries no span"));
            continue;
        };
        if hit.span_absent.is_some() {
            f.push(format!("sentence #{i} carries a span and names an absence"));
        }
        if span.exact != hit.content {
            f.push(format!("sentence #{i}'s span `exact` is not its `content`"));
        }
        let texts = [
            Some(span.document.text_sha256.as_str()),
            hit.document.as_ref().map(|d| d.text_sha256.as_str()),
        ];
        if texts.iter().any(|t| *t != Some(scope)) {
            f.push(format!(
                "sentence #{i} is from outside the `documents` scope ({}…)",
                &scope[..12.min(scope.len())]
            ));
        }
    }
    f
}

/// `knowledge.sentences` (§2.11): each fixture signature searched at
/// sentence granularity, scoped to its own text; every reply holds
/// [`sentence_reply_failures`], and every sentence's range reads back
/// through the text endpoint byte-equal to its `exact`. A host that refuses
/// its advertised scorer by name could not be judged; one that names
/// another scorer, or refuses naming one it did not advertise, fails.
pub async fn check_knowledge_sentences(
    host: &Host,
    m: &ProviderManifest,
    lib: &Library,
    state: &FixtureState,
) -> Check {
    let id = "knowledge.sentences";
    let fail = |why: String| Check::fail(id, Level::Feature, why);
    if !m.has_feature(features::KNOWLEDGE_SENTENCES) {
        return Check::skip(id, Level::Feature, "knowledge:sentences not advertised");
    }
    let k = m.knowledge.as_ref();
    let (Some(advertised), Some(ev)) = (
        k.and_then(|k| k.sentence_scorer),
        k.and_then(|k| k.evidence.as_ref()),
    ) else {
        return fail(
            "knowledge:sentences advertised without knowledge.sentence_scorer and \
             knowledge.evidence to read a sentence back"
                .into(),
        );
    };
    let corpus = match fixture_corpus(id, state) {
        Ok(c) => c,
        Err(skip) => return skip,
    };
    let corpus_q = format!("?corpus={corpus}");
    let limit = 20;
    let mut texts: BTreeMap<String, String> = BTreeMap::new();
    let (mut read_back, mut scorer, mut route) = (0usize, String::new(), String::new());
    // A text's name is the sha256 of the text the host stored, which need not
    // be the fixture's bytes (the reference host's extractor trims a trailing
    // newline). Each document's name is learned from its own record, whose
    // `source.sha256` is the fixture text's (§3), as knowledge.scoped learns
    // its target from the host.
    let signatures = lib
        .signatures
        .values()
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let named: BTreeMap<String, String> = match hits(
        host,
        m,
        &json!({"query": signatures, "corpora": [corpus], "limit": 20}),
    )
    .await
    {
        Ok(all) => all
            .iter()
            .filter_map(|h| h.document.as_ref())
            .filter_map(|d| {
                let doc = lib.documents.iter().find(|f| {
                    d.source.sha256.as_deref() == Some(sha256_hex(f.text.as_bytes()).as_str())
                })?;
                Some((doc.name.clone(), d.text_sha256.clone()))
            })
            .collect(),
        Err(e) => return fail(e),
    };
    if named.is_empty() {
        return Check::skip(
            id,
            Level::Feature,
            "could not judge: no hit named a fixture document's record, so no text could be scoped",
        );
    }
    for doc in lib.documents.iter().filter(|d| named.contains_key(&d.name)) {
        let scope = named[&doc.name].clone();
        let query = lib.signatures.get(&doc.name).map_or("", String::as_str);
        let body = json!({"query": query, "corpora": [corpus], "documents": [scope],
                          "limit": limit, "granularity": "sentence"});
        let r = match search_body(host, m, &body).await {
            Ok(r) => r,
            Err(e) => return fail(e),
        };
        if r.status == 503
            && r.body.get("error").and_then(Value::as_str) == Some(SCORER_UNAVAILABLE)
        {
            let named = r.body.get("scorer").and_then(Value::as_str);
            if named == Some(advertised.as_str()) {
                return Check::skip(
                    id,
                    Level::Feature,
                    format!(
                        "could not judge: the host refuses its advertised scorer `{}` by name \
                         ({SCORER_UNAVAILABLE})",
                        advertised.as_str()
                    ),
                );
            }
            return fail(format!(
                "refused `{SCORER_UNAVAILABLE}` naming {named:?}, not the advertised `{}`",
                advertised.as_str()
            ));
        }
        if r.status != 200 {
            return fail(format!("sentence search: {}", r.brief()));
        }
        let reply: KnowledgeSearchResponse = match serde_json::from_value(r.body.clone()) {
            Ok(x) => x,
            Err(e) => return fail(format!("the sentence reply does not decode: {e}")),
        };
        let bad = sentence_reply_failures(advertised, &scope, limit, &reply);
        if !bad.is_empty() {
            return fail(bad.join("; "));
        }
        if reply.results.is_empty() {
            return fail(format!(
                "`{}`'s signature, scoped to its own text, found no sentence",
                doc.name
            ));
        }
        if let (Some(s), Some(rt)) = (&reply.scorer, reply.route) {
            scorer = format!("{} ({})", s.name.as_str(), s.model);
            route = format!("{rt:?}").to_lowercase();
        }
        for hit in &reply.results {
            let Some(span) = &hit.span else { continue };
            if !texts.contains_key(&scope) {
                match read(host, &ev.text_endpoint, &scope, &corpus_q).await {
                    Ok(w) => texts.insert(scope.clone(), w.text),
                    Err(e) => return fail(e),
                };
            }
            let there = code_points(&texts[&scope], span.start, span.end);
            if there.as_deref().map(str::as_bytes) != Some(span.exact.as_bytes()) {
                return fail(format!(
                    "a sentence's span [{}, {}) says {:?}; the text there says {there:?}",
                    span.start, span.end, span.exact
                ));
            }
            read_back += 1;
        }
    }
    Check::pass(
        id,
        Level::Feature,
        format!(
            "{read_back} sentence(s) over {} scoped searches dereference byte-equal; scored by \
             {scorer}, route {route}",
            lib.documents.len()
        ),
    )
}
