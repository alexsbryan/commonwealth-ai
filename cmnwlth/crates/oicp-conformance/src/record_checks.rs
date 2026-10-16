// SPDX-License-Identifier: AGPL-3.0-or-later
//! v0.6's records-by-identifier check (oicp-v0.6.md §2.16, §6):
//! `ingest.lookups`. It installs its own shelf twice, once allowing lookups
//! and once not, and reads each document's merged record off progress: a
//! declared field kept, an identifier that did not resolve given nothing, no
//! lookup for the install that did not allow one, and the record on a hit
//! the same as on progress.

use std::collections::BTreeMap;

use oicp_types::record::DECLARED;
use oicp_types::{
    features, IngestDocument, IngestDocumentState, KnowledgeResult, LookupOutcome, MergedRecord,
    ProviderManifest,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::checks::Host;
use crate::fixture::sha256_hex;
use crate::report::{Check, Level};
use crate::shelf::{Shelf, ShelfDoc};

/// The records shelf, the corpus it is installed as again without
/// lookups, and what each identifier should come to.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordsShelf {
    /// The library, installed allowing lookups.
    pub shelf: Shelf,
    /// The same library's corpus when installed without allowing them.
    pub closed_corpus_id: String,
    /// The identifiers and whether each should resolve.
    pub lookups: Vec<ExpectedLookup>,
    /// The test host's resolver table; read by no check.
    #[serde(default)]
    pub fake_resolver: Value,
}

/// A document's identifier and whether a resolver should know it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedLookup {
    /// The document.
    pub document: String,
    /// The CSL variable carrying the identifier.
    pub identifier: String,
    /// Whether its resolver should return a record.
    pub resolves: bool,
}

/// `ingest.lookups` (§2.16).
pub async fn check_ingest_lookups(host: &Host, m: &ProviderManifest) -> Check {
    let id = "ingest.lookups";
    let fail = |why: String| Check::fail(id, Level::Feature, why);
    if !m.has_feature(features::INGEST_LOOKUPS) {
        return Check::skip(id, Level::Feature, "ingest:lookups not advertised");
    }
    let ing = m.knowledge.as_ref().and_then(|k| k.ingest.as_ref());
    let (Some(ing), true) = (ing, m.has_feature(features::INGEST_RECIPE)) else {
        return Check::skip(
            id,
            Level::Feature,
            "could not judge: the records shelf is installed by ingest:recipe",
        );
    };
    let fx: RecordsShelf = match serde_json::from_str(include_str!("../fixture/records.json")) {
        Ok(x) => x,
        Err(e) => return fail(format!("fixture/records.json does not load: {e}")),
    };
    let closed = Shelf {
        corpus_id: fx.closed_corpus_id.clone(),
        ..fx.shelf.clone()
    };
    for (shelf, extra) in [
        (&fx.shelf, vec![("allow_lookups", json!(true))]),
        (&closed, vec![]),
    ] {
        if let Err(e) = shelf.install(host, m, &extra).await {
            return fail(e);
        }
    }
    let progress = match host.get(&ing.progress_endpoint).await {
        Ok(r) => r.body,
        Err(e) => return fail(e),
    };
    let mut unjudged = None;
    for (corpus, allowed) in [(&fx.shelf.corpus_id, true), (&fx.closed_corpus_id, false)] {
        let records = match listed_records(&progress, corpus, &fx.shelf) {
            Ok(r) => r,
            Err(e) => return fail(e),
        };
        for d in &fx.shelf.documents {
            let expect = fx.lookups.iter().find(|l| l.document == d.name);
            match record_failures(d, records[&d.name].as_ref(), expect, allowed) {
                Ok(None) => {}
                Ok(Some(why)) => unjudged = Some(why),
                Err(e) => return fail(format!("{corpus}: {e}")),
            }
        }
        if allowed {
            if let Err(e) = hit_record(host, m, corpus, &fx.shelf, &records).await {
                return fail(e);
            }
        }
    }
    if let Some(why) = unjudged {
        return Check::skip(id, Level::Feature, format!("could not judge: {why}"));
    }
    Check::pass(
        id,
        Level::Feature,
        format!(
            "{} identifier(s) looked up only where allowed, declared fields kept, nothing \
             taken from one that did not resolve; records listed on progress and on hits",
            fx.lookups.len()
        ),
    )
}

/// Each shelf document's record as `corpus`'s progress entry lists it; a
/// document listed other than `ready` is a failure.
fn listed_records(
    progress: &Value,
    corpus: &str,
    shelf: &Shelf,
) -> Result<BTreeMap<String, Option<MergedRecord>>, String> {
    let documents = progress
        .get("progress")
        .and_then(|p| p.get(corpus))
        .and_then(|e| e.get("documents"))
        .ok_or(format!("progress lists no documents for {corpus}"))?;
    let documents: Vec<IngestDocument> = serde_json::from_value(documents.clone())
        .map_err(|e| format!("{corpus}'s documents do not decode: {e}"))?;
    let mut out = BTreeMap::new();
    for d in &shelf.documents {
        let listed = documents
            .iter()
            .find(|x| x.source.as_deref() == Some(d.name.as_str()));
        match listed.map(|x| &x.state) {
            Some(IngestDocumentState::Ready { record, .. }) => {
                out.insert(d.name.clone(), record.clone());
            }
            other => return Err(format!("`{}` is listed {other:?} in {corpus}", d.name)),
        }
    }
    Ok(out)
}

