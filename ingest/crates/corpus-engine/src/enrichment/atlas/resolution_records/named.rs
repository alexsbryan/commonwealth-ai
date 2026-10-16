// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a record is called: the name its readers gave it. A window read names
//! each particular it writes (`SUBJECT_NAME_ATTRIBUTE`, gathered on each
//! statement as `Described::names`); a record RESOLVE builds is called by the
//! name read for the earliest of its statements that has one, its statements
//! in the order they were folded, and its other read names are its aliases.
//! A record no reader named keeps its opening passage. Shown, never compared:
//! the record's id is its opening statement's (`exact_entity_content_hash`),
//! so a name never decides which record is which.

use std::collections::{BTreeSet, HashMap};

use tracing::debug;

use crate::enrichment::atlas::resolve_records::{Record, Statement};

/// The name and aliases of `record` of type `ty`, its statements looked up in
/// `statements`: [`given`], or its opening passage `surface` and none.
pub(super) fn of_record(
    ty: &str,
    record: &Record,
    statements: &HashMap<&str, &Statement>,
    surface: String,
) -> (String, Vec<String>) {
    let read: Vec<_> = record
        .statements
        .iter()
        .filter_map(|s| statements.get(s.as_str()))
        .map(|s| &s.described.names)
        .collect();
    match given(&read, &record.described.names) {
        Some((name, aliases)) => {
            debug!(r#type = ty, record = %record.id, %name, aliases = aliases.len(), "atlas/resolve: a record is called by the name its reader gave it");
            (name, aliases)
        }
        None => {
            debug!(r#type = ty, record = %record.id, "atlas/resolve: no reader named the record; its opening passage names it");
            (surface, Vec::new())
        }
    }
}

/// The name and aliases of a record whose statements, in fold order, were
/// read under `names_in_order`, and whose readers read it under `all` names in
/// all; `None` when no reader named it. Of one statement's several names the
/// first in order is taken, so the choice never depends on a run.
pub(super) fn given(
    names_in_order: &[&BTreeSet<String>],
    all: &BTreeSet<String>,
) -> Option<(String, Vec<String>)> {
    let name = names_in_order
        .iter()
        .find_map(|names| names.iter().find(|n| !n.trim().is_empty()))?
        .clone();
    let aliases = all
        .iter()
        .filter(|n| **n != name && !n.trim().is_empty())
        .cloned()
        .collect();
    Some((name, aliases))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// Failing input, named: saltgrass's records were called by their opening
    /// passages, and a resolved reference printed the passage (arm
    /// bundle1-fresh-sg: records 27.6k -> 50.7k chars, `speaker` alone 10k).
    /// The earliest named statement's name is the record's; the rest are
    /// aliases; an unnamed first statement is passed over.
    #[test]
    fn a_record_is_called_by_its_earliest_read_name() {
        // The earliest name sorts after the later one, so a rule that took the
        // first name in sorted order would fail here.
        let (first, unnamed, later) = (set(&["Maren Ashquill"]), set(&[]), set(&["Maren"]));
        let all = set(&["Maren", "Maren Ashquill"]);
        assert_eq!(
            given(&[&unnamed, &first, &later], &all),
            Some(("Maren Ashquill".to_string(), vec!["Maren".to_string()]))
        );
        assert_eq!(given(&[&unnamed], &set(&[])), None, "no reader named it");
        assert_eq!(
            given(&[&set(&["  "])], &set(&["  "])),
            None,
            "a blank name is none"
        );
    }
}
