// SPDX-License-Identifier: AGPL-3.0-or-later
//! Every spelling removed since 18f783f44 answers with its retirement and
//! what replaced it — never the usage banner, never a different verb.
//!
//! The list below is this test's own census of removed spellings, not a read
//! of `deprecation::RETIRED`, so dropping a table row turns its case red.
//! `amend design` and `audit <feature-id>` are the two that used to run
//! another verb (the charter amend, the project-wide rollup).
#![cfg(feature = "dev-tools")]

use std::process::Command;

use sovereign_cli_shared::deprecation::find_retired;

const REMOVED: &[&[&str]] = &[
    &["atos"],
    &["atos", "status", "p0-payments"],
    &["design"],
    &["design", "--solo"],
    &["project", "design"],
    &["project", "plan"],
    &["project", "amend", "design"],
    &["amend", "design"],
    &["drift", "accept"],
    &["audit", "p0-payments"],
    &["audit", "p0-payments", "--archive"],
    &["milestone", "p0-payments", "2"],
    &["drift", "p0-payments"],
    &["plan"],
    &["project", "found"],
];

#[test]
fn every_removed_spelling_is_refused_by_name() {
    for argv in REMOVED {
        let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let (old, hint) = find_retired(&[], &args)
            .unwrap_or_else(|| panic!("no RETIRED row for `svrn {}`", argv.join(" ")));
        let out = Command::new(env!("CARGO_BIN_EXE_sovereign-cli"))
            .args(*argv)
            .env("SOVEREIGN_QUIET_DEPRECATIONS", "0")
            .output()
            .expect("spawn sovereign-cli");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let label = format!("svrn {}", argv.join(" "));
        assert_eq!(out.status.code(), Some(2), "{label}:\n{text}");
        assert!(
            text.contains(&format!("`{old}` has been retired")),
            "{label} must name its retirement:\n{text}"
        );
        assert!(
            text.contains(hint),
            "{label} must name its replacement:\n{text}"
        );
        assert!(
            !text.contains("Local AI assistant with code intelligence"),
            "{label} fell through to the usage banner:\n{text}"
        );
    }
}
