// SPDX-License-Identifier: AGPL-3.0-or-later
//! The vectors `propose::Nearest` offers records by: each placed statement's
//! words with the text around them, and each seeded record's card
//! (`mentions::Seeded::cards`), embedded once before the type's documents are
//! resolved by the run's own embedder. A text the embedder refuses is left
//! without a vector, logged, and offered nothing; nothing is guessed for it.

use std::collections::HashMap;

use tracing::{debug, info, warn};

use super::super::resolve_records::propose::Nearest;
use super::Placed;
use crate::EmbedFn;

/// How many records `Nearest` offers each statement. A cost knob: it bounds
/// the candidates a question shows and decides nothing.
const NEAREST_K: usize = 8;

/// The bytes of a statement's surroundings embedded with its words, on each
/// side: enough to carry the sentence a short mention sits in.
const AROUND: usize = 120;

/// The text a statement is embedded as: its words, then the text around them.
fn statement_text(body: &str, start: usize, end: usize) -> String {
    let mut from = start.saturating_sub(AROUND);
    while !body.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = (end + AROUND).min(body.len());
    while !body.is_char_boundary(to) {
        to += 1;
    }
    format!("{}\n{}", &body[start..end], &body[from..to])
}

/// The proposer for one type: its placed statements and seeded cards
/// embedded, every record a statement opens later carded by that statement.
pub(super) async fn nearest(
    embed: &EmbedFn,
    type_name: &str,
    placed: &[Placed<'_>],
    cards: &[(String, String)],
) -> Nearest {
    let (mut statements, mut records, mut refused) = (HashMap::new(), Vec::new(), 0usize);
    for p in placed {
        for s in &p.statements {
            match embed(&statement_text(&p.body, s.start, s.end)).await {
                Ok(v) => {
                    statements.insert(s.id.clone(), v);
                }
                Err(e) => {
                    refused += 1;
                    debug!(r#type = type_name, statement = %s.id, error = %e, "atlas/resolve: a statement the embedder refused; nearest offers it nothing");
                }
            }
        }
    }
    for (id, card) in cards {
        match embed(card).await {
            Ok(v) => records.push((id.clone(), v)),
            Err(e) => {
                refused += 1;
                debug!(r#type = type_name, record = %id, error = %e, "atlas/resolve: a card the embedder refused; nearest never offers it");
            }
        }
    }
    if refused > 0 {
        warn!(
            r#type = type_name,
            refused, "atlas/resolve: texts the embedder refused are offered nothing by nearest"
        );
    }
    info!(
        r#type = type_name,
        statements = statements.len(),
        seeded = records.len(),
        k = NEAREST_K,
        "atlas/resolve: nearest proposer built"
    );
    Nearest::new(NEAREST_K, statements, records)
}
