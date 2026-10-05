// SPDX-License-Identifier: AGPL-3.0-or-later
use super::{fixture, oracle};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum AdmissionAction {
    Propose { formatter_package: String },
    Check { candidate: String },
    Accept { candidate: String, receipt: String },
    Stop { reason: String },
}

// No Deserialize: wire output cannot be submitted back as verifier authority.
struct AdmissionReceipt {
    candidate: String,
    contract: String,
    architecture_passed: bool,
    behavior_passed: bool,
    output: Value,
}

pub struct AdmissionController {
    root: PathBuf,
    cargo: PathBuf,
    timeout: Duration,
    contract: String,
    candidates: BTreeMap<String, (String, PathBuf)>,
    receipts: BTreeMap<String, AdmissionReceipt>,
    accepted: Option<Value>,
    stopped: bool,
}

impl AdmissionController {
    pub fn new(root: &Path) -> Result<Self, String> {
        Self::with_runner(root, Path::new("cargo"), Duration::from_secs(30))
    }

    pub fn with_runner(root: &Path, cargo: &Path, timeout: Duration) -> Result<Self, String> {
        fs::create_dir(root).map_err(|e| format!("run root must be new: {e}"))?;
        let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
        for dir in ["candidates", "receipts", "checks"] {
            fs::create_dir(root.join(dir)).map_err(|e| e.to_string())?;
        }
        let checker = fixture::hash(
            &fs::read(std::env::current_exe().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
        );
        let contract = fixture::hash(&serde_json::to_vec(&json!({"task":fixture::hash(fixture::TASK.as_bytes()),"policy":fixture::hash(fixture::POLICY.as_bytes()),"checker":checker,"cargo":cargo,"timeout_ms":timeout.as_millis()})).map_err(|e| e.to_string())?);
        let host = Self {
            root,
            cargo: cargo.into(),
            timeout,
            contract,
            candidates: BTreeMap::new(),
            receipts: BTreeMap::new(),
            accepted: None,
            stopped: false,
        };
        host.write_json("contract.json", &json!({"contract":host.contract,"task":fixture::TASK,"policy":fixture::POLICY,"checker":checker}))?;
        Ok(host)
    }

    pub fn dispatch(&mut self, input: &str) -> Value {
        let result = serde_json::from_str::<AdmissionAction>(input)
            .map_err(|e| format!("invalid action: {e}"))
            .and_then(|a| self.apply(a));
        let output = match result {
            Ok(v) => v,
            Err(reason) => json!({"status":"refused","reason":reason}),
        };
        tracing::debug!(target:"agent_admission", request=input, decision=%output, "admission: transition");
        let event = json!({"request":input,"result":output});
        let recorded = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("events.jsonl"))
            .and_then(|mut f| {
                writeln!(f, "{event}")?;
                f.sync_all()
            });
        if let Err(e) = recorded {
            return json!({"status":"recording-error","reason":e.to_string(),"result":output});
        }
        output
    }

    fn apply(&mut self, action: AdmissionAction) -> Result<Value, String> {
        if self.stopped {
            return Err("run was stopped; no further transitions".into());
        }
        if self.accepted.is_some()
            && !matches!(
                action,
                AdmissionAction::Accept { .. } | AdmissionAction::Stop { .. }
            )
        {
            return Err("task already accepted; only repeat acceptance or stop is legal".into());
        }
        match action {
            AdmissionAction::Propose { formatter_package } => self.propose(&formatter_package),
            AdmissionAction::Check { candidate } => self.check(&candidate),
            AdmissionAction::Accept { candidate, receipt } => self.accept(&candidate, &receipt),
            AdmissionAction::Stop { reason } => {
                if reason.trim().is_empty() {
                    return Err("stop requires a reason".into());
                }
                self.stopped = true;
                Ok(json!({"status":"stopped","reason":reason,"accepted":self.accepted}))
            }
        }
    }

    fn propose(&mut self, formatter: &str) -> Result<Value, String> {
        if self.accepted.is_some() {
            return Err("task already accepted".into());
        }
        if !["none", "ask-format", "model-host", "wire"].contains(&formatter) {
            return Err("unknown formatter package".into());
        }
        let staging =
            tempfile::tempdir_in(self.root.join("candidates")).map_err(|e| e.to_string())?;
        let tree = staging.path().join("snapshot");
        fixture::write(&tree, formatter)?;
        let id = fixture::digest_tree(&tree)?;
        let dest = self.root.join("candidates").join(&id);
        if self.candidates.contains_key(&id) {
            self.validate(&id)?;
            return Err(
                "candidate already proposed; check it or propose a different package".into(),
            );
        } else {
            fs::rename(tree, &dest).map_err(|e| e.to_string())?;
            self.candidates.insert(id.clone(), (formatter.into(), dest));
        }
        Ok(json!({"status":"proposed","candidate":id,"formatter_package":formatter}))
    }

