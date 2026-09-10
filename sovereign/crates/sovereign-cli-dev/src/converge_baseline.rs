// SPDX-License-Identifier: AGPL-3.0-or-later
//! The concept ratchet's BASELINE FILE — its shape, its reader, its writer.
//!
//! Split out of `converge_cmd` because they answer different questions. That
//! module decides the VERDICT (did the count rise, can the graph speak for
//! this commit, is a rise real); this one owns the FORMAT on disk, and the
//! format is where the compatibility lives: every peer clone still carries the
//! bare scalar this file replaced, and reading one must yield its count rather
//! than a refusal.
//!
//! `quality/baselines/concepts.txt`, since 2026-09-10:
//!
//! ```text
//! # concept-gate baseline — the reachable duplicate-name SET, and its size.
//! # ...
//! 39            <- the count, first non-comment line
//! Admission     <- one name per line after it
//! AtomSpan
//! ```

use std::collections::BTreeSet;

/// The header every ratchet baseline in `quality/baselines/` carries: what the
/// file is, and the exact command that regenerates it.
const BASELINE_HEADER: &str = "\
# concept-gate baseline — the reachable duplicate-name SET, and its size.
# MACHINE-WRITTEN. Regenerate (snapshot current state — defend the diff in review):
#   cargo run -p xtask -- concept-gate --update-baseline
# The first non-comment line is the COUNT; every line after it is one name.
";

/// What `quality/baselines/concepts.txt` holds.
///
/// It held a bare scalar until 2026-09-10, and that is why a rise could never
/// be dispositioned: every sibling ratchet stores per-key rows — `clock_reads`
/// names the file, `oversized` names the file, `lines.tsv` names the crate —
/// so a red tells you WHICH. This one could only say that four somethings
/// crossed. Recording the set closes that, and the set is free: `census`
/// already computes the rows the count is drawn from.
pub struct ConceptBaseline {
    pub count: Option<usize>,
    pub names: BTreeSet<String>,
    /// False for a file minted before the names were recorded. That is a
    /// DIFFERENT fact from "no names" and must not be collapsed into it
    /// (ARCH §18.3) — an unnamed baseline cannot name an addition, and the
    /// relay says so rather than reporting an empty list as "nothing added".
    pub named: bool,
    /// Set when the file EXISTS but no count could be read from it. `count:
    /// None` alone cannot tell a missing baseline from a corrupt one, and the
    /// two want different repairs — mint, versus look at what is in the file.
    /// Both are NEVER-RAN; only one of them is normal.
    pub unreadable: Option<String>,
}

pub fn read_baseline(path: &std::path::Path) -> ConceptBaseline {
    let absent = |unreadable| ConceptBaseline {
        count: None,
        names: BTreeSet::new(),
        named: false,
        unreadable,
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        // A missing file is "not minted yet". Anything else — a permission
        // error, a directory, invalid UTF-8 — is a file this gate could not
        // read, and reporting that as "no baseline" would send the reader to
        // `--mint` for a problem minting cannot fix.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return absent(None),
        Err(e) => return absent(Some(format!("{}: {e}", path.display()))),
    };
    let mut rows = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'));
    let Some(first) = rows.next() else {
        return absent(Some(format!(
            "{}: no non-comment line — the count is the first one",
            path.display()
        )));
    };
    let Ok(count) = first.parse::<usize>() else {
        return absent(Some(format!(
            "{}: first non-comment line is {first:?}, which is not a count",
            path.display()
        )));
    };
    let names: BTreeSet<String> = rows.map(String::from).collect();
    ConceptBaseline {
        count: Some(count),
        named: !names.is_empty(),
        names,
        unreadable: None,
    }
}

pub fn write_baseline(path: &std::path::Path, n: usize, names: &[&str]) -> std::io::Result<()> {
    let mut out = String::from(BASELINE_HEADER);
    out.push_str(&format!("{n}\n"));
    for name in names {
        out.push_str(name);
        out.push('\n');
    }
    std::fs::write(path, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── the baseline is a SET, and one shape of it predates that ───────────

    #[test]
    fn a_minted_baseline_round_trips_its_names() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("concepts.txt");
        write_baseline(&f, 3, &["Admission", "Verdict", "WorkUnit"]).unwrap();

        let b = read_baseline(&f);
        assert_eq!(b.count, Some(3));
        assert!(b.named, "a file written with names reads back as named");
        assert_eq!(
            b.names,
            ["Admission", "Verdict", "WorkUnit"]
                .into_iter()
                .map(String::from)
                .collect::<BTreeSet<_>>()
        );

        // The header must not be mistaken for the count, and the count must
        // still be the first non-comment line — `instruments.toml` declares
        // this file `kind = "count"`.
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.starts_with("# concept-gate baseline"), "{text}");
        let first = text
            .lines()
            .find(|l| !l.starts_with('#') && !l.trim().is_empty())
            .unwrap();
        assert_eq!(first, "3");
    }

    /// Every baseline on every peer clone is the bare scalar this file held
    /// until 2026-09-10. Reading one must still yield the COUNT — a gate that
    /// refuses on the old shape is a gate that fails on every machine that has
    /// not re-minted yet.
    #[test]
    fn the_bare_scalar_baseline_still_reads_as_a_count() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("concepts.txt");
        std::fs::write(&f, "35\n").unwrap();

        let b = read_baseline(&f);
        assert_eq!(b.count, Some(35));
        assert!(
            !b.named,
            "an unnamed baseline must report itself as unnamed, so the relay \
             says `cannot say WHICH` instead of `nothing was added`"
        );
        assert!(b.names.is_empty());
    }

    #[test]
    fn a_missing_baseline_is_no_count_and_not_a_zero() {
        let b = read_baseline(std::path::Path::new("/nonexistent/concepts.txt"));
        assert_eq!(b.count, None, "absence is reported, never defaulted");
        assert!(!b.named);
        assert_eq!(
            b.unreadable, None,
            "a file that is not there is `none yet`, which --mint does fix"
        );
    }

    /// `count: None` alone cannot tell "not minted yet" from "the file is
    /// there and says something else". Both are NEVER-RAN, and only one of
    /// them is repaired by `--mint`, so the two must not collapse (§18.3).
    #[test]
    fn a_baseline_that_is_present_but_unparseable_says_so_instead_of_none() {
        let dir = tempfile::tempdir().unwrap();

        let junk = dir.path().join("junk.txt");
        std::fs::write(&junk, "# header only\nthirty-five\n").unwrap();
        let b = read_baseline(&junk);
        assert_eq!(b.count, None);
        let why = b.unreadable.expect("a present-but-bad file is UNREADABLE");
        assert!(why.contains("thirty-five"), "quotes what it found: {why}");

        let empty = dir.path().join("empty.txt");
        std::fs::write(&empty, "# nothing but a header\n").unwrap();
        let b = read_baseline(&empty);
        assert_eq!(b.count, None);
        assert!(
            b.unreadable
                .as_deref()
                .is_some_and(|w| w.contains("no non-comment line")),
            "a header-only file is unreadable, not absent: {:?}",
            b.unreadable
        );
    }
}
