// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn journal` — read, share, or switch off the local journals.
//!
//! A **journal** is one feature's append-only local record of how it
//! behaved on the developer's real work: metadata only, on their disk,
//! and never sent anywhere. The machinery (file layout, rotation, caps,
//! the off-switches) is `sovereign_contracts::types::journal`; this
//! module is the read and consent surface over whatever streams exist.
//!
//! # Adding a feature's journal to this command
//!
//! One file and one row. Write a `journal_cmd/<feature>.rs` exporting a
//! `pub const VIEW: JournalView` — a name, a title, its
//! [`JournalStream`], and two renderers — then add it to [`VIEWS`].
//! Nothing else here changes: `bundle`, `off`, `on`, `clear` and the
//! stream selector are all written against the registry, not against a
//! `match` on feature names (`ARCH_PRINCIPLES.md` §4 — open sets are
//! registries).
//!
//! `next_edit` is the first row and reads exactly like the second one
//! will.
//!
//! # Usage
//!
//! ```text
//! svrn journal                       # stats for every journal
//! svrn journal <stream>              # stats for one
//! svrn journal show [--last N]      # the raw records
//! svrn journal bundle [--out P]     # one file to hand back + a manifest of what is in it
//! svrn journal off | on             # stop / resume recording
//! svrn journal clear                # delete every record
//! ```
//!
//! Every subcommand takes an optional leading stream name, which scopes
//! it: `svrn journal next-edit off` stops one stream, `svrn journal off`
//! stops them all.
//!
//! # What is deliberately absent
//!
//! **There is no send, submit, or upload subcommand, and adding one is a
//! design change, not a feature.** `bundle` produces a file and tells
//! the developer precisely what is in it; where it goes after that is
//! their decision, made having read the contents.

mod next_edit;

use std::io::Write;
use std::path::{Path, PathBuf};

use sovereign_contracts::types::{journal_dir, JournalStream, DISABLED_MARKER};

use crate::util::help::{Help, HelpSection};

/// One feature's journal, as this command sees it.
///
/// A plain descriptor with two function pointers rather than a trait
/// object: the registry is a `const` table, the two behaviours that vary
/// are both pure `&Path -> Vec<String>` renderers, and keeping it
/// data-shaped means a new stream cannot accidentally override the parts
/// that must not vary (the caps, the retention, the off-switch — all
/// enforced in [`JournalStream`]).
pub struct JournalView {
    /// Selector: `svrn journal <name> ...`. Conventionally the stream's
    /// file stem.
    pub name: &'static str,
    /// Human title, shown as a heading when more than one journal exists.
    pub title: &'static str,
    /// One clause completing "records ...", for the multi-stream listing
    /// and for `--help`. This is the developer's summary of what they are
    /// consenting to keep, so write it plainly.
    pub records: &'static str,
    /// The stream this view reads.
    pub stream: JournalStream,
    /// Render the stats block for this stream.
    pub stats: fn(&Path) -> Vec<String>,
    /// Render the records, oldest first, capped at `last`.
    pub show: fn(&Path, usize) -> Vec<String>,
}

/// Every journal `svrn journal` can read. Add a row to register one.
pub const VIEWS: &[JournalView] = &[next_edit::VIEW];

const HELP: Help = Help {
    command: "svrn journal",
    summary: "Your local journals — metadata only, on this machine, yours to share or delete.",
    sections: &[
        HelpSection::Usage(
            "svrn journal [<stream>] [stats | show | bundle | off | on | clear] [flags]",
        ),
        HelpSection::Subcommands(&[
            (
                "stats",
                "What each feature did and what became of it (default)",
            ),
            ("show", "The raw records, oldest first"),
            (
                "bundle",
                "Write ONE file to hand back, and print what is in it",
            ),
            (
                "off",
                "Stop recording (the daemon notices on its next record)",
            ),
            ("on", "Resume recording"),
            ("clear", "Delete every record"),
        ]),
        HelpSection::Flags(&[
            ("--last <N>", "show: only the last N records (default 20)"),
            (
                "--out <path>",
                "bundle: where to write (default ./sovereign-journal-bundle.jsonl)",
            ),
            ("--yes", "clear: skip the confirmation"),
            ("--help, -h", "Show this message"),
        ]),
        HelpSection::Notes(
            "A leading stream name scopes any subcommand: `svrn journal next-edit off` stops one \
             journal, `svrn journal off` stops them all. Journals record WHY a feature did what it \
             did — never your code: not the document, the file path, the matched text, or anything \
             it proposed. `svrn journal bundle` prints the complete list of fields in the file it \
             writes, so you can check that claim rather than take it. Nothing is ever sent \
             anywhere; there is no upload path in the code.",
        ),
    ],
};

