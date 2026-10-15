// SPDX-License-Identifier: AGPL-3.0-or-later
//! v0.6's checks that install a shelf of their own (oicp-v0.6.md §6):
//! `knowledge.locator` (§2.13), every span of a headed text names the
//! headings it stands under and a span of a text with none names nothing.
//! Each aligns a shelf's quotations verbatim and reads the locator of the
//! span that comes back: align is the evidence route that returns a span of
//! exactly the text asked about.

use oicp_types::{features, AlignResponse, ProviderManifest, SpanLocator};
use serde_json::{json, Value};

use crate::checks::Host;
use crate::fixture::{collapse_ws, sha256_hex};
use crate::ingest_checks::check_ingest_install_limit;
use crate::report::{Check, Level};
use crate::shelf::LocatedShelf;

/// The v0.6 checks that read no fixture library, in order. The install
/// limit sends no library at all; the rest install their own shelf.
pub async fn own_library_checks(host: &Host, m: &ProviderManifest) -> Vec<Check> {
    vec![
        check_ingest_install_limit(host, m).await,
        check_knowledge_locator(host, m).await,
    ]
}

/// `knowledge.locator` (§2.13): the headings shelf installed, each of its
/// quotations aligned, and the span's locator is the heading path the rule
/// gives it, or none: before the first heading, under a contents line, and
/// in a text with no headings.
pub async fn check_knowledge_locator(host: &Host, m: &ProviderManifest) -> Check {
    let id = "knowledge.locator";
    if !m.has_feature(features::KNOWLEDGE_LOCATOR) {
        return Check::skip(id, Level::Feature, "knowledge:locator not advertised");
    }
    match LocatedShelf::load(include_str!("../fixture/headings.json")) {
        Ok(fx) => judge_located(id, host, m, &fx, &[]).await,
        Err(e) => Check::fail(id, Level::Feature, format!("fixture/headings.json {e}")),
    }
}

/// Install `fx` (with `extra` in the install request), align each of its
/// quotations in its corpus, and hold each span to the quotation, its
/// document, and its locator.
pub async fn judge_located(
    id: &str,
    host: &Host,
    m: &ProviderManifest,
    fx: &LocatedShelf,
    extra: &[(&str, Value)],
) -> Check {
    let fail = |why: String| Check::fail(id, Level::Feature, why);
    let align = m
        .knowledge
        .as_ref()
        .and_then(|k| k.evidence.as_ref())
        .and_then(|e| e.align_endpoint.clone());
    let (Some(align), true) = (align, m.has_feature(features::INGEST_RECIPE)) else {
        return Check::skip(
            id,
            Level::Feature,
            "could not judge: the shelf is installed by ingest:recipe and read by evidence:align",
        );
    };
    if let Err(e) = fx.shelf.install(host, m, extra).await {
        return fail(e);
    }
    let corpus = &fx.shelf.corpus_id;
    let (mut named, mut none) = (0usize, 0usize);
    for l in &fx.located {
        let body = json!({"quote": l.quote, "corpora": [corpus]});
        let r = match host.post(&align, &body).await {
            Ok(r) if r.status == 200 => r,
            Ok(r) => return fail(format!("align: {}", r.brief())),
            Err(e) => return fail(e),
        };
        let reply: AlignResponse = match serde_json::from_value(r.body) {
            Ok(x) => x,
            Err(e) => return fail(format!("the align reply does not decode: {e}")),
        };
        let Some(span) = reply
            .alignments
            .iter()
            .map(|a| &a.span)
            .find(|s| s.corpus_id == *corpus)
        else {
            return fail(format!("`{}` aligned nowhere in {corpus}", l.quote));
        };
        if collapse_ws(&span.exact) != collapse_ws(&l.quote) {
            return fail(format!(
                "`{}` aligned to {:?}, not to itself",
                l.quote, span.exact
            ));
        }
        let source = fx
            .shelf
            .doc(&l.document)
            .map(|d| sha256_hex(d.text.as_bytes()));
        if span.document.source.sha256 != source {
            return fail(format!(
                "`{}` aligned into a document whose source is not `{}`",
                l.quote, l.document
            ));
        }
        if span.locator != l.locator {
            return fail(format!(
                "`{}` ({}) carries {}; it stands {}",
                l.quote,
                l.document,
                describe(span.locator.as_ref()),
                describe(l.locator.as_ref())
            ));
        }
        if l.locator.is_some() {
            named += 1;
        } else {
            none += 1;
        }
    }
    Check::pass(
        id,
        Level::Feature,
        format!(
            "{named} span(s) name where they stand; {none} that stand nowhere named name nothing"
        ),
    )
}

fn describe(l: Option<&SpanLocator>) -> String {
    match l {
        None => "no locator".into(),
        Some(SpanLocator::HeadingPath { headings }) => format!("under {headings:?}"),
        Some(SpanLocator::Timestamp {
            milliseconds,
            speaker,
        }) => format!(
            "at {milliseconds} ms, speaker {}",
            speaker
                .as_deref()
                .map_or("absent".into(), |s| format!("`{s}`"))
        ),
        Some(other) => format!("{other:?}"),
    }
}
