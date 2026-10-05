// SPDX-License-Identifier: AGPL-3.0-or-later
#[path = "../examples/agent_admission/controller.rs"]
mod controller;
#[path = "../examples/agent_admission/fixture.rs"]
mod fixture;
#[path = "../examples/agent_admission/oracle.rs"]
mod oracle;

use controller::AdmissionController;
use serde_json::{json, Value};
use std::{fs, path::Path, time::Duration};

fn new_host() -> (tempfile::TempDir, AdmissionController) {
    let dir = tempfile::tempdir().unwrap();
    let host = AdmissionController::new(&dir.path().join("run")).unwrap();
    (dir, host)
}

fn act(host: &mut AdmissionController, action: Value) -> Value {
    host.dispatch(&action.to_string())
}

fn propose(host: &mut AdmissionController, package: &str) -> String {
    let reply = act(
        host,
        json!({"action":"propose", "formatter_package":package}),
    );
    assert_eq!(reply["status"], "proposed", "{reply}");
    reply["candidate"].as_str().unwrap().into()
}

fn check(host: &mut AdmissionController, candidate: &str) -> Value {
    act(host, json!({"action":"check", "candidate":candidate}))
}

fn accept(host: &mut AdmissionController, candidate: &str, receipt: &Value) -> Value {
    act(
        host,
        json!({"action":"accept", "candidate":candidate, "receipt":receipt["receipt"]}),
    )
}

#[test]
fn agent_admission_accepts_exact_legal_snapshot_and_repeats_idempotently() {
    let (dir, mut host) = new_host();
    let candidate = propose(&mut host, "ask-format");
    let receipt = check(&mut host, &candidate);
    assert_eq!(receipt["architecture"]["verdict"], "passed", "{receipt}");
    assert_eq!(receipt["behavior"]["verdict"], "passed", "{receipt}");
    let first = accept(&mut host, &candidate, &receipt);
    assert_eq!(first["status"], "accepted", "{first}");
    assert_eq!(first["candidate"], candidate);
    assert_eq!(accept(&mut host, &candidate, &receipt), first);
    let accepted: Value =
        serde_json::from_slice(&fs::read(dir.path().join("run/accepted.json")).unwrap()).unwrap();
    assert_eq!(accepted, first);
    assert_eq!(host.accepted_candidate(), Some(candidate.as_str()));
    assert_eq!(
        fixture::digest_tree(&host.candidate_path(&candidate).unwrap()).unwrap(),
        candidate
    );
}