/// `svrn journal [<stream>] <sub>`. Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }

    // A leading stream NAME scopes the command; anything else is the
    // subcommand. Resolving in this order means `svrn journal show` and
    // `svrn journal next-edit show` both read naturally, and a future
    // stream name cannot collide with a subcommand unless someone names
    // a feature "bundle".
    let (selected, rest): (Option<&JournalView>, &[String]) = match args.first() {
        Some(first) => match VIEWS.iter().find(|v| v.name == first.as_str()) {
            Some(v) => (Some(v), &args[1..]),
            None => (None, args),
        },
        None => (None, &[][..]),
    };
    let views: Vec<&JournalView> = match selected {
        Some(v) => vec![v],
        None => VIEWS.iter().collect(),
    };

    let sub = rest.first().map(String::as_str).unwrap_or("stats");
    let flags = if rest.is_empty() { &[][..] } else { &rest[1..] };
    let dir = journal_dir();

    match sub {
        "stats" => cmd_stats(&dir, &views),
        "show" => cmd_show(&dir, &views, flags),
        "bundle" => cmd_bundle(&dir, &views, flags),
        "off" => cmd_switch(&dir, selected, false),
        "on" => cmd_switch(&dir, selected, true),
        "clear" => cmd_clear(&dir, &views, flags),
        other => {
            eprintln!("unknown subcommand `{other}` — try `svrn journal --help`");
            eprintln!(
                "known journals: {}",
                VIEWS.iter().map(|v| v.name).collect::<Vec<_>>().join(", ")
            );
            2
        }
    }
}

/// Heading for one stream, printed only when more than one is in play —
/// a single-journal machine should not have to read a section header to
/// find its numbers.
fn heading(view: &JournalView, multi: bool) {
    if multi {
        println!();
        println!("── {} — `{}` ──", view.title, view.name);
        // What this stream records, printed where the numbers are. On a
        // multi-journal machine the developer should not have to open
        // `--help` to learn what they are keeping.
        println!("   records {}", view.records);
    }
}

fn cmd_stats(dir: &Path, views: &[&JournalView]) -> i32 {
    println!("journals · {}", dir.display());
    let multi = views.len() > 1;
    for view in views {
        heading(view, multi);
        if !view.stream.enabled(dir) {
            println!(
                "  recording: OFF (`svrn journal {} on` to resume)",
                view.name
            );
        }
        for line in (view.stats)(dir) {
            println!("{line}");
        }
    }
    0
}

fn cmd_show(dir: &Path, views: &[&JournalView], flags: &[String]) -> i32 {
    let last = flag_value(flags, "--last")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20);
    let multi = views.len() > 1;
    for view in views {
        heading(view, multi);
        for line in (view.show)(dir, last) {
            println!("{line}");
        }
    }
    0
}

// ── bundle ───────────────────────────────────────────────────────────

/// Everything that is in a bundle, derived FROM the bundle body rather
/// than from the records that went into it.
///
/// That direction is the point: the manifest is a claim about the bytes
/// on disk, so it has to be computed from those bytes or it is only a
/// claim about our intentions (ARCH §18.1 — a check you cannot watch
/// fail is not a check). Feature-agnostic by construction: it counts
/// `kind` values and collects JSON keys, so a new stream is audited by
/// this code the day it is added.
#[derive(Debug, Default, PartialEq)]
pub struct BundleManifest {
    pub lines: usize,
    pub bytes: usize,
    /// `kind` value → count, for streams whose lines carry one.
    pub kinds: Vec<(String, usize)>,
    /// Every JSON key that appears anywhere in the bundle, at any depth,
    /// sorted. This is the auditable part: a reader can check that no
    /// field capable of carrying code is present.
    pub fields: Vec<String>,
}

/// Concatenate the selected streams' raw lines and describe the result.
///
/// Raw lines, never a re-serialization: the developer is auditing the
/// bytes they are about to share.
pub fn build_bundle(raw: &[String]) -> (String, BundleManifest) {
    let mut body = String::new();
    for line in raw {
        body.push_str(line);
        body.push('\n');
    }
    let mut fields: std::collections::BTreeSet<String> = Default::default();
    let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
    let mut lines = 0usize;
    for text in body.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
            continue;
        };
        lines += 1;
        if let Some(kind) = v.get("kind").and_then(|k| k.as_str()) {
            *kinds.entry(kind.to_string()).or_default() += 1;
        }
        collect_keys(&v, &mut fields);
    }
    let m = BundleManifest {
        lines,
        bytes: body.len(),
        kinds: kinds.into_iter().collect(),
        fields: fields.into_iter().collect(),
    };
    (body, m)
}

