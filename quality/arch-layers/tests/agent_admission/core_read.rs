// SPDX-License-Identifier: AGPL-3.0-or-later
use super::{controller, core_read, episode, fixture, oracle, source};

use controller::AdmissionController;
use serde_json::{json, Value};
use std::{fs, path::Path, time::Duration};

fn new_core(root: &Path) -> AdmissionController {
    AdmissionController::with_core_read(root, &std::env::current_dir().unwrap()).unwrap()
}

fn act(host: &mut AdmissionController, value: Value) -> Value {
    host.dispatch(&value.to_string())
}
fn methods() -> Vec<&'static str> {
    vec![
        "embed",
        "installed_indexes",
        "open_index_for_corpus",
        "index_dir",
        "foreground_lease",
        "builtin_corpora",
    ]
}
fn observe(host: &mut AdmissionController) {
    let result = act(
        host,
        json!({"action":"lookup","path":source::PATHS[0],"start":1,"end":20}),
    );
    assert_eq!(result["status"], "observed", "{result}");
    assert_eq!(result["observation"]["base"], source::BASE);
    let probe = act(host, json!({"action":"probe_existing"}));
    assert_eq!(probe["status"], "probed", "{probe}");
    assert_eq!(probe["verdict"], "failed", "{probe}");
    assert!(result["observation"]["text"]
        .as_str()
        .unwrap()
        .contains("pub trait IndexSource"));
}
fn proposal(
    host: &mut AdmissionController,
    target: &str,
    binding: &str,
    methods: Vec<&str>,
) -> String {
    let result = act(
        host,
        json!({"action":"propose_extension","target":target,"binding":binding,"methods":methods}),
    );
    assert_eq!(result["status"], "proposed", "{result}");
    result["candidate"].as_str().unwrap().to_owned()
}
fn check(host: &mut AdmissionController, id: &str) -> Value {
    act(host, json!({"action":"check","candidate":id}))
}
fn accept(host: &mut AdmissionController, id: &str, checked: &Value) -> Value {
    act(
        host,
        json!({"action":"accept","candidate":id,"receipt":checked["receipt"]}),
    )
}

#[test]
fn core_read_exact_extension_requires_observation_compiler_and_bound_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = new_core(&tmp.path().join("run"));
    assert!(!host.schema().to_string().contains("propose_extension"));
    let early = act(
        &mut host,
        json!({"action":"propose_extension","target":"contract","binding":"port","methods":methods()}),
    );
    assert_eq!(early["status"], "refused");
    observe(&mut host);
    assert!(host.schema().to_string().contains("propose_extension"));
    let id = proposal(&mut host, "contract", "port", methods());
    assert!(!host.schema().to_string().contains("\"accept\""));
    let result = check(&mut host, &id);
    assert_eq!(result["architecture"]["verdict"], "passed", "{result}");
    assert_eq!(result["behavior"]["verdict"], "passed", "{result}");
    assert!(host.schema().to_string().contains("\"accept\""));
    assert_eq!(accept(&mut host, &id, &result)["status"], "accepted");
    assert_eq!(
        fixture::digest_tree(host.candidate_path(&id).unwrap()).unwrap(),
        id
    );
}

#[test]
fn core_read_behaviorally_green_concrete_engine_dependency_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = new_core(&tmp.path().join("run"));
    observe(&mut host);
    let id = proposal(&mut host, "engine", "engine", methods());
    let result = check(&mut host, &id);
    assert_eq!(result["behavior"]["verdict"], "passed", "{result}");
    assert_eq!(result["architecture"]["verdict"], "failed", "{result}");
    assert!(result["architecture"]["reason"]
        .as_str()
        .unwrap()
        .contains("engine-probe"));
    assert_eq!(accept(&mut host, &id, &result)["status"], "refused");
    assert_eq!(host.accepted_candidate(), None);
}

#[test]
fn core_read_incomplete_interface_is_a_compiler_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = new_core(&tmp.path().join("run"));
    observe(&mut host);
    let mut selected = methods();
    selected.retain(|m| *m != "foreground_lease");
    let id = proposal(&mut host, "contract", "port", selected);
    let result = check(&mut host, &id);
    assert_eq!(result["architecture"]["verdict"], "passed", "{result}");
    assert_eq!(result["behavior"]["verdict"], "failed", "{result}");
    assert_eq!(accept(&mut host, &id, &result)["status"], "refused");
}

