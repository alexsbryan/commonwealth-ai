// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn ring new` — four files and a next step.
//!
//! The templates live beside this module as real `.html` and `.js` files
//! rather than as `const &str` blobs: a 150-line page embedded in a Rust
//! string literal is edited by nobody and linted by nothing (ARCH §6.2).
//! `include_str!` resolves relative to THIS file, so the set moves as one.
//!
//! # These files are the reference app, not a sample
//!
//! When the rail's journal line stopped being an `ExpenseOp`, the money rules
//! — the penny remainder, settlement idempotency, the refusals — left Rust.
//! They live in `templates/expenses.js`, and `templates/expenses.test.mjs`
//! pins them, run as part of `cargo test` by
//! [`the_reference_apps_money_rules_pass_their_own_tests`].
//!
//! So the thing a housemate starts from is the same thing the workspace
//! gates, and a scaffold whose arithmetic is wrong cannot ship. The
//! alternative — a sample nobody runs, beside a tested copy somewhere else —
//! is two implementations of one split (ARCH §10.6).

use std::path::PathBuf;

use super::flag;

const STARTER_INDEX_HTML: &str = include_str!("templates/index.html");
const STARTER_APP_JS: &str = include_str!("templates/app.js");
/// The app's own layer: what an act MEANS. The rail below it has never heard
/// of an expense.
const STARTER_EXPENSES_JS: &str = include_str!("templates/expenses.js");
/// Scaffolded alongside the rules, so an author starts with a harness rather
/// than with the intention of writing one.
const STARTER_EXPENSES_TEST: &str = include_str!("templates/expenses.test.mjs");

pub(super) fn run_new(args: &[String]) -> i32 {
    let Some(dir) = args.first().filter(|a| !a.starts_with("--")) else {
        eprintln!("ring new: where? `svrn ring new ./house-expenses`");
        return 2;
    };
    let dir = PathBuf::from(dir);
    let name = flag(args, "--name").map(str::to_string).unwrap_or_else(|| {
        title_case(
            &dir.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )
    });
    if dir.join("index.html").exists() {
        // Not an error, and it writes nothing. Re-running the scaffold is
        // what somebody does when they have lost the thread, and the useful
        // answer is "it is already here, run this next" rather than a refusal
        // they have to interpret. Never overwrites — the safe direction is
        // the one that keeps their edits.
        println!("This directory is already a ring app: {}", dir.display());
        println!();
        println!("  svrn ring dev <namespace> --dir {}", dir.display());
        return 0;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("ring new: {}: {e}", dir.display());
        return 1;
    }
    let files: [(&str, String); 4] = [
        ("index.html", STARTER_INDEX_HTML.replace("{{NAME}}", &name)),
        ("app.js", STARTER_APP_JS.to_string()),
        ("expenses.js", STARTER_EXPENSES_JS.to_string()),
        ("expenses.test.mjs", STARTER_EXPENSES_TEST.to_string()),
    ];
    for (file, body) in files {
        if let Err(e) = std::fs::write(dir.join(file), body) {
            eprintln!("ring new: write {file}: {e}");
            return 1;
        }
    }
    println!("Scaffolded `{name}` in {}", dir.display());
    println!();
    println!("  index.html + app.js   the page");
    println!("  expenses.js           what an act MEANS — the part that is yours");
    println!(
        "  expenses.test.mjs     its tests: `node --test {}`",
        dir.display()
    );
    println!();
    println!("  svrn ring roster add <you> --self --ring <namespace>");
    println!("  svrn ring dev <namespace> --dir {}", dir.display());
    0
}

fn title_case(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|s| !s.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scaffolded_page_loads_its_own_script_and_shows_gaps() {
        assert!(STARTER_INDEX_HTML.contains("./app.js"));
        assert!(STARTER_INDEX_HTML.contains("id=\"gaps\""));
        assert!(STARTER_APP_JS.contains("log.gaps"));
    }

    /// **The page must show BOTH kinds of gap.** The rail's (an op that has
    /// not arrived, a signature that does not verify) say the numbers cover a
    /// subset; the app's (an amount that is not money) say an act could not
    /// be read. Rendering one and not the other is a confident total over a
    /// subset wearing a complete-looking page, which is the §18.3 failure the
    /// whole gap mechanism exists to prevent.
    #[test]
    fn the_scaffolded_page_renders_rail_gaps_and_app_gaps_together() {
        assert!(STARTER_APP_JS.contains("log.gaps"), "the rail's");
        assert!(STARTER_APP_JS.contains("book.gaps"), "the app's own");
    }

    /// The app folds through the SDK rather than walking the log itself —
    /// which is what keeps a voided entry from being counted twice.
    #[test]
    fn the_scaffolded_app_folds_through_the_sdk() {
        assert!(STARTER_APP_JS.contains("window.ring.fold("));
        assert!(
            !STARTER_APP_JS.contains("log.ops.filter") && !STARTER_APP_JS.contains("ops.sort"),
            "the scaffold re-derives the rail's order — teaching every author who \
             copies it to do the same"
        );
    }

    /// **The door must run the validator the reducer runs.**
    ///
    /// The comment saying so stood beside a `record` call that did not, which
    /// is exactly the §7.2 smell: an assertion in prose instead of in a test.
    /// Unchecked, the page cheerfully writes an act the reducer will refuse —
    /// an unknown `kind`, a non-positive amount — and every node in the ring
    /// then reports it as a gap, permanently, because a log does not forget.
    #[test]
    fn the_scaffolded_door_runs_the_validator_before_writing() {
        let (before, after) = STARTER_APP_JS
            .split_once("expenses.writable(act, roster)")
            .expect("the submit handler writes without asking whether the act is writable");
        assert!(
            !before.contains("window.ring.record("),
            "the write happens before the validator that is supposed to gate it"
        );
        assert!(
            after.contains("window.ring.record("),
            "the validator gates nothing — there is no write after it"
        );
    }

    /// **The gate the money rules kept when they left Rust.**
    ///
    /// `expenses.js` holds the penny remainder, settlement idempotency and
    /// the refusals, and none of it is reachable from `cargo test` except
    /// through its own runner. Running it here means the workspace test gate
    /// still covers the arithmetic — under a different runtime, in the same
    /// command.
    ///
    /// A missing `node` FAILS rather than skips. "Could not judge" is not
    /// "passed" (ARCH §18.1), and a silent skip here is how the split
    /// arithmetic ends up ungated on every machine that happens to lack a
    /// toolchain nobody noticed it needed.
    #[test]
    fn the_reference_apps_money_rules_pass_their_own_tests() {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, body) in [
            ("expenses.js", STARTER_EXPENSES_JS),
            ("expenses.test.mjs", STARTER_EXPENSES_TEST),
        ] {
            std::fs::write(dir.path().join(name), body).expect("write template");
        }
        let out = std::process::Command::new("node")
            .arg("--test")
            .arg(dir.path())
            .output()
            .unwrap_or_else(|e| {
                panic!(
                    "`node` is required to gate the reference app's money rules \
                     (templates/expenses.test.mjs) and could not be run: {e}. \
                     This is not skipped on purpose — the split arithmetic left \
                     Rust and this is the only thing checking it."
                )
            });
        assert!(
            out.status.success(),
            "the reference app's money rules failed their own tests:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    #[test]
    fn title_case_reads_a_directory_name_as_a_title() {
        assert_eq!(title_case("house-expenses"), "House Expenses");
        assert_eq!(title_case("tool_lending_board"), "Tool Lending Board");
    }
}
