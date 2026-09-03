// SPDX-License-Identifier: AGPL-3.0-or-later
//! rec-1 ring 4: tree recursion over real Rust, with the COMPILER as the
//! oracle and a collision that exists only in the merge.
//!
//! The calc fixture's merge trap was a wrong VALUE that survived into the
//! merged tree. This one is the other shape, and the one a real refactor
//! actually hits: two branches each add a private helper with the same name
//! to the same file, far enough apart that git merges both hunks cleanly,
//! and the merged crate does not compile (E0428). Each branch is green on
//! its own. No test in either branch can see it. Only `Combine` — which
//! merges the branches and re-runs the goal on the merged tree — can.
//!
//! Scripted evaluator, as ring 0: the plant has to be exact for the bar to
//! mean anything. What is real here is the subject (a cargo crate), the
//! oracle (`cargo test`, so a compile error is the failure), and the merge.

use crate::recur_fixture::{count, g, root_path, sh};
use commonwealth_tdd::recur::{
    driver::delivered_to, Driver, DriverConfig, EvalRequest, EvalResponse, Event, ScriptedEvaluator,
};
use commonwealth_tdd::recur::{
    GoalCatalog, ModelConfig, ModelEvaluator, RECUR_MODEL_INSTRUCTION_RUST,
};
use commonwealth_tdd::{Language, Workdir};
use kernel_types::Verdict;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn cargo_available() -> bool {
    Command::new("cargo")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `src/big.rs`. Both helpers are named `scale` and land in opposite ends of
/// the file: `area` adds one at the top, `text` one at the bottom, separated
/// by filler so the two hunks never touch and git merges both.
fn big_rs(area_helper: bool, text_helper: bool) -> String {
    let mut s = String::from("//! Two groups that grew up in one file.\n\n");
    if area_helper {
        s.push_str("fn scale(x: i64) -> i64 {\n    x * 2\n}\n\n");
        s.push_str("pub fn rect_area(w: i64, h: i64) -> i64 {\n    scale(w) * h\n}\n\n");
    } else {
        s.push_str("pub fn rect_area(w: i64, h: i64) -> i64 {\n    w * h\n}\n\n");
    }
    s.push_str("pub fn tri_area(b: i64, h: i64) -> i64 {\n    b * h / 2\n}\n\n");
    // Filler: the distance that makes the merge clean. Without it the two
    // hunks share context lines and git reports a conflict instead — which
    // the driver already catches, and which is NOT the case under test.
    for i in 0..8 {
        s.push_str(&format!(
            "pub fn spare_{i}(x: i64) -> i64 {{\n    x + {i}\n}}\n\n"
        ));
    }
    if text_helper {
        s.push_str("fn scale(s: &str) -> String {\n    format!(\"{s}!\")\n}\n\n");
        s.push_str("pub fn shout(s: &str) -> String {\n    scale(&s.to_uppercase())\n}\n\n");
    } else {
        s.push_str("pub fn shout(s: &str) -> String {\n    s.to_uppercase()\n}\n\n");
    }
    s.push_str("pub fn whisper(s: &str) -> String {\n    s.to_lowercase()\n}\n");
    s
}

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn rust_fixture(root: &Path) -> Workdir {
    sh(root, &["git", "init", "-q", "--initial-branch=main"]);
    sh(root, &["git", "config", "user.email", "recur@test"]);
    sh(root, &["git", "config", "user.name", "recur"]);
    // Same lesson as the pytest fixture: anything the oracle WRITES would
    // land in the tree hash and in the merge. cargo writes both of these.
    write(root, ".gitignore", "target/\nCargo.lock\n");
    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"bigmod\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n\n[workspace]\n",
    );
    write(root, "src/lib.rs", "pub mod big;\n");
    write(root, "src/big.rs", &big_rs(false, false));
    write(
        root,
        "tests/behaviour.rs",
        "use bigmod::big;\n\n#[test]\nfn area_works() {\n    assert_eq!(big::rect_area(2, 3), 12);\n}\n\n#[test]\nfn text_works() {\n    assert_eq!(big::shout(\"hi\"), \"HI!\");\n}\n",
    );
    sh(root, &["git", "add", "-A"]);
    sh(root, &["git", "commit", "-q", "-m", "fixture"]);
    Workdir::check_safe(root.to_path_buf(), false).unwrap()
}

/// Goals are whatever the oracle can run: cargo's own filter arguments.
const ROOT: &str = "--tests";
const AREA: &str = "--test behaviour area_works";
const TEXT: &str = "--test behaviour text_works";

fn cfg(scratch: PathBuf) -> DriverConfig {
    DriverConfig {
        language: Language::Rust,
        test_command: "cargo test --offline {goal}".into(),
        test_timeout: Duration::from_secs(180),
        ..DriverConfig::pytest(scratch)
    }
}