/// What `rec`, `d`'s record, says wrongly for an install that did
/// (`allowed`) or did not allow lookups. `Ok(Some(why))` when a resolver
/// was unreachable and the outcome could not be judged.
pub fn record_failures(
    d: &ShelfDoc,
    rec: Option<&MergedRecord>,
    expect: Option<&ExpectedLookup>,
    allowed: bool,
) -> Result<Option<String>, String> {
    let name = &d.name;
    let Some(expect) = expect else {
        return match rec.and_then(|r| r.looked_up().next()) {
            Some((field, origin)) => Err(format!(
                "`{name}` carries no identifier and its `{field}` is from `{origin}`"
            )),
            None => Ok(None),
        };
    };
    let rec = rec.ok_or(format!(
        "`{name}` carries a {} and progress lists no record",
        expect.identifier
    ))?;
    for k in rec.csl.keys() {
        if !rec.origins.contains_key(k) {
            return Err(format!("`{name}`'s `{k}` names no origin"));
        }
    }
    if let Some(k) = rec.origins.keys().find(|k| !rec.csl.contains_key(*k)) {
        return Err(format!(
            "`{name}` names an origin for `{k}`, a field it lacks"
        ));
    }
    let declared = d.metadata.as_ref().and_then(Value::as_object);
    for (k, v) in declared.into_iter().flatten() {
        if rec.csl.get(k) != Some(v) || rec.origins.get(k).map(String::as_str) != Some(DECLARED) {
            return Err(format!(
                "declared `{k}` of `{name}` is not kept: the record holds {:?} from {:?}",
                rec.csl.get(k),
                rec.origins.get(k)
            ));
        }
    }
    let value = declared
        .and_then(|m| m.get(&expect.identifier))
        .and_then(Value::as_str);
    let lookup = rec
        .lookups
        .iter()
        .find(|l| l.identifier == expect.identifier && Some(l.value.as_str()) == value)
        .ok_or(format!(
            "`{name}`'s {} is not listed among its lookups",
            expect.identifier
        ))?;
    let taken = rec.looked_up().next();
    let what = format!("`{name}`'s {}", expect.identifier);
    match (allowed, expect.resolves, lookup.outcome, taken) {
        (false, _, LookupOutcome::NotAllowed, None) => Ok(None),
        (false, _, outcome, _) => Err(format!(
            "{what} was looked up ({outcome:?}) for an install that did not allow it"
        )),
        (true, _, LookupOutcome::Unreachable, None) => Ok(Some(format!(
            "{what}'s resolver `{}` was unreachable",
            lookup.resolver
        ))),
        (true, true, LookupOutcome::Resolved, _) => Ok(None),
        (true, false, LookupOutcome::Unresolved, None) => Ok(None),
        (true, _, _, Some((field, origin))) if lookup.outcome != LookupOutcome::Resolved => Err(
            format!("a field (`{field}`) from `{origin}` for {what}, which did not resolve"),
        ),
        (true, resolves, outcome, taken) => Err(format!(
            "{what} came to {outcome:?}{}; it should {}",
            taken.map_or(String::new(), |(f, o)| format!(" with `{f}` from `{o}`")),
            if resolves {
                "resolve"
            } else {
                "not resolve, and add nothing"
            }
        )),
    }
}

/// With `knowledge:document`, a hit on a document with a record carries
/// that record, the same as progress lists.
async fn hit_record(
    host: &Host,
    m: &ProviderManifest,
    corpus: &str,
    shelf: &Shelf,
    records: &BTreeMap<String, Option<MergedRecord>>,
) -> Result<(), String> {
    let k = m.knowledge.as_ref();
    let (Some(k), true) = (k, m.has_feature(features::KNOWLEDGE_DOCUMENT)) else {
        return Ok(());
    };
    for d in &shelf.documents {
        let Some(want) = records[&d.name].as_ref() else {
            continue;
        };
        let query = d.text.lines().next().unwrap_or(&d.text);
        let r = host
            .post(
                &k.search_endpoint,
                &json!({"query": query, "corpora": [corpus], "limit": 10}),
            )
            .await?;
        let hits: Vec<KnowledgeResult> =
            serde_json::from_value(r.body.get("results").cloned().unwrap_or(Value::Null))
                .map_err(|e| format!("search results do not decode: {e}"))?;
        let source = sha256_hex(d.text.as_bytes());
        let hit = hits
            .iter()
            .filter_map(|h| h.document.as_ref())
            .find(|doc| doc.source.sha256.as_deref() == Some(source.as_str()))
            .ok_or(format!("`{}`'s own words found no hit on it", d.name))?;
        if hit.record.as_ref() != Some(want) {
            return Err(format!(
                "a hit on `{}` carries {:?}; progress lists {want:?}",
                d.name, hit.record
            ));
        }
    }
    Ok(())
}
