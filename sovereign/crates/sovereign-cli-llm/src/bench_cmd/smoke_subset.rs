// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign/bench/smoke.toml` — which items a `svrn quality check` lane runs.
//!
//! ONE reader, because the same file is consulted by three verbs that have
//! nothing else in common (`bench all`'s `eval run` subprocess,
//! `bench chaos-monkey run`, and the `quality lane bench` wrapper that expands
//! `knowledge-gym`'s `--fixture` list). A second parser is a second answer to
//! "what is in the smoke subset" (§10.6).
//!
//! # Why declared and not sampled
//!
//! `bench all --sample-questions N` and `chaos-monkey --limit N` both exist.
//! Both were rejected for this: a count picks a DIFFERENT set as the bank
//! grows, and `sovereign/bench/README.md` records what that costs — a sampled
//! lane's baseline is cap-specific, so moving the cap false-fires the lane's
//! hard gate against a subset it never ran. The `subset_id` is in the
//! quality-check fingerprint, so editing an ids list moves the stack and last
//! week's numbers stop being comparable, which is the intended behaviour.
//!
//! # Absent, malformed and unlisted are three answers
//!
//! - The file is ABSENT → `Err`. A caller that asked for a subset by name and
//!   got none ran the whole bank, which is the expensive accident this file
//!   exists to prevent.
//! - The file is MALFORMED → `Err` naming it.
//! - The bank has no row in the named subset → `Err` naming the bank. Not
//!   "run it whole": a bank nobody curated is a bank nobody priced.
//! - A declared id is not in the bank → the CALLER refuses, via
//!   [`SmokeSelection::check_all_present`]. A subset that silently shrank is a lane
//!   that quietly stopped checking something (ARCH §18.3).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// What one bank's row in a subset says to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmokeSelection {
    /// Every item in the bank — declared, not defaulted.
    Full,
    /// Exactly these ids, in the bank's own order.
    Ids(BTreeSet<String>),
}

impl SmokeSelection {
    /// Does this selection keep the item with this id?
    pub fn keeps(&self, id: &str) -> bool {
        match self {
            SmokeSelection::Full => true,
            SmokeSelection::Ids(s) => s.contains(id),
        }
    }

    /// The declared ids the bank does not contain. Empty is the healthy
    /// answer; anything else is a stale subset and the caller must refuse
    /// rather than run a smaller lane than the one it declared.
    pub fn check_all_present<'a>(&self, present: impl Iterator<Item = &'a str>) -> Vec<String> {
        let SmokeSelection::Ids(want) = self else {
            return Vec::new();
        };
        let have: BTreeSet<&str> = present.collect();
        want.iter()
            .filter(|id| !have.contains(id.as_str()))
            .cloned()
            .collect()
    }

    /// How many ids were named, for a log line. `None` for `Full` — a count
    /// there would be a claim about the bank, not about the subset.
    pub fn declared(&self) -> Option<usize> {
        match self {
            SmokeSelection::Full => None,
            SmokeSelection::Ids(s) => Some(s.len()),
        }
    }
}

/// The one path. Not a flag on each verb: three callers with three different
/// notions of "root" would be three ways to point at a different file.
pub fn smoke_path() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("quality").is_dir() && dir.join("sovereign").is_dir() {
            return Some(dir.join("sovereign/bench/smoke.toml"));
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Look up one bank's selection inside a named subset.
///
/// `bank` is compared after stripping the checkout prefix, so a caller that
/// holds an absolute path and a file that declares a repo-relative one agree
/// without either having to normalise the other's shape.
pub fn selection_for(subset_id: &str, bank: &Path) -> Result<SmokeSelection, String> {
    let path = smoke_path().ok_or_else(|| {
        "--smoke-subset needs sovereign/bench/smoke.toml; run from a source checkout".to_string()
    })?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    selection_in(&text, subset_id, bank, &path)
}

/// The selection for a subset that declares exactly ONE bank.
///
/// The `{ids:<subset_id>}` expansion in a lane's argv has no bank to name —
/// `knowledge-gym`'s "bank" is a fixtures directory and the flag it feeds
/// (`--fixture`) takes slugs, not a path. So the subset itself must be
/// single-bank, and a subset that grew a second bank is REFUSED rather than
/// silently expanded from whichever row came first.
pub fn selection_for_sole_bank(subset_id: &str) -> Result<SmokeSelection, String> {
    let path = smoke_path().ok_or_else(|| {
        "--smoke-subset needs sovereign/bench/smoke.toml; run from a source checkout".to_string()
    })?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    sole_bank_in(&text, subset_id, &path)
}

fn sole_bank_in(text: &str, subset_id: &str, origin: &Path) -> Result<SmokeSelection, String> {
    let doc: toml::Value =
        toml::from_str(text).map_err(|e| format!("{}: {e}", origin.display()))?;
    let rows = doc
        .get("subset")
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("{}: declares no [[subset]] rows", origin.display()))?;
    let banks: Vec<&str> = rows
        .iter()
        .filter(|r| r.get("subset_id").and_then(|v| v.as_str()) == Some(subset_id))
        .filter_map(|r| r.get("bank").and_then(|v| v.as_str()))
        .collect();
    match banks.as_slice() {
        [] => Err(format!("{}: no subset `{subset_id}`", origin.display())),
        [one] => selection_in(text, subset_id, Path::new(one), origin),
        many => Err(format!(
            "{}: subset `{subset_id}` spans {} banks ({}); an id expansion cannot say which",
            origin.display(),
            many.len(),
            many.join(", ")
        )),
    }
}