    fn validate(&self, id: &str) -> Result<&Path, String> {
        let (_, path) = self.candidates.get(id).ok_or("unknown candidate")?;
        if fixture::digest_tree(path)? != id {
            return Err("candidate input digest mismatch".into());
        }
        Ok(path)
    }

    fn check(&mut self, id: &str) -> Result<Value, String> {
        let snapshot = self.validate(id)?.to_owned();
        if self
            .receipts
            .values()
            .any(|r| r.candidate == id && r.contract == self.contract)
        {
            return Err(
                "candidate already checked under this contract; use its receipt or revise".into(),
            );
        }
        let work = tempfile::tempdir_in(self.root.join("checks"))
            .map_err(|e| e.to_string())?
            .keep();
        let metadata = oracle::run(
            &self.cargo,
            &[
                "metadata",
                "--offline",
                "--locked",
                "--no-deps",
                "--format-version",
                "1",
            ],
            &snapshot,
            &work,
            "metadata",
            self.timeout,
        )
        .and_then(|(rc, out, err)| {
            if rc == 0 {
                oracle::edges(&out, &snapshot)
            } else {
                Err(format!("metadata exit {rc}: {err}"))
            }
        });
        let mut output = json!({"status":"checked","candidate":id,"contract":self.contract,"logs":work,"edges":[],"architecture":{"verdict":"could-not-judge","reason":"metadata unavailable"},"behavior":{"verdict":"never-ran","reason":"metadata unavailable"}});
        let (architecture_passed, behavior_passed) = match metadata {
            Err(reason) => {
                output["architecture"]["reason"] = json!(reason);
                (false, false)
            }
            Ok(edges) => {
                let map = arch_layers::parse(fixture::POLICY)?;
                let violations = arch_layers::evaluate_packages(&map, &edges);
                let architecture_passed = violations.is_empty();
                output["edges"] = oracle::render_edges(&edges);
                output["architecture"] = json!({"verdict":if architecture_passed {"passed"} else {"failed"},"reason":if architecture_passed {"all fixture edges stay in their package budgets".into()} else {violations.iter().map(|v| v.describe()).collect::<Vec<_>>().join("\n")}});
                let behavior = oracle::run(
                    &self.cargo,
                    &[
                        "test",
                        "--offline",
                        "--locked",
                        "--package",
                        "ask-app",
                        "--lib",
                        "--",
                        "--exact",
                        "task_returns_expected_answer",
                    ],
                    &snapshot,
                    &work,
                    "behavior",
                    self.timeout,
                );
                let passed = match behavior {
                    Err(reason) => {
                        output["behavior"] = json!({"verdict":"could-not-judge","reason":reason});
                        false
                    }
                    Ok((exit, stdout, stderr)) => {
                        let observed = stdout.contains("test task_returns_expected_answer ... ok")
                            && stdout.contains("1 passed; 0 failed");
                        let passed = exit == 0 && observed;
                        output["behavior"] = json!({"verdict":if passed {"passed"} else if exit == 0 {"could-not-judge"} else {"failed"},"reason":if passed {"fixed task assertion ran and passed"} else {"behavior did not establish the fixed task assertion"},"exit":exit,"assertion_observed":observed,"output_digest":fixture::hash(format!("{stdout}\n{stderr}").as_bytes())});
                        passed
                    }
                };
                (architecture_passed, passed)
            }
        };
        self.validate(id)?;
        let receipt_id = fixture::hash(&serde_json::to_vec(&output).map_err(|e| e.to_string())?);
        output["receipt"] = json!(receipt_id);
        self.write_json(&format!("receipts/{receipt_id}.json"), &output)?;
        self.receipts.insert(
            receipt_id,
            AdmissionReceipt {
                candidate: id.into(),
                contract: self.contract.clone(),
                architecture_passed,
                behavior_passed,
                output: output.clone(),
            },
        );
        Ok(output)
    }

