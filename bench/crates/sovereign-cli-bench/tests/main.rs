// SPDX-License-Identifier: AGPL-3.0-or-later
//! bench's binary answers its own verbs and refuses what is not its. The
//! dispatcher execs it for `svrn bench|eval` and `svrn quality lane`, so this
//! is the binary those spellings reach.

use std::process::Command;

fn exit_of(args: &[&str]) -> i32 {
    Command::new(env!("CARGO_BIN_EXE_sovereign-cli-bench"))
        .args(args)
        .output()
        .expect("run sovereign-cli-bench")
        .status
        .code()
        .expect("exited, not signalled")
}

/// Failing input: a verb missing from the binary's table answers 2 here.
#[test]
fn the_binary_answers_bench_eval_and_quality_lane() {
    for verb in ["bench", "eval", "quality-lane"] {
        assert_eq!(exit_of(&[verb, "--help"]), 0, "{verb} --help");
    }
    assert_eq!(exit_of(&["chat", "--help"]), 2, "chat is not bench's");
    assert_eq!(exit_of(&[]), 2, "no verb is a usage error");
}

#[test]
fn er_score_reports_recovery_through_the_shipped_binary() {
    let dir = tempfile::tempdir().unwrap();
    let predicted = dir.path().join("predicted.json");
    let gold = dir.path().join("gold.json");
    std::fs::write(&predicted, r#"{"a":"p1","c":"p2"}"#).unwrap();
    std::fs::write(&gold, r#"{"a":"g1","b":"g1","c":"g2","d":"g2"}"#).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sovereign-cli-bench"))
        .args(["bench", "er-score"])
        .arg(predicted)
        .arg(gold)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["b_cubed"]["f1"], 1.0);
    assert_eq!(report["recovery_b_cubed"]["precision"], 1.0);
    assert_eq!(report["recovery_b_cubed"]["recall"], 0.25);
    assert_eq!(report["recovery_b_cubed"]["f1"], 0.4);
}
