// SPDX-License-Identifier: AGPL-3.0-or-later
//! v0.5's evidence checks (§2-§3): records on search hits, stored texts read
//! back by name, and the alignment of a planted misquote. Each judges the
//! fixture library, so each reports could-not-judge when it is not
//! installed, naming why.

use oicp_types::evidence::{is_sha256_hex, reasons, texts_digest_preimage};
use oicp_types::{
    features, AlignResponse, Document, KnowledgeResult, ProviderManifest, Span, TextSlice,
};
use serde_json::{json, Value};

use crate::checks::Host;
use crate::fixture::{code_points, collapse_ws, sha256_hex, Library};
use crate::ingest_checks::FixtureState;
use crate::report::{Check, Level};

/// §2.1 and §4 feature co-occurrence: `evidence:text` iff
/// `knowledge.evidence`, `evidence:align` iff its `align_endpoint`,
/// `ingest:recipe` only beside `ingest:v1`, `knowledge:document` only with a
/// knowledge plane.
pub fn v05_feature_failures(m: &ProviderManifest) -> Vec<String> {
    let mut f = Vec::new();
    let evidence = m.knowledge.as_ref().and_then(|k| k.evidence.as_ref());
    let text = m.has_feature(features::EVIDENCE_TEXT);
    if text != evidence.is_some() {
        f.push(format!(
            "evidence:text ({text}) must co-occur with knowledge.evidence ({})",
            evidence.is_some()
        ));
    }
    let align = m.has_feature(features::EVIDENCE_ALIGN);
    let align_endpoint = evidence.and_then(|e| e.align_endpoint.as_ref()).is_some();
    if align != align_endpoint {
        f.push(format!(
            "evidence:align ({align}) must co-occur with knowledge.evidence.align_endpoint \
             ({align_endpoint})"
        ));
    }
    if m.has_feature(features::INGEST_RECIPE) && !m.has_feature(features::INGEST_V1) {
        f.push("ingest:recipe requires ingest:v1".into());
    }
    if m.has_feature(features::KNOWLEDGE_DOCUMENT) && m.knowledge.is_none() {
        f.push("knowledge:document requires a knowledge plane".into());
    }
    f
}

/// What is wrong with a stored text's record (§2.2).
pub fn record_failures(d: &Document) -> Vec<String> {
    let mut f = Vec::new();
    if !is_sha256_hex(&d.text_sha256) {
        f.push(format!(
            "text_sha256 `{}` is not 64 lowercase hex",
            d.text_sha256
        ));
    }
    if d.extractor.is_empty() || d.extractor.contains(char::is_whitespace) {
        f.push(format!(
            "extractor `{}` must be non-empty, without whitespace",
            d.extractor
        ));
    }
    if d.source.id.is_empty() {
        f.push("source.id is empty".into());
    }
    if let Some(s) = &d.source.sha256 {
        if !is_sha256_hex(s) {
            f.push(format!("source.sha256 `{s}` is not 64 lowercase hex"));
        }
    }
    f
}

/// `knowledge.search`'s typed half (§3): when the host advertises
/// `knowledge:document`, every hit decodes as a `KnowledgeResult` carrying a
/// well-formed `document`, or naming why not with one of
/// [`reasons::HIT_ABSENCES`]. A live host holds corpora built before the
/// text store, so absence is lawful there; silence about it is not.
pub fn typed_hit_failures(results: &[Value]) -> Vec<String> {
    let mut f = Vec::new();
    for (i, v) in results.iter().enumerate() {
        match serde_json::from_value::<KnowledgeResult>(v.clone()) {
            Err(e) => f.push(format!("hit #{i} does not decode: {e}")),
            Ok(hit) => match &hit.document {
                None => match hit.document_absent.as_deref() {
                    Some(r) if reasons::HIT_ABSENCES.contains(&r) => {}
                    Some(r) => f.push(format!("hit #{i} names an unknown absence `{r}`")),
                    None => f.push(format!("hit #{i} carries no document and names no reason")),
                },
                Some(d) => f.extend(
                    record_failures(d)
                        .into_iter()
                        .map(|e| format!("hit #{i}: {e}")),
                ),
            },
        }
    }
    f
}