/// Every key at every depth. Depth matters: a nested object is exactly
/// where a code-bearing field would hide from a shallow audit.
fn collect_keys(v: &serde_json::Value, out: &mut std::collections::BTreeSet<String>) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, child) in map {
                out.insert(k.clone());
                collect_keys(child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_keys(child, out);
            }
        }
        _ => {}
    }
}

fn cmd_bundle(dir: &Path, views: &[&JournalView], flags: &[String]) -> i32 {
    let out = flag_value(flags, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("sovereign-journal-bundle.jsonl"));

    let mut raw = Vec::new();
    let mut unreadable = 0usize;
    for view in views {
        let (lines, bad) = view.stream.read_raw(dir);
        raw.extend(lines);
        unreadable += bad;
    }
    if raw.is_empty() {
        println!("nothing to bundle — no records in {}", dir.display());
        return 0;
    }
    let (body, m) = build_bundle(&raw);
    if let Err(e) = std::fs::write(&out, &body) {
        eprintln!("could not write {}: {e}", out.display());
        return 1;
    }
    print_manifest(&out, &m, unreadable, views);
    0
}

fn print_manifest(out: &Path, m: &BundleManifest, unreadable: usize, views: &[&JournalView]) {
    println!("wrote {}", out.display());
    println!(
        "  {} bytes · {} record(s) from {}",
        m.bytes,
        m.lines,
        views.iter().map(|v| v.name).collect::<Vec<_>>().join(", ")
    );
    for (kind, n) in &m.kinds {
        println!("    {n} × {kind}");
    }
    if unreadable > 0 {
        println!("  {unreadable} unreadable file(s) were NOT included");
    }
    println!("\nEvery field in that file, and there are no others:");
    for chunk in m.fields.chunks(6) {
        println!("  {}", chunk.join("  "));
    }
    println!(
        "\nNo document text, no file paths, no matched or proposed code. Read the file before you \
         share it; it is small and it is plain JSON, one record per line."
    );
    println!("Nothing has been sent anywhere. This command only writes a file.");
}

// ── off / on / clear ─────────────────────────────────────────────────

/// Switch recording. Unscoped writes the GLOBAL marker (every stream,
/// including ones added later); scoped writes only that stream's.
fn cmd_switch(dir: &Path, selected: Option<&JournalView>, on: bool) -> i32 {
    let (marker, what) = match selected {
        Some(v) => (v.stream.marker_in(dir), v.name),
        None => (dir.join(DISABLED_MARKER), "every journal"),
    };
    if on {
        match std::fs::remove_file(&marker) {
            Ok(()) => println!("recording ON for {what} — resumes on the next record"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!("recording was already on for {what}");
                // A global marker silently overrides a per-stream `on`;
                // saying so beats leaving the user to wonder why nothing
                // appeared.
                if selected.is_some() && dir.join(DISABLED_MARKER).exists() {
                    println!(
                        "  note: journaling is off GLOBALLY — `svrn journal on` to lift that too"
                    );
                }
            }
            Err(e) => {
                eprintln!("could not remove {}: {e}", marker.display());
                return 1;
            }
        }
        return 0;
    }
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("could not create {}: {e}", dir.display());
        return 1;
    }
    if let Err(e) = std::fs::write(&marker, "written by `svrn journal off`\n") {
        eprintln!("could not write {}: {e}", marker.display());
        return 1;
    }
    println!("recording OFF for {what} — stops on the next record");
    println!("existing records are untouched; `svrn journal clear` deletes them");
    0
}

