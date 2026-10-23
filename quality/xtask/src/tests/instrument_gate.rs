use super::*;

/// The verdict-line census reads a row's command to find its script, and
/// the shapes in this registry are not uniform: `python3 x.py`,
/// `./scripts/x.sh --flag`, a bare path, and plenty of rows that name no
/// script at all. A resolver that missed one would quietly shrink the
/// denominator, which is the failure mode a census cannot survive.
#[test]
fn a_rows_command_resolves_to_its_script_or_to_nothing() {
    let root = common::repo_root();
    assert_eq!(
        script_of("python3 scripts/nc-thesis.py", &root).as_deref(),
        Some("scripts/nc-thesis.py")
    );
    assert_eq!(
        script_of("./scripts/sovereign-ci-bench.sh --quick", &root).as_deref(),
        Some("scripts/sovereign-ci-bench.sh")
    );
    assert_eq!(
        script_of("python3 scripts/co_liveness.py verify", &root).as_deref(),
        Some("scripts/co_liveness.py")
    );
    // A cargo/xtask row names no script, and a path that does not exist is
    // not a reference to anything — both resolve to nothing rather than to
    // a string the census would then fail to read.
    assert_eq!(script_of("cargo xtask env-gate", &root), None);
    assert_eq!(script_of("python3 scripts/not-a-real-tool.py", &root), None);
}

/// The two markers that make the vocabulary detectable, and the two that
/// do not. PASS and FAIL are in every script in this repo and say nothing
/// about which decision system a tool is in.
#[test]
fn the_vocabulary_is_the_two_verdicts_an_exit_code_cannot_express() {
    let has = renders_verdict;
    assert!(has("status=\"SKIP(no-data)\"  # COULD-NOT-JUDGE"));
    assert!(has("rec.update(verdict=NEVER_RAN, detail=...)"));
    assert!(!has("echo \"PASS — everything is fine\""));
    assert!(!has("if rc != 0: print(\"FAIL\")"));
}

fn keys(line: &str) -> Vec<String> {
    keys_in_line(Path::new("/nonexistent"), line)
        .iter()
        .map(Key::label)
        .collect()
}

#[test]
fn npm_npx_and_cargo_invocations_become_keys() {
    assert_eq!(keys("      - run: npm run check"), vec!["npm:check"]);
    assert_eq!(
        keys("npm --prefix clients/desktop run test:e2e"),
        vec!["npm:test:e2e"]
    );
    assert_eq!(
        keys("      - run: npx playwright test"),
        vec!["npx:playwright test"]
    );
    assert_eq!(
        keys("      - run: cargo fmt --all --check"),
        vec!["cargo:fmt"]
    );
    assert_eq!(keys("  cargo deny check advisories"), vec!["cargo:deny"]);
}

/// `npx playwright install` is setup and `npx playwright test` is the
/// suite. One key for both would let the setup step ride the suite's
/// registration.
#[test]
fn the_npx_subcommand_is_part_of_the_key() {
    assert_ne!(
        keys("npx playwright install --with-deps chromium"),
        keys("npx playwright test")
    );
}

/// `cargo build -p xtask` is a build step; `cargo build --workspace
/// --timings` is the weekly critical-path bench. Same subcommand, and
/// collapsing them would exempt the bench by accident.
#[test]
fn a_timings_build_is_a_different_key_from_a_plain_build() {
    assert!(keys("      - run: cargo build -p xtask").is_empty());
    assert_eq!(
        keys("      - run: cargo build --workspace --timings -j 3"),
        vec!["cargo:build --timings"]
    );
}

/// Both xtask spellings in the repo reach the same key: `cargo xtask
/// arch-gate` locally, `cargo run -p xtask -- api-gate` in weekly,
/// `./target/debug/xtask "$g"` in the CI ratchets job.
#[test]
fn every_xtask_spelling_reaches_the_same_key_shape() {
    assert_eq!(keys("cargo xtask arch-gate"), vec!["xtask:arch-gate"]);
    assert_eq!(
        keys("      - run: cargo run -p xtask -- api-gate"),
        vec!["xtask:api-gate"]
    );
    assert_eq!(
        keys("            ./target/debug/xtask size-gate"),
        vec!["xtask:size-gate"]
    );
    // Neither a loop variable nor a prose placeholder is a gate name.
    assert!(keys("            ./target/debug/xtask \"$g\" || rc=1").is_empty());
    assert!(keys("            Weekly `cargo xtask <gate> --tighten` run.").is_empty());
}

/// The loop and the array are where eight of the nine ratchets are named.
#[test]
fn the_gate_lists_in_ci_and_pre_push_are_expanded() {
    let ci = "          for g in docs-gate arch-gate boundary-gate; do\n            ./target/debug/xtask \"$g\" || rc=1\n          done\n";
    let got: Vec<String> = xtask_loop_keys(ci, "ci.yml")
        .iter()
        .map(|(k, _)| k.label())
        .collect();
    assert_eq!(
        got,
        ["xtask:docs-gate", "xtask:arch-gate", "xtask:boundary-gate"]
    );

    let hook = "XTASK_GATES=(docs-gate arch-gate concept-gate)\nrun \"$xtask\" \"$g\"\n";
    let got: Vec<String> = xtask_loop_keys(hook, "pre-push.sh")
        .iter()
        .map(|(k, _)| k.label())
        .collect();
    assert_eq!(
        got,
        ["xtask:docs-gate", "xtask:arch-gate", "xtask:concept-gate"]
    );

    // A file that never mentions xtask contributes nothing, so an
    // unrelated `for` loop cannot mint gate names.
    assert!(xtask_loop_keys("for f in a b c; do echo $f; done\n", "x.sh").is_empty());
}

#[test]
fn package_json_script_names_are_read_as_keys() {
    let pkg = "{\n  \"name\": \"x\",\n  \"scripts\": {\n    \"check\": \"svelte-check\",\n    \"test:e2e:real\": \"playwright test\"\n  },\n  \"devDependencies\": {\n    \"vite\": \"^5\"\n  }\n}\n";
    let got: Vec<String> = npm_script_names(pkg, "package.json")
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(got, ["check", "test:e2e:real"]);
}

/// Matching is against the DECLARED spellings only. An alias is written
/// down or it does not exist.
#[test]
fn a_key_matches_only_a_declared_spelling() {
    let k = Key::Npx("playwright".into(), "test".into());
    assert!(!k.matches("npm run test:e2e"));
    assert!(k.matches("npx playwright test"));
    assert!(Key::Npm("test:e2e".into()).matches("npm run test:e2e"));
    assert!(!Key::Npm("test".into()).matches("npm run test:e2e"));
    assert!(Key::Xtask("arch-gate".into()).matches("cargo run -p xtask -- arch-gate"));
    assert!(Key::Script("scripts/wizard-verify.sh".into())
        .matches("run_capped 900 scripts/wizard-verify.sh"));
}