/// The parse and lookup, separated from the filesystem so the rules are
/// testable without a checkout.
pub fn selection_in(
    text: &str,
    subset_id: &str,
    bank: &Path,
    origin: &Path,
) -> Result<SmokeSelection, String> {
    let doc: toml::Value =
        toml::from_str(text).map_err(|e| format!("{}: {e}", origin.display()))?;
    let rows = doc
        .get("subset")
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("{}: declares no [[subset]] rows", origin.display()))?;

    let want_bank = tail_path(bank);
    let mut subset_exists = false;
    for row in rows {
        let Some(id) = row.get("subset_id").and_then(|v| v.as_str()) else {
            return Err(format!(
                "{}: a [[subset]] row has no subset_id",
                origin.display()
            ));
        };
        if id != subset_id {
            continue;
        }
        subset_exists = true;
        let Some(row_bank) = row.get("bank").and_then(|v| v.as_str()) else {
            return Err(format!(
                "{}: subset `{subset_id}` has a row with no bank",
                origin.display()
            ));
        };
        if tail_path(Path::new(row_bank)) != want_bank {
            continue;
        }
        return match (row.get("mode").and_then(|v| v.as_str()), row.get("ids")) {
            (Some("full"), None) => Ok(SmokeSelection::Full),
            (None, Some(ids)) => {
                let arr = ids.as_array().ok_or_else(|| {
                    format!(
                        "{}: `ids` for {row_bank} must be an array",
                        origin.display()
                    )
                })?;
                let set: BTreeSet<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                if set.len() != arr.len() {
                    return Err(format!(
                        "{}: `ids` for {row_bank} holds a non-string or a duplicate",
                        origin.display()
                    ));
                }
                if set.is_empty() {
                    return Err(format!(
                        "{}: `ids` for {row_bank} is empty — a lane that runs nothing verifies \
                         nothing; say mode = \"full\" or name the ids",
                        origin.display()
                    ));
                }
                Ok(SmokeSelection::Ids(set))
            }
            // Both or neither: two answers to one question, refused rather
            // than resolved by precedence.
            _ => Err(format!(
                "{}: the row for {row_bank} in subset `{subset_id}` must declare EITHER \
                 mode = \"full\" OR an `ids` list, not both and not neither",
                origin.display()
            )),
        };
    }
    if subset_exists {
        Err(format!(
            "{}: subset `{subset_id}` does not declare bank `{}`. Every bank a smoke lane \
             touches must have a row — an undeclared bank would run whole",
            origin.display(),
            bank.display()
        ))
    } else {
        Err(format!("{}: no subset `{subset_id}`", origin.display()))
    }
}