fn fixture_corpus<'a>(id: &str, state: &'a FixtureState) -> Result<&'a str, Check> {
    match state {
        FixtureState::Installed(c) => Ok(c),
        FixtureState::Unavailable(why) => Err(Check::skip(
            id,
            Level::Feature,
            format!("could not judge: the fixture library is not installed ({why})"),
        )),
    }
}

async fn search(
    host: &Host,
    m: &ProviderManifest,
    query: &str,
    corpus: &str,
) -> Result<Vec<KnowledgeResult>, String> {
    let k = m.knowledge.as_ref().ok_or("no knowledge plane")?;
    let body = json!({"query": query, "corpora": [corpus], "limit": 10});
    let r = host.post(&k.search_endpoint, &body).await?;
    if r.status != 200 {
        return Err(format!("search: {}", r.brief()));
    }
    let results = r.body.get("results").cloned().unwrap_or(Value::Null);
    serde_json::from_value(results).map_err(|e| format!("search results do not decode: {e}"))
}

/// The fixture document a record is of: its source bytes are that
/// document's text.
fn fixture_doc_of<'a>(lib: &'a Library, d: &Document) -> Option<&'a str> {
    lib.documents
        .iter()
        .find(|f| d.source.sha256.as_deref() == Some(sha256_hex(f.text.as_bytes()).as_str()))
        .map(|f| f.name.as_str())
}

/// `knowledge.document` (§3): each fixture document's declared metadata
/// comes back on its hits verbatim, and every hit carries its record.
pub async fn check_knowledge_document(
    host: &Host,
    m: &ProviderManifest,
    lib: &Library,
    state: &FixtureState,
) -> Check {
    let id = "knowledge.document";
    if !m.has_feature(features::KNOWLEDGE_DOCUMENT) {
        return Check::skip(id, Level::Feature, "knowledge:document not advertised");
    }
    let corpus = match fixture_corpus(id, state) {
        Ok(c) => c,
        Err(skip) => return skip,
    };
    let mut judged = Vec::new();
    for doc in lib.documents.iter().filter(|d| d.metadata.is_some()) {
        let Some(sig) = lib.signatures.get(&doc.name) else {
            continue;
        };
        let hits = match search(host, m, sig, corpus).await {
            Ok(h) => h,
            Err(e) => return Check::fail(id, Level::Feature, e),
        };
        let mut found = None;
        for (i, hit) in hits.iter().enumerate() {
            let Some(d) = &hit.document else {
                return Check::fail(
                    id,
                    Level::Feature,
                    format!("hit #{i} for `{}` carries no document", doc.name),
                );
            };
            let bad = record_failures(d);
            if !bad.is_empty() {
                return Check::fail(id, Level::Feature, bad.join("; "));
            }
            if fixture_doc_of(lib, d) == Some(doc.name.as_str()) {
                found = Some(d.clone());
            }
        }
        let Some(d) = found else {
            return Check::fail(
                id,
                Level::Feature,
                format!(
                    "searching `{sig}` returned no hit whose source is `{}`",
                    doc.name
                ),
            );
        };
        if d.metadata != doc.metadata {
            return Check::fail(
                id,
                Level::Feature,
                format!(
                    "`{}` declared {:?} and came back as {:?}",
                    doc.name, doc.metadata, d.metadata
                ),
            );
        }
        judged.push(doc.name.as_str());
    }
    Check::pass(
        id,
        Level::Feature,
        format!("declared metadata verbatim for {judged:?}"),
    )
}

/// POST an align request; the raw body beside the decoded reply.
async fn align(
    host: &Host,
    endpoint: &str,
    quote: &str,
    corpus: &str,
) -> Result<(Value, AlignResponse), String> {
    let raw = align_raw(host, endpoint, quote, corpus).await?;
    let typed = decode(&raw)?;
    Ok((raw, typed))
}