#[test]
fn agent_admission_architecture_discriminates_behaviorally_green_alias() {
    let (_dir, mut host) = new_host();
    let candidate = propose(&mut host, "model-host");
    let receipt = check(&mut host, &candidate);
    // Behavior-only acceptance would accept this exact candidate (the mutant).
    assert_eq!(receipt["behavior"]["verdict"], "passed", "{receipt}");
    assert_eq!(receipt["architecture"]["verdict"], "failed", "{receipt}");
    assert!(receipt["architecture"]["reason"]
        .as_str()
        .unwrap()
        .contains("model-host"));
    assert!(receipt["edges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["from"] == "ask-app" && e["to"] == "model-host"));
    assert_eq!(accept(&mut host, &candidate, &receipt)["status"], "refused");
    assert_eq!(host.accepted_candidate(), None);
}

#[test]
fn agent_admission_missing_dependency_is_not_completion() {
    let (_dir, mut host) = new_host();
    let candidate = propose(&mut host, "none");
    let receipt = check(&mut host, &candidate);
    assert_eq!(receipt["architecture"]["verdict"], "passed");
    assert_eq!(receipt["behavior"]["verdict"], "failed", "{receipt}");
    assert_ne!(receipt["behavior"]["exit"], 0);
    assert_eq!(accept(&mut host, &candidate, &receipt)["status"], "refused");
}

#[test]
fn agent_admission_does_not_trust_forged_or_cross_candidate_receipts() {
    let (_dir, mut host) = new_host();
    let legal = propose(&mut host, "ask-format");
    let illegal = propose(&mut host, "model-host");
    assert_eq!(
        act(
            &mut host,
            json!({"action":"accept", "candidate":legal, "receipt":"invented"})
        )["status"],
        "refused"
    );
    assert_eq!(
        act(
            &mut host,
            json!({"action":"accept", "candidate":legal, "receipt":{"verdict":"passed"}})
        )["status"],
        "refused"
    );
    let receipt = check(&mut host, &legal);
    assert_eq!(accept(&mut host, &illegal, &receipt)["status"], "refused");
    assert_eq!(host.accepted_candidate(), None);
}

#[test]
fn agent_admission_rechecks_inputs_and_contract_before_acceptance() {
    let (_dir, mut host) = new_host();
    let candidate = propose(&mut host, "ask-format");
    let receipt = check(&mut host, &candidate);
    fs::write(
        host.candidate_path(&candidate)
            .unwrap()
            .join("ask-app/Cargo.toml"),
        "[package]\nname='forged'\n",
    )
    .unwrap();
    assert_eq!(accept(&mut host, &candidate, &receipt)["status"], "refused");
    let (_dir, mut host) = new_host();
    let candidate = propose(&mut host, "ask-format");
    let receipt = check(&mut host, &candidate);
    host.change_contract_for_test();
    assert_eq!(accept(&mut host, &candidate, &receipt)["status"], "refused");
}

#[test]
fn agent_admission_rejects_extra_inputs_policy_edits_and_unknown_actions() {
    let (_dir, mut host) = new_host();
    for action in [
        json!({"action":"propose", "formatter_package":"ask-format", "policy":"allow all"}),
        json!({"action":"propose", "formatter_package":"../../escape"}),
        json!({"action":"set_policy", "policy":"allow all"}),
        json!({"action":"check", "candidate":"unknown", "verdict":"passed"}),
        json!({"action":"finish", "verdict":"passed"}),
    ] {
        assert_eq!(act(&mut host, action)["status"], "refused");
    }
    let candidate = propose(&mut host, "ask-format");
    fs::write(
        host.candidate_path(&candidate).unwrap().join("build.rs"),
        "fn main() {}",
    )
    .unwrap();
    let receipt = check(&mut host, &candidate);
    assert_eq!(receipt["status"], "refused");
    assert_eq!(host.accepted_candidate(), None);
}

#[test]
fn agent_admission_unavailable_metadata_and_timeout_are_not_passes() {
    for (program, timeout) in [
        (Path::new("/not/a/cargo"), Duration::from_secs(1)),
        (Path::new("cargo"), Duration::ZERO),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut host =
            AdmissionController::with_runner(&dir.path().join("run"), program, timeout).unwrap();
        let candidate = propose(&mut host, "ask-format");
        let receipt = check(&mut host, &candidate);
        assert_eq!(
            receipt["architecture"]["verdict"], "could-not-judge",
            "{receipt}"
        );
        assert_eq!(receipt["behavior"]["verdict"], "never-ran");
        assert_eq!(accept(&mut host, &candidate, &receipt)["status"], "refused");
    }
}

#[test]
fn agent_admission_stop_is_not_success_and_receipt_files_do_not_grant_authority() {
    let (dir, mut host) = new_host();
    let candidate = propose(&mut host, "ask-format");
    fs::write(
        dir.path().join("run/receipts/forged.json"),
        r#"{"verdict":"passed"}"#,
    )
    .unwrap();
    assert_eq!(
        act(
            &mut host,
            json!({"action":"accept", "candidate":candidate, "receipt":"forged"})
        )["status"],
        "refused"
    );
    assert_eq!(
        act(&mut host, json!({"action":"stop", "reason":"cannot solve"}))["status"],
        "stopped"
    );
    assert_eq!(
        act(
            &mut host,
            json!({"action":"propose", "formatter_package":"ask-format"})
        )["status"],
        "refused"
    );
    assert_eq!(host.accepted_candidate(), None);
}

#[test]
fn agent_admission_schema_only_offers_verifier_backed_acceptance() {
    let (_dir, mut host) = new_host();
    assert!(!host.state()["schema"].to_string().contains("\"accept\""));
    let illegal = propose(&mut host, "model-host");
    check(&mut host, &illegal);
    assert!(!host.schema().to_string().contains("\"accept\""));
    let legal = propose(&mut host, "ask-format");
    let receipt = check(&mut host, &legal);
    let schema = host.schema();
    let arms = schema["oneOf"].as_array().unwrap();
    let accepts: Vec<_> = arms
        .iter()
        .filter(|a| a["properties"]["action"]["const"] == "accept")
        .collect();
    assert_eq!(accepts.len(), 1);
    assert_eq!(accepts[0]["properties"]["candidate"]["const"], legal);
    assert_eq!(
        accepts[0]["properties"]["receipt"]["const"],
        receipt["receipt"]
    );
    assert_eq!(host.state()["receipts"].as_array().unwrap().len(), 2);
    accept(&mut host, &legal, &receipt);
    assert!(host.schema().to_string().contains("\"accept\""));
    assert!(!host.schema().to_string().contains("\"check\""));
    assert_eq!(check(&mut host, &legal)["status"], "refused");
    act(&mut host, json!({"action":"stop","reason":"complete"}));
    assert_eq!(host.schema(), json!({"not":{}}));
}

#[test]
fn agent_admission_incomplete_metadata_cannot_be_a_clean_graph() {
    for text in [
        "not json",
        r#"{"packages":[],"workspace_members":[]}"#,
        r#"{"packages":[{"name":"ask-app"}]}"#,
    ] {
        assert!(oracle::edges(text, Path::new(".")).is_err());
    }
}

#[test]
fn agent_admission_novelty_prevents_successful_no_op_loops() {
    let (_dir, mut host) = new_host();
    let candidate = propose(&mut host, "ask-format");
    assert_eq!(
        act(
            &mut host,
            json!({"action":"propose","formatter_package":"ask-format"})
        )["status"],
        "refused"
    );
    let schema = host.schema();
    let proposal = schema["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["properties"]["action"]["const"] == "propose")
        .unwrap();
    assert!(!proposal["properties"]["formatter_package"]["enum"]
        .as_array()
        .unwrap()
        .contains(&json!("ask-format")));
    check(&mut host, &candidate);
    assert_eq!(check(&mut host, &candidate)["status"], "refused");
    assert!(!host.schema().to_string().contains("\"check\""));
}

#[test]
fn agent_admission_relative_root_keeps_build_outputs_outside_snapshot() {
    let dir = tempfile::Builder::new()
        .prefix("admission-relative-")
        .tempdir_in(".")
        .unwrap();
    let mut host = AdmissionController::new(&dir.path().join("run")).unwrap();
    let candidate = propose(&mut host, "ask-format");
    let receipt = check(&mut host, &candidate);
    assert_eq!(receipt["behavior"]["verdict"], "passed", "{receipt}");
    assert_eq!(
        accept(&mut host, &candidate, &receipt)["status"],
        "accepted"
    );
}

#[cfg(unix)]
#[test]
fn agent_admission_green_text_zero_tests_and_killed_runner_are_not_passes() {
    use std::os::unix::fs::PermissionsExt;
    for (plant, expected) in [
        (
            "printf 'test task_returns_expected_answer ... ok\\n1 passed; 0 failed\\n'; exit 7",
            "failed",
        ),
        ("printf '0 passed; 0 failed\\n'", "could-not-judge"),
        ("kill -KILL $$", "could-not-judge"),
        ("sleep 5", "could-not-judge"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let runner = dir.path().join("cargo-control");
        fs::write(
            &runner,
            format!("#!/bin/sh\nif [ \"$1\" = metadata ]; then exec cargo \"$@\"; fi\n{plant}\n"),
        )
        .unwrap();
        fs::set_permissions(&runner, fs::Permissions::from_mode(0o700)).unwrap();
        let mut host = AdmissionController::with_runner(
            &dir.path().join("run"),
            &runner,
            Duration::from_secs(2),
        )
        .unwrap();
        let candidate = propose(&mut host, "ask-format");
        let receipt = check(&mut host, &candidate);
        assert_eq!(receipt["architecture"]["verdict"], "passed", "{receipt}");
        assert_eq!(receipt["behavior"]["verdict"], expected, "{receipt}");
        assert_eq!(accept(&mut host, &candidate, &receipt)["status"], "refused");
    }
}