/// The last three components of a path, lowercased separators aside — enough
/// to distinguish `sep/questions.toml` from `wikipedia/questions.toml` while
/// letting an absolute path match a repo-relative declaration.
fn tail_path(p: &Path) -> String {
    let parts: Vec<String> = p
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let n = parts.len().min(3);
    parts[parts.len() - n..].join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"
schema_version = 1
[[subset]]
subset_id = "a"
bank = "sovereign/bench/sep/questions.toml"
ids = ["q1", "q2"]
[[subset]]
subset_id = "a"
bank = "sovereign/bench/routing/cells_v1.toml"
mode = "full"
[[subset]]
subset_id = "b"
bank = "sovereign/bench/sep/questions.toml"
ids = ["q3"]
"#;

    fn sel(subset: &str, bank: &str) -> Result<SmokeSelection, String> {
        selection_in(FILE, subset, Path::new(bank), Path::new("smoke.toml"))
    }

    /// The same bank in two subsets is two different selections. This is why
    /// the flag names the SUBSET and not the file.
    #[test]
    fn one_bank_in_two_subsets_is_two_selections() {
        assert_eq!(
            sel("a", "sovereign/bench/sep/questions.toml"),
            Ok(SmokeSelection::Ids(
                ["q1".to_string(), "q2".to_string()].into_iter().collect()
            ))
        );
        assert_eq!(
            sel("b", "sovereign/bench/sep/questions.toml"),
            Ok(SmokeSelection::Ids(
                ["q3".to_string()].into_iter().collect()
            ))
        );
    }

    #[test]
    fn an_absolute_path_matches_a_repo_relative_declaration() {
        assert_eq!(
            sel(
                "a",
                "/home/x/dev/commonwealth-ai/sovereign/bench/routing/cells_v1.toml"
            ),
            Ok(SmokeSelection::Full)
        );
    }

    /// The three refusals. Each is a case where "run the whole bank" would be
    /// the convenient answer and the wrong one.
    #[test]
    fn an_unlisted_bank_an_unknown_subset_and_a_malformed_file_all_refuse() {
        let unlisted = sel("a", "sovereign/bench/wikipedia/questions.toml").unwrap_err();
        assert!(unlisted.contains("does not declare bank"), "{unlisted}");
        let unknown = sel("zzz", "sovereign/bench/sep/questions.toml").unwrap_err();
        assert!(unknown.contains("no subset `zzz`"), "{unknown}");
        let bad = selection_in(
            "[[subset\nnope",
            "a",
            Path::new("x.toml"),
            Path::new("smoke.toml"),
        )
        .unwrap_err();
        assert!(bad.contains("smoke.toml"), "{bad}");
    }

    /// `mode` and `ids` are two answers to one question. Neither is refused
    /// as well: a row with neither would run the bank whole, which is the
    /// expensive accident this file exists to prevent.
    #[test]
    fn a_row_with_both_or_neither_is_refused() {
        for row in ["mode = \"full\"\nids = [\"q1\"]", "# neither", "ids = []"] {
            let f = format!("[[subset]]\nsubset_id = \"a\"\nbank = \"b/c/d.toml\"\n{row}\n");
            assert!(
                selection_in(&f, "a", Path::new("b/c/d.toml"), Path::new("s.toml")).is_err(),
                "row `{row}` must be refused"
            );
        }
    }

    /// A stale id is the failure this whole file is exposed to: someone
    /// renames a question, the subset keeps the old id, and the lane runs
    /// four items where it declared five — reading exactly like a pass.
    #[test]
    fn a_declared_id_the_bank_no_longer_has_is_named() {
        let s = SmokeSelection::Ids(["q1".into(), "q2".into()].into_iter().collect());
        assert!(s
            .check_all_present(["q1", "q2", "q9"].into_iter())
            .is_empty());
        assert_eq!(
            s.check_all_present(["q1", "q9"].into_iter()),
            vec!["q2".to_string()]
        );
        assert!(SmokeSelection::Full
            .check_all_present([].into_iter())
            .is_empty());
    }

    /// The `{ids:...}` expansion has no bank to name, so a subset that grew a
    /// second bank must refuse rather than expand from whichever row is
    /// first in the file.
    #[test]
    fn a_sole_bank_expansion_refuses_a_subset_that_spans_two() {
        assert_eq!(
            sole_bank_in(FILE, "b", Path::new("smoke.toml")),
            Ok(SmokeSelection::Ids(
                ["q3".to_string()].into_iter().collect()
            ))
        );
        let err = sole_bank_in(FILE, "a", Path::new("smoke.toml")).unwrap_err();
        assert!(err.contains("spans 2 banks"), "{err}");
    }

    #[test]
    fn full_keeps_everything_and_ids_keep_only_theirs() {
        assert!(SmokeSelection::Full.keeps("anything"));
        let s = SmokeSelection::Ids(["q1".into()].into_iter().collect());
        assert!(s.keeps("q1"));
        assert!(!s.keeps("q2"));
        assert_eq!(s.declared(), Some(1));
        assert_eq!(SmokeSelection::Full.declared(), None);
    }
}
