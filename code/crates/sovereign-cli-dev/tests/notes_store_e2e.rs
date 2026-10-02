// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn notes` and `svrn reflect` are code's verbs over code's one notes
//! store (pb-notes-verbs), run as the BUILT binary with no daemon.
//!
//! The sandbox holds two stores: code's, at the data root, and a decoy at the
//! working directory's `.sovereign/notes.db`, which the retired resolver's
//! pointer file also names. Before this row, both verbs answered from the
//! decoy. The daemon URL is a dead port, so the daemon-first route falls back
//! and says so.

#![cfg(feature = "workbench")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use corpus_engine_notes::NoteStore;

/// Nothing listens on loopback port 2: a dial is refused at once.
const DEAD_DAEMON: &str = "http://127.0.0.1:2";

async fn seed(db: &Path, marker: &str) {
    std::fs::create_dir_all(db.parent().expect("db has a parent")).expect("store dir");
    let store = NoteStore::open(db).expect("open the seed store");
    store
        .write_note(
            "decision",
            &format!("{marker} decision"),
            vec![],
            vec![],
            "sess-seed",
        )
        .await
        .expect("seed a decision");
    store
        .write_reflection(&format!("{marker} reflection"), Some("blast"), "sess-seed")
        .await
        .expect("seed a reflection");
}

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sovereign-cli-dev"))
        .args(args)
        .current_dir(home.join("repo"))
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("SVRNMESH_DATA_DIR", home.join("root"))
        .env_remove("SOVEREIGN_DATA_DIR")
        .env("SVRNMESH_DAEMON_URL", DEAD_DAEMON)
        .env_remove("SOVEREIGN_DAEMON_URL")
        .output()
        .expect("spawn sovereign-cli-dev")
}

fn text(out: &Output) -> String {
    format!(
        "status: {:?}\n--- stdout\n{}\n--- stderr\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[tokio::test]
async fn notes_list_and_reflect_answer_from_codes_store_with_no_daemon() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let decoy = home.join("repo/.sovereign/notes.db");
    seed(&home.join("root/notes.db"), "CODESTORE").await;
    seed(&decoy, "DECOYSTORE").await;
    std::fs::write(
        home.join("root/active_notes_db"),
        decoy.to_string_lossy().as_bytes(),
    )
    .expect("write the retired pointer");

    let list = run(home, &["notes", "list", "--query", "decision"]);
    let shown = text(&list);
    assert!(list.status.success(), "{shown}");
    assert!(
        shown.contains("daemon unreachable"),
        "the daemon is absent: {shown}"
    );
    assert!(shown.contains("CODESTORE decision"), "{shown}");
    assert!(
        !shown.contains("DECOYSTORE"),
        "answered from the decoy: {shown}"
    );

    let reflect = run(home, &["reflect", "--raw"]);
    let shown = text(&reflect);
    assert!(reflect.status.success(), "{shown}");
    assert!(shown.contains("CODESTORE reflection"), "{shown}");
    assert!(
        !shown.contains("DECOYSTORE"),
        "answered from the decoy: {shown}"
    );
}

/// Every production `.rs` under `src/`, with any trailing `#[cfg(test)] mod`
/// cut off, so a test's temp-dir `join("notes.db")` is not counted.
fn production_sources(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
    for entry in std::fs::read_dir(dir).expect("read src").flatten() {
        let path = entry.path();
        if path.is_dir() {
            production_sources(&path, out);
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") || path.ends_with("tests.rs") {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("read source");
        let lines: Vec<&str> = body.lines().collect();
        let cut = lines
            .windows(2)
            .position(|w| w[0].trim() == "#[cfg(test)]" && w[1].trim_start().starts_with("mod "))
            .unwrap_or(lines.len());
        out.push((path, lines[..cut].join("\n")));
    }
}

#[test]
fn code_names_its_notes_store_in_one_place() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    production_sources(&src, &mut sources);
    let sites: Vec<String> = sources
        .iter()
        .flat_map(|(path, body)| {
            let rel = path.strip_prefix(&src).unwrap().display().to_string();
            body.lines()
                .filter(|l| l.contains(".join(\"notes.db\")") && !l.trim_start().starts_with("//"))
                .map(move |l| format!("{rel}: {}", l.trim()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(
        sites.len(),
        1,
        "one decider for notes.db, found: {sites:#?}"
    );
    assert!(sites[0].starts_with("notes_db.rs: "), "{sites:#?}");
}