#[test]
fn core_read_prose_labels_and_new_service_are_not_operative_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = new_core(&tmp.path().join("run"));
    observe(&mut host);
    for extra in [
        json!({"disposition":"EXTEND"}),
        json!({"delta":"retain the concrete engine"}),
        json!({"new_service":"read-server"}),
    ] {
        let mut action = json!({"action":"propose_extension","target":"engine","binding":"engine","methods":methods()});
        action
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_eq!(act(&mut host, action)["status"], "refused");
    }
    let duplicate = act(
        &mut host,
        json!({"action":"propose_extension","target":"contract","binding":"port","methods":["embed","embed"]}),
    );
    assert_eq!(duplicate["status"], "refused");
}

#[test]
fn core_read_mutated_artifact_cannot_use_a_passing_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = new_core(&tmp.path().join("run"));
    observe(&mut host);
    let id = proposal(&mut host, "contract", "port", methods());
    let result = check(&mut host, &id);
    assert_eq!(result["behavior"]["verdict"], "passed", "{result}");
    fs::write(
        host.candidate_path(&id)
            .unwrap()
            .join("core-probe/Cargo.toml"),
        "tampered",
    )
    .unwrap();
    assert_eq!(accept(&mut host, &id, &result)["status"], "refused");
}

#[test]
fn core_read_compiler_observes_wrong_return_type() {
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("candidate");
    core_read::write(
        &tree,
        "contract",
        &methods().into_iter().map(str::to_owned).collect::<Vec<_>>(),
        "port",
    )
    .unwrap();
    let path = tree.join("corpus-index/src/source.rs");
    let original = fs::read_to_string(&path).unwrap();
    assert!(original.contains("Result<Vec<f32>>"));
    fs::write(
        &path,
        original.replace("Result<Vec<f32>>", "Result<Vec<f64>>"),
    )
    .unwrap();
    let (exit, _, error) = oracle::run(
        Path::new("cargo"),
        episode::CORE_READ.behavior_args,
        &tree,
        &tmp.path().join("logs"),
        "wrong-type",
        Duration::from_secs(60),
    )
    .unwrap();
    assert_ne!(exit, 0);
    assert!(
        error.contains("f64") || error.contains("incompatible"),
        "{error}"
    );
}

#[test]
fn core_read_baseline_evidence_requires_every_expected_call_and_no_other_errors() {
    let required: Vec<String> = methods().into_iter().map(str::to_owned).collect();
    let event = |method: &str, file: &str, code: &str| {
        json!({
        "reason":"compiler-message", "message":{"level":"error","code":{"code":code},
        "message":format!("no method named `{method}` found for reference `&dyn IndexSource` in the current scope"),
        "spans":[{"is_primary":true,"file_name":file}]}}).to_string()
    };
    let good = required
        .iter()
        .map(|m| event(m, "core-probe/src/lib.rs", "E0599"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(oracle::missing_reads(&good, &required));
    assert!(!oracle::missing_reads(
        &event("unrelated", "core-probe/src/lib.rs", "E0599"),
        &required
    ));
    assert!(!oracle::missing_reads(
        &good.replace("core-probe/src/lib.rs", "dependency/src/lib.rs"),
        &required
    ));
    assert!(!oracle::missing_reads(
        &format!(
            "{good}\n{}",
            event("missing_type", "core-probe/src/lib.rs", "E0425")
        ),
        &required
    ));
    assert!(!oracle::missing_reads(
        &good.lines().skip(1).collect::<Vec<_>>().join("\n"),
        &required
    ));
}

#[test]
fn core_read_header_observation_does_not_offer_a_probe_or_extension() {
    let tmp = tempfile::tempdir().unwrap();
    let mut host = new_core(&tmp.path().join("run"));
    let observed = act(
        &mut host,
        json!({"action":"lookup","path":source::PATHS[0],"start":1,"end":14}),
    );
    assert_eq!(observed["status"], "observed");
    assert!(!host.schema().to_string().contains("probe_existing"));
    assert!(!host.schema().to_string().contains("propose_extension"));
    assert_eq!(
        act(&mut host, json!({"action":"probe_existing"}))["status"],
        "refused"
    );
}
