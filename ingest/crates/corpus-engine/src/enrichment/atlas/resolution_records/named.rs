// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a record is called: the name its readers gave it. A window read names
//! each particular it writes (`SUBJECT_NAME_ATTRIBUTE`, gathered on each
//! statement as `Described::names`); a record RESOLVE builds is called by the
//! name most of its statements' readers gave it, so one misplaced statement
//! cannot rename it; a tie goes to the name met first in fold order (the
//! opener's, then the earliest). Its other read names are its aliases, and its
//! opening passage stays on it as its first passage (the record view's text),
//! shown once and never as a name. A record no reader named keeps its opening
//! passage as its name, and no first passage repeats it. Shown, never
//! compared: the record's id is its opening statement's
//! (`exact_entity_content_hash`), so a name never decides which record is which.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use tracing::debug;

use crate::enrichment::atlas::resolve_records::{Record, Statement};

/// What a record is called and where it was first read.
pub(super) struct RecordName {
    pub(super) name: String,
    pub(super) aliases: Vec<String>,
    /// The record's opening passage when a reader's name names it; empty when
    /// the passage is its name.
    pub(super) first_passage: String,
}

/// The name, aliases and first passage of `record` of type `ty`, its
/// statements looked up in `statements`: [`given`] with the opening passage
/// `surface` as its first passage, or `surface` as its name and none.
pub(super) fn of_record(
    ty: &str,
    record: &Record,
    statements: &HashMap<&str, &Statement>,
    surface: String,
) -> RecordName {
    let read: Vec<_> = record
        .statements
        .iter()
        .filter_map(|s| statements.get(s.as_str()))
        .map(|s| &s.described.names)
        .collect();
    match given(&read, &record.described.names) {
        Some((name, aliases)) => {
            debug!(r#type = ty, record = %record.id, %name, aliases = aliases.len(), "atlas/resolve: a record is called by the name most of its readers gave it; its opening passage is its first passage");
            RecordName {
                name,
                aliases,
                first_passage: surface,
            }
        }
        None => {
            debug!(r#type = ty, record = %record.id, "atlas/resolve: no reader named the record; its opening passage names it");
            RecordName {
                name: surface,
                aliases: Vec::new(),
                first_passage: String::new(),
            }
        }
    }
}

/// The name and aliases of a record whose statements, in fold order, were
/// read under `names_in_order`, and whose readers read it under `all` names in
/// all; `None` when no reader named it. The name is the one the most
/// statements were read under; of names read equally often, the first met in
/// fold order (a statement's own names in order), so the opener's wins a tie
/// and the choice never depends on a run.
pub(super) fn given(
    names_in_order: &[&BTreeSet<String>],
    all: &BTreeSet<String>,
) -> Option<(String, Vec<String>)> {
    let named = || {
        names_in_order
            .iter()
            .flat_map(|names| names.iter().filter(|n| !n.trim().is_empty()))
    };
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    for n in named() {
        *count.entry(n.as_str()).or_default() += 1;
    }
    let most = count.values().copied().max()?;
    let name = named().find(|n| count[n.as_str()] == most)?.clone();
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

    /// Failing input, named: saltgrass's record of the apprentice was renamed
    /// for the constable because a misplaced statement about the constable was
    /// its earliest named one (arm guard-replay-sg, 2026-10-16), and the
    /// bank's answerer then named the constable as the one who found the body.
    /// A record is called by the name most of its statements' readers gave it.
    #[test]
    fn a_record_is_called_by_the_name_most_of_its_readers_gave() {
        let (stray, ivo) = (set(&["Constable Hale"]), set(&["Ivo"]));
        let all = set(&["Constable Hale", "Ivo"]);
        assert_eq!(
            given(&[&stray, &ivo, &ivo], &all),
            Some(("Ivo".to_string(), vec!["Constable Hale".to_string()]))
        );
    }

    /// A tie goes to the name met first in fold order: the opener's, then the
    /// earliest; an unnamed statement counts for none; a blank name is none.
    #[test]
    fn a_tie_goes_to_the_name_met_first() {
        // "Nell Varden" sorts after "Nell", so a rule that took the first name
        // in sorted order would fail here.
        let (unnamed, full, short) = (set(&[]), set(&["Nell Varden"]), set(&["Nell"]));
        let all = set(&["Nell", "Nell Varden"]);
        assert_eq!(
            given(&[&unnamed, &full, &short], &all),
            Some(("Nell Varden".to_string(), vec!["Nell".to_string()]))
        );
        assert_eq!(given(&[&unnamed], &set(&[])), None, "no reader named it");
        assert_eq!(
            given(&[&set(&["  "])], &set(&["  "])),
            None,
            "a blank name is none"
        );
    }
}