fn script(req: &EvalRequest) -> EvalResponse {
    match req.goal().0.as_str() {
        ROOT => EvalResponse::Split {
            children: vec![g(AREA), g(TEXT)],
        },
        AREA => EvalResponse::Edit {
            path: "src/big.rs".into(),
            content: big_rs(true, false),
        },
        TEXT => EvalResponse::Edit {
            path: "src/big.rs".into(),
            content: big_rs(false, true),
        },
        other => EvalResponse::GiveUp {
            reason: format!("no script for {other}"),
        },
    }
}

/// Explicit-run, like the other rec-1 rings — but for a different reason:
/// this one shells out to `cargo`, and a compiler inside the workspace test
/// run starves the timing-sensitive `sovereign-compute::supervisor` tests
/// sharing that run (measured: 3 of them failed with it in the default
/// suite, 0 without, and all 3 pass in isolation).
///
///     cargo test -p commonwealth-tdd --test main recur_ring4 -- --ignored --nocapture
#[tokio::test]
#[ignore = "spawns cargo; run it with the other rec-1 rings"]
async fn ring4_a_clean_merge_of_two_green_branches_does_not_compile() {
    if !cargo_available() {
        eprintln!("cargo unavailable — SKIPPED, this run verified nothing");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let wd = rust_fixture(&repo);
    let mut d = Driver::start(
        &wd,
        g(ROOT),
        cfg(tmp.path().join("scratch")),
        ScriptedEvaluator::new(script),
    )
    .unwrap();
    let root = d.run().await.unwrap();
    let st = d.state().clone();
    let dump = serde_json::to_string_pretty(&st.events).unwrap();

    // Each branch is green ON ITS OWN. This is the half a per-branch gate
    // sees, and it is the half that makes the defect invisible.
    let slots = delivered_to(&st, &root_path_for(ROOT));
    assert_eq!(slots.len(), 2, "{dump}");
    assert!(
        slots.iter().all(|v| v.verdict == Verdict::Passed),
        "both branches must be green on their own\n{dump}"
    );

    // The merge is CLEAN — git took both hunks. If this fires as a conflict
    // the fixture has drifted and is testing the wrong thing.
    let merged = st
        .events
        .iter()
        .find_map(|e| match e {
            Event::Merged {
                verdict, reason, ..
            } => Some((*verdict, reason.clone())),
            _ => None,
        })
        .expect("a Merged event");
    assert!(
        !merged.1.contains("merge conflict"),
        "the collision must survive a CLEAN merge, not become a conflict: {}",
        merged.1
    );
    assert_eq!(merged.0, Verdict::Failed, "{dump}");
    assert!(
        merged.1.starts_with("passed in every branch"),
        "{}",
        merged.1
    );
    assert_eq!(root.verdict, Verdict::Failed, "{dump}");
    assert_eq!(
        count(&st, |e| matches!(e, Event::Merged { .. })),
        1,
        "{dump}"
    );

    // The defect itself: one file, two `fn scale`, neither branch's doing
    // alone. The compiler is what says so.
    let merged_src = std::fs::read_to_string(repo.join("src/big.rs")).unwrap();
    assert_eq!(
        merged_src.matches("fn scale").count(),
        2,
        "the merged file should carry both helpers:\n{merged_src}"
    );
    let out = Command::new("cargo")
        .args(["test", "--offline", "--tests"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        text.contains("E0428") || text.contains("defined multiple times"),
        "the merged tree should fail to compile with a duplicate definition:\n{text}"
    );
}

fn root_path_for(goal: &str) -> commonwealth_tdd::recur::GoalPath {
    let _ = root_path();
    commonwealth_tdd::recur::GoalPath::root(g(goal))
}

// ── the model arm ────────────────────────────────────────────────────────

/// The goal tree, stated outright. Nothing about "--tests" and "--test
/// behaviour area_works" lets a relation be INFERRED from the strings, which
/// is exactly why the catalog is data now.
fn rust_catalog() -> GoalCatalog {
    GoalCatalog::from_tree([(g(AREA), g(ROOT)), (g(TEXT), g(ROOT))])
}

fn model() -> String {
    std::env::var("RECUR_MODEL").unwrap_or_else(|_| "Qwopus3.5-4B-v3-MTP-Q8_0".into())
}

/// Ring 4 with the local model in the evaluator seat, on a subject whose
/// goal ids are cargo arguments.
///
/// What is BARRED here is the mechanism: the catalog yields parts for the
/// root, the grammar therefore carries a `split` arm naming them, every
/// reply parses, and the runs are identical. What is REPORTED, never
/// demanded, is whether the model chooses to decompose. Asserting a split
/// would be asserting a model behaviour the harness has no right to
/// require — and the first run showed why: given three asks and one small
/// file, the model fixed both failures at the root and the oracle went
/// green. That is the better engineering choice, and it means `Combine` is
/// exercised by the scripted test, not this one.
#[tokio::test]
#[ignore = "needs the daemon and a resident model; spawns cargo"]
async fn ring4_model_arm_decomposes_and_reaches_the_combine() {
    if !cargo_available() {
        eprintln!("cargo unavailable — SKIPPED, this run verified nothing");
        return;
    }
    let runs: usize = std::env::var("RECUR_RUNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let mut logs = Vec::new();
    for i in 0..runs {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let wd = rust_fixture(&repo);
        let mc = ModelConfig {
            language: Language::Rust,
            ..ModelConfig::local(model())
        };
        let ev = ModelEvaluator::new(mc, rust_catalog());
        let mut cfg = cfg(tmp.path().join("scratch"));
        cfg.instruction = RECUR_MODEL_INSTRUCTION_RUST;
        let mut d = Driver::start(&wd, g(ROOT), cfg, ev).unwrap();
        let root = d.run().await.unwrap();
        let st = d.state().clone();
        let splits = count(&st, |e| matches!(e, Event::Split { .. }));
        let merges = count(&st, |e| matches!(e, Event::Merged { .. }));
        let asks = d.evaluator().asks();
        eprintln!(
            "run {i}: root {} ({}) — splits={splits} merges={merges} asks={} unparsed={}",
            root.verdict.as_str(),
            root.reason,
            asks.len(),
            asks.iter().filter(|a| !a.parsed).count()
        );
        for a in &asks {
            eprintln!(
                "    d{} {:>6}ms {}",
                a.depth,
                a.wall_ms,
                a.reply.lines().next().unwrap_or("")
            );
        }
        // BAR grammar: the closed set holds for a subject whose goal ids are
        // cargo arguments.
        assert!(
            asks.iter().all(|a| a.parsed),
            "unparseable reply: {asks:#?}"
        );
        logs.push((splits, merges, crate::recur_fixture::strip_paths(&st)));
    }
    // BAR determinism, as every other ring.
    for (i, l) in logs.iter().enumerate().skip(1) {
        assert_eq!(logs[0].2, l.2, "run {i} diverged from run 0");
    }
    // BAR mechanism: the split arm is genuinely on offer for this subject.
    // This is the thing that was broken — parts inferred from pytest syntax
    // came back empty for cargo goals and the arm was silently dropped.
    let cat = rust_catalog();
    let parts = cat.parts_of(&g(ROOT));
    assert_eq!(parts.len(), 2, "the catalog must name the root's parts");
    let grammar = ModelEvaluator::grammar(cat.goals(), &parts, &["src/big.rs".into()]);
    assert!(grammar.contains("split: "), "{grammar}");
    assert!(grammar.contains(AREA), "{grammar}");
    assert!(grammar.contains(TEXT), "{grammar}");

    // REPORTED, not barred.
    let (splits, merges, _) = &logs[0];
    eprintln!("decomposed: splits={splits} merges={merges} over {runs} identical runs");
    assert_eq!(
        (*splits > 0),
        (*merges > 0),
        "a split without a merge, or a merge without a split, is a driver bug"
    );
}

/// The same subject under a ONE-ASK budget per frame. One whole-file edit
/// cannot fix two independent failures, so a frame that wants to get green
/// has to decompose — and the budget is not a trick, it is the recursion's
/// reduction step, the thing that has to shrink for the process to
/// terminate. This is where a model-driven `Combine` becomes reachable, and
/// where the planted collision can actually bite a model rather than a
/// script. Reported, not barred, except for parse and determinism.
#[tokio::test]
#[ignore = "needs the daemon and a resident model; spawns cargo"]
async fn ring4_model_arm_under_a_one_ask_budget() {
    if !cargo_available() {
        eprintln!("cargo unavailable — SKIPPED, this run verified nothing");
        return;
    }
    let runs: usize = std::env::var("RECUR_RUNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let mut logs = Vec::new();
    for i in 0..runs {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let wd = rust_fixture(&repo);
        let mc = ModelConfig {
            language: Language::Rust,
            ..ModelConfig::local(model())
        };
        let ev = ModelEvaluator::new(mc, rust_catalog());
        let mut cfg = cfg(tmp.path().join("scratch"));
        cfg.instruction = RECUR_MODEL_INSTRUCTION_RUST;
        cfg.asks_per_frame = 1;
        let mut d = Driver::start(&wd, g(ROOT), cfg, ev).unwrap();
        let root = d.run().await.unwrap();
        let st = d.state().clone();
        let splits = count(&st, |e| matches!(e, Event::Split { .. }));
        let merges = count(&st, |e| matches!(e, Event::Merged { .. }));
        let asks = d.evaluator().asks();
        eprintln!(
            "run {i}: root {} ({}) — splits={splits} merges={merges} asks={}",
            root.verdict.as_str(),
            root.reason,
            asks.len()
        );
        for a in &asks {
            eprintln!(
                "    d{} {:>6}ms {}",
                a.depth,
                a.wall_ms,
                a.reply.lines().next().unwrap_or("")
            );
        }
        for e in &st.events {
            if let Event::Merged {
                verdict, reason, ..
            } = e
            {
                eprintln!("    MERGED {} — {reason}", verdict.as_str());
            }
        }
        assert!(
            asks.iter().all(|a| a.parsed),
            "unparseable reply: {asks:#?}"
        );
        logs.push(crate::recur_fixture::strip_paths(&st));
    }
    for (i, l) in logs.iter().enumerate().skip(1) {
        assert_eq!(logs[0], *l, "run {i} diverged from run 0");
    }
}
