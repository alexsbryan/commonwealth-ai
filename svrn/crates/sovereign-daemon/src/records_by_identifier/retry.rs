// SPDX-License-Identifier: AGPL-3.0-or-later
//! A records-only second pass over a kept install. `unreachable` is a fact
//! about the network at one moment, not about the identifier (live,
//! 2026-10-15: Crossref answered 429 once and every later install of the
//! same recipe reused the record that said so), so an install whose records
//! name any lookup `unreachable` is not complete in the lookup sense: the
//! next install of the same recipe and allowance asks those identifiers
//! again, and only those. `resolved` and `unresolved` are answers and stand.
//! The corpus's texts are not read again; the stored records are the input.

use super::*;

/// Ask again each lookup of `kept` that is `unreachable` and merge what
/// comes back under [`merge`]'s rules: a field enters only where none is
/// held (declared, or an earlier resolver's), its origin the resolver.
/// Returns how many lookups were asked.
pub async fn retry_unreachable(
    table: &ResolverTable,
    kept: &mut InstallRecords,
    lookups: &RecordLookups<'_>,
) -> usize {
    // One map per record holding only the identifiers to ask again, so
    // `resolve` asks those and no others.
    let pending: Vec<Map<String, Value>> = kept
        .records
        .values()
        .map(|rec| {
            rec.lookups
                .iter()
                .filter(|l| l.outcome == LookupOutcome::Unreachable)
                .map(|l| (l.identifier.clone(), Value::String(l.value.clone())))
                .collect::<Map<_, _>>()
        })
        .filter(|m| !m.is_empty())
        .collect();
    if pending.is_empty() {
        tracing::debug!("records: no lookup unreachable, none asked again");
        return 0;
    }
    let Some(answers) = resolve(table, pending.iter(), lookups).await else {
        tracing::debug!("records: lookups not allowed, none asked again");
        return 0;
    };
    tracing::info!(
        asked = answers.len(),
        "records: unreachable lookups asked again"
    );
    let mut asked = 0;
    for (name, rec) in kept.records.iter_mut() {
        for l in rec
            .lookups
            .iter_mut()
            .filter(|l| l.outcome == LookupOutcome::Unreachable)
        {
            asked += 1;
            match answers.get(&(l.resolver.clone(), l.value.clone())) {
                Some(ResolverAnswer::Found(fields)) => {
                    for (k, v) in fields {
                        if table.keep.contains(k) && !rec.csl.contains_key(k) {
                            rec.csl.insert(k.clone(), v.clone());
                            rec.origins.insert(k.clone(), l.resolver.clone());
                        }
                    }
                    l.outcome = LookupOutcome::Resolved;
                    tracing::info!(text = %name, resolver = %l.resolver, value = %l.value, "records: retried, now resolved");
                }
                Some(ResolverAnswer::Unresolved) => {
                    l.outcome = LookupOutcome::Unresolved;
                    tracing::info!(text = %name, resolver = %l.resolver, value = %l.value, "records: retried, the resolver does not know it");
                }
                Some(ResolverAnswer::Unreachable(why)) => {
                    tracing::warn!(text = %name, resolver = %l.resolver, value = %l.value, %why, "records: retried, still unreachable");
                }
                None => {
                    tracing::warn!(text = %name, resolver = %l.resolver, value = %l.value, "records: retried, no answer kept, still unreachable");
                }
            }
        }
    }
    asked
}

/// `corpus`'s kept records, with any `unreachable` lookup asked again and the
/// file rewritten when the pass asked anything. Nothing else of the install
/// is touched. Records that are not kept, or kept for an install that did
/// not allow lookups, have nothing to ask.
pub async fn retry_last_install(corpus: &Corpus, fetch: &dyn RecordFetch) {
    let mut kept = match InstallRecords::of(corpus) {
        RecordsRead::Kept(k) if k.allow_lookups => k,
        other => {
            tracing::debug!(corpus = %corpus, kept = ?matches!(other, RecordsRead::Kept(_)), "records: no lookups to ask again");
            return;
        }
    };
    let table = match ResolverTable::from_data_file() {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(corpus = %corpus, error = %e, "records: the resolver data file does not load, unreachable lookups not retried");
            return;
        }
    };
    let lookups = RecordLookups::for_install(kept.allow_lookups, fetch);
    if retry_unreachable(&table, &mut kept, &lookups).await == 0 {
        return;
    }
    match kept.write(corpus) {
        Ok(()) => tracing::info!(corpus = %corpus, "records: kept again after the retry"),
        Err(e) => {
            tracing::warn!(corpus = %corpus, error = %e, "records: the retry did not write; the earlier records stand")
        }
    }
}