/// An align reply's body, before it is decoded, so a check can name the
/// field that is missing rather than report a decode error.
async fn align_raw(
    host: &Host,
    endpoint: &str,
    quote: &str,
    corpus: &str,
) -> Result<Value, String> {
    let r = host
        .post(endpoint, &json!({"quote": quote, "corpora": [corpus]}))
        .await?;
    if r.status != 200 {
        return Err(format!("align: {}", r.brief()));
    }
    Ok(r.body)
}

fn decode(raw: &Value) -> Result<AlignResponse, String> {
    serde_json::from_value(raw.clone()).map_err(|e| format!("align reply does not decode: {e}"))
}

/// The best alignment in `corpus`, or why there is none.
fn top_span(
    resp: &AlignResponse,
    corpus: &str,
    quote: &str,
) -> Result<(Span, Vec<oicp_types::Difference>, f32), String> {
    resp.alignments
        .iter()
        .find(|a| a.span.corpus_id == corpus)
        .map(|a| (a.span.clone(), a.differences.clone(), a.coverage))
        .ok_or_else(|| format!("`{quote}` aligned nowhere in {corpus}"))
}

/// `evidence.align` (§2.3): the fixture's sentence aligns verbatim, and with
/// one word changed aligns with exactly one `substituted` at the changed
/// word's ranges; every reply names its aligner and a `texts_digest` for the
/// corpus, stable across calls and equal to the digest of the records the
/// fixture's documents aligned to.
pub async fn check_evidence_align(
    host: &Host,
    m: &ProviderManifest,
    lib: &Library,
    state: &FixtureState,
) -> Check {
    let id = "evidence.align";
    let fail = |why: String| Check::fail(id, Level::Feature, why);
    if !m.has_feature(features::EVIDENCE_ALIGN) {
        return Check::skip(id, Level::Feature, "evidence:align not advertised");
    }
    let Some(endpoint) = m
        .knowledge
        .as_ref()
        .and_then(|k| k.evidence.as_ref())
        .and_then(|e| e.align_endpoint.clone())
    else {
        return fail("evidence:align advertised but no align_endpoint".into());
    };
    let corpus = match fixture_corpus(id, state) {
        Ok(c) => c,
        Err(skip) => return skip,
    };
    let plant = &lib.plant;
    let (planted, word) = lib.planted_quote();
    let mut digests = Vec::new();
    let mut calls = Vec::new();
    for quote in [plant.sentence.as_str(), planted.as_str()] {
        let raw = match align_raw(host, &endpoint, quote, corpus).await {
            Ok(x) => x,
            Err(e) => return fail(e),
        };
        if raw
            .get("aligner")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return fail(format!("the reply names no aligner: {raw}"));
        }
        let entry = raw.get("corpora").and_then(Value::as_array).and_then(|cs| {
            cs.iter()
                .find(|c| c.get("corpus_id").and_then(Value::as_str) == Some(corpus))
        });
        let Some(digest) = entry
            .and_then(|c| c.get("texts_digest"))
            .and_then(Value::as_str)
        else {
            return fail(format!(
                "the reply carries no texts_digest for {corpus}: {raw}"
            ));
        };
        if !is_sha256_hex(digest) {
            return fail(format!("texts_digest `{digest}` is not 64 lowercase hex"));
        }
        digests.push(digest.to_string());
        match decode(&raw) {
            Ok(resp) => calls.push(resp),
            Err(e) => return fail(e),
        }
    }
    if digests[0] != digests[1] {
        return fail(format!(
            "texts_digest changed between two calls with no ingest: {digests:?}"
        ));
    }

    let (span, diffs, coverage) = match top_span(&calls[0], corpus, &plant.sentence) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    if !diffs.is_empty()
        || coverage < 1.0
        || collapse_ws(&span.exact) != collapse_ws(&plant.sentence)
    {
        return fail(format!(
            "the verbatim sentence aligned with {diffs:?} at coverage {coverage} to {:?}",
            span.exact
        ));
    }
    let (span, diffs, _) = match top_span(&calls[1], corpus, &planted) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    let [d] = diffs.as_slice() else {
        return fail(format!(
            "one changed word must be one difference, got {diffs:?}"
        ));
    };
    if d.kind != oicp_types::DifferenceKind::Substituted || d.quote != [word.start, word.end] {
        return fail(format!(
            "want substituted at quote [{}, {}), got {:?} at {:?}",
            word.start, word.end, d.kind, d.quote
        ));
    }
    let [s0, s1] = d.source;
    let in_span = span.start <= s0 && s1 <= span.end;
    let source_word = in_span
        .then(|| code_points(&span.exact, s0 - span.start, s1 - span.start))
        .flatten();
    if source_word.as_deref() != Some(plant.word.as_str()) {
        return fail(format!(
            "the source range [{s0}, {s1}) names {source_word:?} in span [{}, {}), not `{}`",
            span.start, span.end, plant.word
        ));
    }

    // The digest, re-derived from the fixture documents' own records.
    let mut records = Vec::new();
    for doc in &lib.documents {
        let Some(sig) = lib.signatures.get(&doc.name) else {
            continue;
        };
        match align(host, &endpoint, sig, corpus)
            .await
            .and_then(|(_, r)| top_span(&r, corpus, sig))
        {
            Ok((span, _, _)) => records.push(span.document),
            Err(e) => return fail(e),
        }
    }
    let preimage = texts_digest_preimage(records.iter().map(|d| {
        (
            d.text_sha256.as_str(),
            d.source.sha256.as_deref(),
            d.extractor.as_str(),
        )
    }));
    let derived = sha256_hex(preimage.as_bytes());
    if derived != digests[0] {
        return fail(format!(
            "texts_digest {} is not the digest of the fixture's {} records ({derived})",
            digests[0],
            records.len()
        ));
    }
    Check::pass(
        id,
        Level::Feature,
        format!(
            "verbatim at coverage 1; `{}` → one substituted at quote [{}, {}); texts_digest stable \
             and re-derived",
            plant.planted, word.start, word.end
        ),
    )
}

