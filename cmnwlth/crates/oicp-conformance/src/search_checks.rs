// SPDX-License-Identifier: AGPL-3.0-or-later
//! v0.6's search checks (oicp-v0.6.md §2.8, §6) over the fixture library:
//! `knowledge.range`, every range a hit gives dereferences to its `exact`
//! through the text endpoint, and `knowledge.scoped`, a `documents` filter
//! returns hits from the named texts only and refuses what is not a text name.

use oicp_types::evidence::reasons;
use oicp_types::{features, KnowledgeResult, ProviderManifest};
use serde_json::{json, Value};

use crate::checks::Host;
use crate::evidence_checks::{fixture_corpus, read};
use crate::fixture::{code_points, Library};
use crate::ingest_checks::FixtureState;
use crate::report::{Check, Level};

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

async fn search_body(
    host: &Host,
    m: &ProviderManifest,
    body: &Value,
) -> Result<crate::wire::Reply, String> {
    let k = m.knowledge.as_ref().ok_or("no knowledge plane")?;
    host.post(&k.search_endpoint, body).await
}

async fn hits(
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
