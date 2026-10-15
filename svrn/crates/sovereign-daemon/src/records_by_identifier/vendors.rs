// SPDX-License-Identifier: AGPL-3.0-or-later
//! One reader per resolver reply shape ([`ReplyShape`]): what a status and
//! body say of an identifier. Only a reply that names the identifier's
//! record is `Found`; a resolver that does not know it is `Unresolved`; any
//! other status, a transport error, or a body that does not read is
//! `Unreachable`, named, never read as an empty record.

use serde_json::{json, Map, Value};

use super::{FetchReply, ReplyShape, ResolverAnswer, ResolverRow};

/// What `reply`, the answer to `row`'s request for `value`, says.
pub(super) fn read(
    row: &ResolverRow,
    value: &str,
    reply: Result<FetchReply, String>,
) -> ResolverAnswer {
    let reply = match reply {
        Ok(r) => r,
        Err(e) => return ResolverAnswer::Unreachable(e),
    };
    match row.reply {
        ReplyShape::Csl => csl(&reply),
        ReplyShape::OpenLibraryBooks => open_library(&reply, &row.normalize.apply(value)),
    }
}

/// Crossref's `transform` to CSL-JSON: the record at `200`, `404` (or `400`,
/// `410`) for a DOI it does not hold.
fn csl(reply: &FetchReply) -> ResolverAnswer {
    match reply.status {
        200 => match serde_json::from_str::<Value>(&reply.body) {
            Ok(Value::Object(m)) => ResolverAnswer::Found(m),
            Ok(_) | Err(_) => {
                ResolverAnswer::Unreachable("answered 200 with no CSL-JSON object".into())
            }
        },
        400 | 404 | 410 => ResolverAnswer::Unresolved,
        s => ResolverAnswer::Unreachable(format!("answered {s}")),
    }
}

/// Open Library's Books API: `{"ISBN:<isbn>": {...}}`, or `{}` for an ISBN
/// it does not hold. Its record is read into CSL-JSON names: a name it gives
/// whole stays whole (`literal`), never split into family and given.
fn open_library(reply: &FetchReply, isbn: &str) -> ResolverAnswer {
    if reply.status != 200 {
        return ResolverAnswer::Unreachable(format!("answered {}", reply.status));
    }
    let books = match serde_json::from_str::<Value>(&reply.body) {
        Ok(Value::Object(m)) => m,
        Ok(_) | Err(_) => {
            return ResolverAnswer::Unreachable("answered 200 with no JSON object".into())
        }
    };
    match books.get(&format!("ISBN:{isbn}")) {
        Some(Value::Object(book)) => ResolverAnswer::Found(open_library_csl(book)),
        _ => ResolverAnswer::Unresolved,
    }
}

fn open_library_csl(book: &Map<String, Value>) -> Map<String, Value> {
    let names = |key: &str| -> Vec<&str> {
        book.get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.get("name").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut csl = Map::new();
    csl.insert("type".into(), json!("book"));
    if let Some(t) = book.get("title").and_then(Value::as_str) {
        csl.insert("title".into(), json!(t));
    }
    let authors = names("authors");
    if !authors.is_empty() {
        let literal: Vec<Value> = authors.iter().map(|n| json!({"literal": n})).collect();
        csl.insert("author".into(), Value::Array(literal));
    }
    if let Some(p) = names("publishers").first() {
        csl.insert("publisher".into(), json!(p));
    }
    if let Some(p) = names("publish_places").first() {
        csl.insert("publisher-place".into(), json!(p));
    }
    if let Some(d) = book.get("publish_date").and_then(Value::as_str) {
        let issued = match year_of(d) {
            Some(y) => json!({"date-parts": [[y]]}),
            None => json!({"raw": d}),
        };
        csl.insert("issued".into(), issued);
    }
    if let Some(n) = book.get("number_of_pages").filter(|n| n.is_number()) {
        csl.insert("number-of-pages".into(), n.clone());
    }
    if let Some(u) = book.get("url").and_then(Value::as_str) {
        csl.insert("URL".into(), json!(u));
    }
    csl
}

/// The last four-digit run of `date` read as a year (`July 25th 1996`,
/// `Sep 01, 2008`), or `None` when it holds none.
fn year_of(date: &str) -> Option<u32> {
    date.split(|c: char| !c.is_ascii_digit())
        .filter(|run| run.len() == 4)
        .filter_map(|run| run.parse::<u32>().ok())
        .filter(|y| (1000..=2999).contains(y))
        .last()
}