/// GET a slice of a text; `Err` on any reply but 200 that decodes.
async fn read(host: &Host, endpoint: &str, sha: &str, query: &str) -> Result<TextSlice, String> {
    let r = host.get(&format!("{endpoint}/{sha}{query}")).await?;
    if r.status != 200 {
        return Err(format!(
            "GET text/{}…{query}: {}",
            &sha[..12.min(sha.len())],
            r.brief()
        ));
    }
    serde_json::from_value(r.body.clone()).map_err(|e| format!("text slice does not decode: {e}"))
}

/// `evidence.text` (§2.2): every span from search and align dereferences to
/// text equal to its `exact`; the whole text hashes to its name; context
/// and the two named refusals behave as specified.
pub async fn check_evidence_text(
    host: &Host,
    m: &ProviderManifest,
    lib: &Library,
    state: &FixtureState,
) -> Check {
    let id = "evidence.text";
    let fail = |why: String| Check::fail(id, Level::Feature, why);
    if !m.has_feature(features::EVIDENCE_TEXT) {
        return Check::skip(id, Level::Feature, "evidence:text not advertised");
    }
    let Some(ev) = m.knowledge.as_ref().and_then(|k| k.evidence.as_ref()) else {
        return fail("evidence:text advertised but no knowledge.evidence".into());
    };
    let corpus = match fixture_corpus(id, state) {
        Ok(c) => c,
        Err(skip) => return skip,
    };
    // Spans with ranges come from align; names alone from search hits.
    let mut spans: Vec<Span> = Vec::new();
    let mut names: Vec<Document> = Vec::new();
    for (name, sig) in &lib.signatures {
        if let Some(endpoint) = &ev.align_endpoint {
            match align(host, endpoint, sig, corpus)
                .await
                .and_then(|(_, r)| top_span(&r, corpus, sig))
            {
                // An inline document's source bytes are its text (§6.1).
                Ok((span, _, _)) if span.document.source.sha256 != lib.source_sha256(name) => {
                    return fail(format!(
                        "`{name}`'s source.sha256 is {:?}, not the sha256 of its bytes",
                        span.document.source.sha256
                    ));
                }
                Ok((span, _, _)) => spans.push(span),
                Err(e) => return fail(e),
            }
        }
        if m.has_feature(features::KNOWLEDGE_DOCUMENT) {
            match search(host, m, sig, corpus).await {
                Ok(hits) => names.extend(hits.into_iter().filter_map(|h| h.document)),
                Err(e) => return fail(e),
            }
        }
    }
    if spans.is_empty() && names.is_empty() {
        return Check::skip(
            id,
            Level::Feature,
            "could not judge: no text is named to this client (neither evidence:align nor \
             knowledge:document advertised)",
        );
    }
    names.extend(spans.iter().map(|s| s.document.clone()));
    let corpus_q = format!("?corpus={corpus}");
    for d in &names {
        let whole = match read(host, &ev.text_endpoint, &d.text_sha256, &corpus_q).await {
            Ok(s) => s,
            Err(e) => return fail(e),
        };
        if sha256_hex(whole.text.as_bytes()) != d.text_sha256
            || whole.document.text_sha256 != d.text_sha256
        {
            return fail(format!(
                "the text served as {} does not hash to its name",
                d.text_sha256
            ));
        }
        let len = whole.text.chars().count() as u64;
        if whole.start != 0 || whole.end != len {
            return fail(format!(
                "a whole-text read reports [{}, {}) for a text of {len}",
                whole.start, whole.end
            ));
        }
    }
    for span in &spans {
        let sha = &span.document.text_sha256;
        let whole = match read(host, &ev.text_endpoint, sha, &corpus_q).await {
            Ok(s) => s,
            Err(e) => return fail(e),
        };
        if code_points(&whole.text, span.start, span.end).as_deref() != Some(span.exact.as_str()) {
            return fail(format!(
                "span [{}, {}) says {:?}; the text there says {:?}",
                span.start,
                span.end,
                span.exact,
                code_points(&whole.text, span.start, span.end)
            ));
        }
        let q = format!(
            "?start={}&end={}&context=8&corpus={corpus}",
            span.start, span.end
        );
        let slice = match read(host, &ev.text_endpoint, sha, &q).await {
            Ok(s) => s,
            Err(e) => return fail(e),
        };
        let before = code_points(&whole.text, span.start.saturating_sub(8), span.start);
        let after = code_points(&whole.text, span.end, (span.end + 8).min(whole.end));
        if slice.text != span.exact
            || Some(slice.before.clone()) != before
            || Some(slice.after.clone()) != after
        {
            return fail(format!(
                "range read [{}, {}) context 8 gave {:?} / {:?} / {:?}",
                span.start, span.end, slice.before, slice.text, slice.after
            ));
        }
    }
    let Some(any) = names.first() else {
        return fail("no record to probe the refusals with".into());
    };
    let unknown = "0".repeat(64);
    match host.get(&format!("{}/{unknown}", ev.text_endpoint)).await {
        Ok(r) if r.status == 404 && r.error() == Some(reasons::TEXT_NOT_HELD) => {}
        Ok(r) => {
            return fail(format!(
                "an unknown text must be 404 `{}`, got {}",
                reasons::TEXT_NOT_HELD,
                r.brief()
            ))
        }
        Err(e) => return fail(e),
    }
    let past = format!(
        "{}/{}?start=1000000000&end=1000000001",
        ev.text_endpoint, any.text_sha256
    );
    match host.get(&past).await {
        Ok(r) if r.status == 400 && r.error() == Some(reasons::RANGE_OUTSIDE_TEXT) => {}
        Ok(r) => {
            return fail(format!(
                "a range past the end must be 400 `{}`, got {}",
                reasons::RANGE_OUTSIDE_TEXT,
                r.brief()
            ))
        }
        Err(e) => return fail(e),
    }
    Check::pass(
        id,
        Level::Feature,
        format!(
            "{} span(s) and {} name(s) dereference exactly; refusals named",
            spans.len(),
            names.len()
        ),
    )
}