    fn accept(&mut self, candidate: &str, receipt_id: &str) -> Result<Value, String> {
        self.validate(candidate)?;
        let receipt = self
            .receipts
            .get(receipt_id)
            .ok_or("receipt was not issued by this controller")?;
        if receipt.candidate != candidate || receipt.contract != self.contract {
            return Err("receipt candidate or governing contract mismatch".into());
        }
        if !receipt.architecture_passed || !receipt.behavior_passed {
            return Err("acceptance requires architecture AND behavior passed".into());
        }
        if let Some(accepted) = &self.accepted {
            if accepted["candidate"] == candidate {
                return Ok(accepted.clone());
            }
            return Err("a different artifact was already accepted".into());
        }
        let accepted = json!({"status":"accepted","candidate":candidate,"receipt":receipt_id,"contract":self.contract,"artifact":self.candidates[candidate].1});
        self.write_json("accepted.json", &accepted)?;
        self.accepted = Some(accepted.clone());
        Ok(accepted)
    }

    fn write_json(&self, name: &str, value: &Value) -> Result<(), String> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.root.join(name))
            .map_err(|e| e.to_string())?;
        f.write_all(value.to_string().as_bytes())
            .and_then(|()| f.sync_all())
            .map_err(|e| e.to_string())
    }

    pub fn schema(&self) -> Value {
        if self.stopped {
            return json!({"not":{}});
        }
        let mut arms = vec![
            json!({"type":"object","properties":{"action":{"const":"stop"},"reason":{"type":"string","minLength":1}},"required":["action","reason"],"additionalProperties":false}),
        ];
        if let Some(accepted) = &self.accepted {
            arms.push(json!({"type":"object","properties":{"action":{"const":"accept"},"candidate":{"const":accepted["candidate"]},"receipt":{"const":accepted["receipt"]}},"required":["action","candidate","receipt"],"additionalProperties":false}));
        }
        if self.accepted.is_none() {
            let novel: Vec<_> = ["ask-format", "model-host", "wire", "none"]
                .into_iter()
                .filter(|p| {
                    !self
                        .candidates
                        .values()
                        .any(|(package, _)| package.as_str() == *p)
                })
                .collect();
            if !novel.is_empty() {
                arms.push(json!({"type":"object","properties":{"action":{"const":"propose"},"formatter_package":{"enum":novel}},"required":["action","formatter_package"],"additionalProperties":false}));
            }
            let unchecked: Vec<_> = self
                .candidates
                .keys()
                .filter(|id| {
                    !self
                        .receipts
                        .values()
                        .any(|r| &r.candidate == *id && r.contract == self.contract)
                })
                .collect();
            if !unchecked.is_empty() {
                arms.push(json!({"type":"object","properties":{"action":{"const":"check"},"candidate":{"enum":unchecked}},"required":["action","candidate"],"additionalProperties":false}));
            }
            for (id, r) in &self.receipts {
                if r.architecture_passed && r.behavior_passed && r.contract == self.contract {
                    arms.push(json!({"type":"object","properties":{"action":{"const":"accept"},"candidate":{"const":r.candidate},"receipt":{"const":id}},"required":["action","candidate","receipt"],"additionalProperties":false}));
                }
            }
        }
        json!({"oneOf":arms})
    }

    pub fn state(&self) -> Value {
        json!({"task":fixture::TASK,"contract":self.contract,"candidates":self.candidates.iter().map(|(id,(package,_))| json!({"candidate":id,"formatter_package":package})).collect::<Vec<_>>(),"receipts":self.receipts.values().map(|r| &r.output).collect::<Vec<_>>(),"accepted":self.accepted,"stopped":self.stopped,"schema":self.schema()})
    }

    pub fn accepted_candidate(&self) -> Option<&str> {
        self.accepted.as_ref().and_then(|v| v["candidate"].as_str())
    }

    #[cfg(test)]
    pub fn candidate_path(&self, id: &str) -> Option<&Path> {
        self.candidates.get(id).map(|(_, path)| path.as_path())
    }
    #[cfg(test)]
    pub fn change_contract_for_test(&mut self) {
        self.contract = "changed".into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_admission_contract_change_invalidates_an_issued_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let mut host = AdmissionController::new(&dir.path().join("run")).unwrap();
        let proposal = host.dispatch(r#"{"action":"propose","formatter_package":"ask-format"}"#);
        let candidate = proposal["candidate"].as_str().unwrap();
        assert!(host.candidate_path(candidate).is_some());
        let receipt = host.dispatch(&json!({"action":"check","candidate":candidate}).to_string());
        assert_eq!(receipt["behavior"]["verdict"], "passed", "{receipt}");
        host.change_contract_for_test();
        let result = host.dispatch(
            &json!({"action":"accept","candidate":candidate,"receipt":receipt["receipt"]})
                .to_string(),
        );
        assert_eq!(result["status"], "refused");
        assert!(result["reason"]
            .as_str()
            .unwrap()
            .contains("contract mismatch"));
    }
}