fn cmd_clear(dir: &Path, views: &[&JournalView], flags: &[String]) -> i32 {
    let total: usize = views.iter().map(|v| v.stream.read_raw(dir).0.len()).sum();
    if total == 0 {
        println!("nothing to clear");
        return 0;
    }
    if !flags.iter().any(|a| a == "--yes") {
        print!("delete {total} record(s) in {}? [y/N] ", dir.display());
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err()
            || !matches!(answer.trim(), "y" | "Y" | "yes")
        {
            println!("left alone");
            return 0;
        }
    }
    let removed: usize = views.iter().map(|v| v.stream.clear(dir)).sum();
    println!("removed {removed} journal file(s)");
    0
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == flag)?;
    args.get(i + 1).map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::types::{
        JournalLine, NextEditEpisode, NextEditOutcome, NextEditOutcomeLine, NEXT_EDIT_STREAM,
    };

    fn seed(dir: &Path) {
        let mut e = NextEditEpisode::new("model", 2, 1400);
        e.episode_id = "ep-1".into();
        e.path_ext = Some("rs".into());
        e.region_bytes = Some(512);
        NEXT_EDIT_STREAM
            .append(dir, &JournalLine::Episode(e))
            .unwrap();
        NEXT_EDIT_STREAM
            .append(
                dir,
                &JournalLine::Outcome(NextEditOutcomeLine::new(
                    "ep-1".into(),
                    NextEditOutcome::Accepted,
                )),
            )
            .unwrap();
    }

    /// The registry is the extension point, so its invariants are worth
    /// a test: unique selectable names, and none colliding with a
    /// subcommand (which the arg resolver would then shadow).
    #[test]
    fn every_registered_view_is_addressable() {
        let mut names: Vec<&str> = VIEWS.iter().map(|v| v.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "two journals share a name: {names:?}");
        for name in &names {
            assert!(
                !matches!(*name, "stats" | "show" | "bundle" | "off" | "on" | "clear"),
                "journal name `{name}` shadows a subcommand"
            );
            assert!(!name.is_empty() && !name.starts_with('-'));
        }
        assert!(
            !VIEWS.is_empty(),
            "a command with no journals has nothing to show"
        );
    }

    /// Every view must say what it records — that sentence is what the
    /// developer is consenting to keep.
    #[test]
    fn every_registered_view_describes_what_it_records() {
        for v in VIEWS {
            assert!(!v.title.trim().is_empty(), "{} has no title", v.name);
            assert!(
                !v.records.trim().is_empty(),
                "{} does not say what it records",
                v.name
            );
        }
    }

    /// The manifest is the audit. If it did not enumerate exactly the
    /// keys in the file, a developer reading it would be checking a
    /// claim about our intentions rather than about their data.
    #[test]
    fn manifest_field_list_matches_the_file_exactly() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let (raw, _) = NEXT_EDIT_STREAM.read_raw(dir.path());
        let (body, m) = build_bundle(&raw);

        let mut actual: std::collections::BTreeSet<String> = Default::default();
        for line in body.lines() {
            collect_keys(&serde_json::from_str(line).unwrap(), &mut actual);
        }
        assert_eq!(m.fields, actual.into_iter().collect::<Vec<_>>());
        assert_eq!(m.bytes, body.len());
        assert_eq!(m.lines, 2);
        assert_eq!(
            m.kinds,
            vec![("episode".to_string(), 1), ("outcome".to_string(), 1)]
        );
    }

    /// The claim the manifest makes on the developer's behalf: no field
    /// in a bundle can carry code. Named keys, so a schema change in ANY
    /// registered stream fails here instead of shipping.
    #[test]
    fn no_bundle_field_can_carry_code() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path());
        let mut raw = Vec::new();
        for v in VIEWS {
            raw.extend(v.stream.read_raw(dir.path()).0);
        }
        let (_, m) = build_bundle(&raw);
        for forbidden in [
            "text",
            "path",
            "needle",
            "rule_find",
            "rule_replace",
            "new_text",
            "old",
            "new",
            "verify_hunk",
            "region",
            "edits",
            "prefix",
            "suffix",
            "content",
            "source",
            "body",
        ] {
            assert!(
                !m.fields.iter().any(|f| f == forbidden),
                "bundle manifest lists `{forbidden}`, which can carry code: {:?}",
                m.fields
            );
        }
        for expected in ["episode_id", "path_ext", "region_bytes", "outcome"] {
            assert!(
                m.fields.iter().any(|f| f == expected),
                "missing `{expected}`"
            );
        }
    }

    #[test]
    fn an_empty_journal_bundles_to_an_empty_manifest() {
        let (body, m) = build_bundle(&[]);
        assert!(body.is_empty());
        assert_eq!(m, BundleManifest::default());
    }

    /// `off` unscoped must stop streams that do not exist yet, which is
    /// what the GLOBAL marker buys over writing one marker per known
    /// stream.
    #[test]
    fn unscoped_off_uses_the_global_marker() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        assert_eq!(cmd_switch(dir.path(), None, false), 0);
        assert!(dir.path().join(DISABLED_MARKER).exists());
        for v in VIEWS {
            assert!(!v.stream.enabled(dir.path()), "{} kept recording", v.name);
        }
    }

    #[test]
    fn scoped_off_leaves_the_other_streams_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        let target = &VIEWS[0];
        assert_eq!(cmd_switch(dir.path(), Some(target), false), 0);
        assert!(
            !dir.path().join(DISABLED_MARKER).exists(),
            "scoped off must not go global"
        );
        assert!(!target.stream.enabled(dir.path()));
    }

    #[test]
    fn flag_value_reads_the_next_arg_only() {
        let args: Vec<String> = ["--last", "5", "--out"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(flag_value(&args, "--last"), Some("5"));
        assert_eq!(
            flag_value(&args, "--out"),
            None,
            "a trailing flag has no value"
        );
        assert_eq!(flag_value(&args, "--nope"), None);
    }
}
