// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ring 2: the local model behind [`Evaluator`], over the daemon's
//! OpenAI-shaped API. Three structural levers, all on the wire:
//!
//! * `stable_prefix_len` — the instruction is declared as ONE prefix
//!   family (`prefix_state.rs` directed pin), so after the first frame the
//!   instruction is restored, never re-prefilled.
//! * `lark_grammar` — the reply is a closed set of moves; `push`/`split`
//!   may only name tests from the catalog that are not on the stack, and
//!   `edit` may only name a tracked non-test source file. A cycle, an
//!   invented test, or an edit to the oracle is unsampleable.
//! * `chat_template_kwargs.enable_thinking=false`, `temperature=0` — the
//!   ask is a pure function of the prompt, which the determinism bar reads.
//!
//! `fidelity_probe` is the restore-fidelity measurement (the kill bar): each
//! ask embeds a unique frame id INSIDE the pinned prefix so it is a fresh
//! family, and is sent twice — Learn path (prefill + save, one resident
//! context), then Restore path (state file + suffix). Same bytes both times;
//! the outputs must agree token-for-token.

use super::evaluator::{EvalError, EvalRequest, EvalResponse, Evaluator};
use super::frame::GoalId;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct ModelConfig {
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub timeout: Duration,
    /// Declare the instruction as a stable prefix.
    pub pin: bool,
    /// The restore-fidelity measurement (doubles every ask).
    pub fidelity_probe: bool,
    /// Bytes of source shown under SOURCE, total.
    pub source_budget: usize,
    /// Prefilled at the generation position — the continue register. For
    /// this family it closes the think block the chat template opens
    /// unconditionally (`prompt_helpers.rs` ~L308); without it the grammar
    /// constrains the model's thoughts and every edit body ended in
    /// `</think>`, which broke the import (measured, ring 2).
    pub assistant_prefix: Option<String>,
}

impl ModelConfig {
    pub fn local(model: impl Into<String>) -> Self {
        Self {
            base_url: std::env::var("RECUR_DAEMON")
                .unwrap_or_else(|_| "http://localhost:9741".into()),
            model: model.into(),
            max_tokens: 600,
            timeout: Duration::from_secs(180),
            pin: true,
            fidelity_probe: false,
            source_budget: 6 * 1024,
            assistant_prefix: Some("</think>\n\n".into()),
        }
    }
}

/// One ask's telemetry. The cost table and every ring-2 bar read off these.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskRecord {
    pub depth: usize,
    pub prompt_bytes: usize,
    pub pin_bytes: usize,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub wall_ms: u128,
    pub parsed: bool,
    pub reply: String,
    /// `Some(true)` when the Learn-path and Restore-path outputs agreed.
    pub fidelity: Option<bool>,
}

pub struct ModelEvaluator {
    cfg: ModelConfig,
    client: reqwest::Client,
    catalog: Vec<GoalId>,
    asks: Mutex<Vec<AskRecord>>,
    frame_ids: AtomicU64,
}

#[derive(Serialize)]
struct ChatReq<'a> {
    model: &'a str,
    temperature: f32,
    max_tokens: u32,
    messages: Vec<Msg<'a>>,
    chat_template_kwargs: serde_json::Value,
    lark_grammar: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    stable_prefix_len: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assistant_prefix: Option<&'a str>,
}

#[derive(Serialize)]
struct Msg<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResp {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    message: MsgOwned,
}

#[derive(Deserialize)]
struct MsgOwned {
    content: String,
}

#[derive(Deserialize, Default)]
struct Usage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

impl ModelEvaluator {
    pub fn new(cfg: ModelConfig, catalog: Vec<GoalId>) -> Self {
        Self {
            cfg,
            client: reqwest::Client::new(),
            catalog,
            asks: Mutex::new(Vec::new()),
            frame_ids: AtomicU64::new(0),
        }
    }

    pub fn asks(&self) -> Vec<AskRecord> {
        self.asks.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Every pytest node id plus each test file, from `--collect-only`. The
    /// closed set `push`/`split` draw from.
    pub fn catalog_from_pytest(workdir: &Path) -> std::io::Result<Vec<GoalId>> {
        let out = Command::new("python3")
            .args([
                "-m",
                "pytest",
                "--collect-only",
                "-q",
                "-p",
                "no:cacheprovider",
            ])
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .current_dir(workdir)
            .output()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut set = BTreeSet::new();
        for line in text.lines() {
            let line = line.trim();
            if let Some((file, _)) = line.split_once("::") {
                set.insert(file.to_string());
                set.insert(line.to_string());
            }
        }
        Ok(set.into_iter().map(GoalId::new).collect())
    }

    /// Tracked `.py` files that are not tests: the oracle is never editable.
    fn allowed_files(worktree: &Path) -> Vec<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(["ls-files", "--", "*.py"])
            .output();
        let Ok(out) = out else { return Vec::new() };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("tests/") && !l.ends_with("conftest.py"))
            .map(String::from)
            .collect()
    }

    /// Which sources the frame shows: the goal's own test file, every
    /// tracked file the observation names, then what those import (relative
    /// and package imports, resolved against the tracked set), breadth-first
    /// within the budget. The module under test is one import away from
    /// the failing test, and a frame that cannot see it cannot edit it.
    fn sources_for(&self, req: &EvalRequest, tracked: &[String]) -> Vec<(String, String)> {
        let mut order: Vec<String> = Vec::new();
        let push = |p: &str, order: &mut Vec<String>| {
            if tracked.iter().any(|t| t == p) && !order.iter().any(|o| o == p) {
                order.push(p.to_string());
            }
        };
        let goal_file = req.goal().0.split("::").next().unwrap_or("");
        push(goal_file, &mut order);
        for t in tracked {
            if req.observation.contains(t.as_str()) {
                push(t, &mut order);
            }
        }
        let mut out = Vec::new();
        let mut used = 0usize;
        let mut i = 0;
        while i < order.len() {
            let path = order[i].clone();
            i += 1;
            let Ok(src) = std::fs::read_to_string(req.worktree.join(&path)) else {
                continue;
            };
            for m in Self::imports(&src, &path) {
                push(&m, &mut order);
            }
            if used + src.len() > self.cfg.source_budget {
                continue;
            }
            used += src.len();
            out.push((path, src));
        }
        out
    }

    /// `from calc.g import g` → `calc/g.py`; `from .h import base` inside
    /// `calc/f.py` → `calc/h.py`; `import calc.g` → `calc/g.py`.
    fn imports(src: &str, from_path: &str) -> Vec<String> {
        let dir = from_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let mut out = Vec::new();
        for line in src.lines() {
            let line = line.trim();
            let module = if let Some(rest) = line.strip_prefix("from ") {
                rest.split_whitespace().next()
            } else if let Some(rest) = line.strip_prefix("import ") {
                rest.split(',').next().map(str::trim)
            } else {
                None
            };
            let Some(module) = module else { continue };
            let path = if let Some(rel) = module.strip_prefix('.') {
                format!("{dir}/{}.py", rel.replace('.', "/"))
            } else {
                format!("{}.py", module.replace('.', "/"))
            };
            out.push(path);
        }
        out
    }

    fn lark_literal(s: &str) -> String {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }

    /// The move grammar for this frame. `goals` are the tests not on the
    /// stack (push), `parts` the goal's own descendants among them (split),
    /// `files` the editable sources. An empty set removes its arm rather
    /// than admitting anything: a leaf test cannot split, and a goal with
    /// nothing left to push cannot push.
    pub fn grammar(goals: &[GoalId], parts: &[GoalId], files: &[String]) -> String {
        let mut arms = vec!["give_up"];
        let mut rules = String::new();
        let alts = |gs: &[GoalId]| {
            gs.iter()
                .map(|g| Self::lark_literal(&g.0))
                .collect::<Vec<_>>()
                .join(" | ")
        };
        if !goals.is_empty() {
            arms.push("push");
            rules.push_str(&format!("GOAL: {}\n", alts(goals)));
            rules.push_str("push: \"push \" GOAL\n");
        }
        if parts.len() >= 2 {
            arms.push("split");
            rules.push_str(&format!("PART: {}\n", alts(parts)));
            rules.push_str("split: \"split \" PART (\" \" PART)+\n");
        }
        if !files.is_empty() {
            arms.push("edit");
            let paths = files
                .iter()
                .map(|f| Self::lark_literal(f))
                .collect::<Vec<_>>()
                .join(" | ");
            rules.push_str(&format!("PATH: {paths}\n"));
            rules.push_str("edit: \"edit \" PATH \"\\n\" BODY\n");
            rules.push_str("BODY: /[\\s\\S]+/\n");
        }
        rules.push_str("give_up: \"give_up \" /[^\\n]{1,200}/\n");
        format!("start: {}\n{}", arms.join(" | "), rules)
    }

    /// `child` is a part of `goal`: a node id of a test file, or anything
    /// under a directory.
    pub fn is_part_of(goal: &GoalId, child: &GoalId) -> bool {
        child != goal
            && (child.0.starts_with(&format!("{}::", goal.0))
                || (!goal.0.contains("::") && child.0.starts_with(&format!("{}/", goal.0))))
    }

    /// The prompt, and the byte length of its pinned prefix as the daemon's
    /// flattener sees it (`"User: "` + instruction [+ frame-id line]).
    fn build_prompt(
        &self,
        req: &EvalRequest,
        goals: &[GoalId],
        parts: &[GoalId],
        files: &[String],
        sources: &[(String, String)],
        frame_id: Option<u64>,
    ) -> (String, usize) {
        let mut p = String::from(req.instruction);
        if !p.ends_with('\n') {
            p.push('\n');
        }
        if let Some(id) = frame_id {
            p.push_str(&format!("frame-id: {id}\n"));
        }
        let pin_bytes = "User: ".len() + p.len();
        // The stack is NOT in the prompt: the grammar excludes it, and listing
        // it made the frame O(depth) (measured 4063 → 7312 bytes, ring 2).
        p.push_str("\n## FRAME\n");
        p.push_str(&format!("goal: {}\n", req.goal()));
        p.push_str(&format!("asks_left: {}\n", req.asks_left));
        if let Some(v) = &req.sub_result {
            p.push_str(&format!(
                "sub_result: {} {} ({})\n",
                v.goal,
                v.verdict.as_str(),
                v.reason
            ));
        }
        if let Some(e) = &req.rejected {
            p.push_str(&format!(
                "rejected: your last edit was NOT written, it does not parse:\n{e}\n"
            ));
        }
        if let Some(r) = &req.refused {
            p.push_str(&format!(
                "refused: {r} (already on the stack; do not push it again)\n"
            ));
        }
        p.push_str(
            "
## ALLOWED TESTS (push)
",
        );
        for g in goals {
            p.push_str(&g.0);
            p.push('\n');
        }
        if parts.len() >= 2 {
            p.push_str(
                "
## PARTS OF THIS GOAL (split)
",
            );
            for g in parts {
                p.push_str(&g.0);
                p.push('\n');
            }
        }
        p.push_str("\n## ALLOWED FILES\n");
        for f in files {
            p.push_str(f);
            p.push('\n');
        }
        p.push_str("\n## OBSERVATION\n");
        p.push_str(req.observation.trim_end());
        p.push('\n');
        for (path, src) in sources {
            p.push_str(&format!("\n## SOURCE {path}\n{src}"));
            if !src.ends_with('\n') {
                p.push('\n');
            }
        }
        (p, pin_bytes)
    }

    async fn call(
        &self,
        prompt: &str,
        pin: Option<usize>,
        grammar: &str,
    ) -> Result<(String, Usage), EvalError> {
        let body = ChatReq {
            model: &self.cfg.model,
            temperature: 0.0,
            max_tokens: self.cfg.max_tokens,
            messages: vec![Msg {
                role: "user",
                content: prompt,
            }],
            chat_template_kwargs: serde_json::json!({ "enable_thinking": false }),
            lark_grammar: grammar,
            stable_prefix_len: pin,
            assistant_prefix: self.cfg.assistant_prefix.as_deref(),
        };
        let url = format!("{}/v1/chat/completions", self.cfg.base_url);
        let resp = self
            .client
            .post(&url)
            .timeout(self.cfg.timeout)
            .json(&body)
            .send()
            .await
            .map_err(|e| EvalError::Backend(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| EvalError::Backend(e.to_string()))?;
        if !status.is_success() {
            return Err(EvalError::Backend(format!("{status}: {text}")));
        }
        let parsed: ChatResp =
            serde_json::from_str(&text).map_err(|e| EvalError::Backend(format!("{e}: {text}")))?;
        let content = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| EvalError::Backend("no choices".into()))?;
        Ok((content, parsed.usage.unwrap_or_default()))
    }

    /// The move, from the grammar's wire form. Errors name what did not fit.
    pub fn parse(reply: &str) -> Result<EvalResponse, String> {
        let reply = reply.trim_start();
        let (head, rest) = reply.split_once('\n').unwrap_or((reply, ""));
        let head = head.trim_end();
        if let Some(goal) = head.strip_prefix("push ") {
            return Ok(EvalResponse::Push {
                goal: GoalId::new(goal.trim()),
            });
        }
        if let Some(list) = head.strip_prefix("split ") {
            let mut seen = Vec::new();
            for g in list.split_whitespace() {
                if !seen.iter().any(|s: &GoalId| s.0 == g) {
                    seen.push(GoalId::new(g));
                }
            }
            return match seen.len() {
                0 => Err("split with no goals".into()),
                1 => Ok(EvalResponse::Push {
                    goal: seen.remove(0),
                }),
                _ => Ok(EvalResponse::Split { children: seen }),
            };
        }
        if let Some(reason) = head.strip_prefix("give_up") {
            return Ok(EvalResponse::GiveUp {
                reason: reason.trim().to_string(),
            });
        }
        if let Some(path) = head.strip_prefix("edit ") {
            let mut content = rest.to_string();
            if !content.ends_with('\n') {
                content.push('\n');
            }
            if content.trim().is_empty() {
                return Err(format!("edit {path} with empty body"));
            }
            return Ok(EvalResponse::Edit {
                path: path.trim().to_string(),
                content,
            });
        }
        Err(format!("unrecognised move: {head:?}"))
    }
}

#[async_trait]
impl Evaluator for ModelEvaluator {
    async fn evaluate(&self, req: &EvalRequest) -> Result<EvalResponse, EvalError> {
        let goals: Vec<GoalId> = self
            .catalog
            .iter()
            .filter(|g| !req.on_stack.contains(g))
            .cloned()
            .collect();
        let files = Self::allowed_files(&req.worktree);
        let tracked = {
            let mut all = files.clone();
            let out = Command::new("git")
                .arg("-C")
                .arg(&req.worktree)
                .args(["ls-files", "--", "*.py"])
                .output();
            if let Ok(out) = out {
                all.extend(
                    String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .map(|l| l.trim().to_string()),
                );
            }
            all.sort();
            all.dedup();
            all
        };
        let sources = self.sources_for(req, &tracked);
        let parts: Vec<GoalId> = goals
            .iter()
            .filter(|c| Self::is_part_of(req.goal(), c))
            .cloned()
            .collect();
        let grammar = Self::grammar(&goals, &parts, &files);
        let frame_id = self
            .cfg
            .fidelity_probe
            .then(|| self.frame_ids.fetch_add(1, Ordering::SeqCst));
        let (prompt, pin_bytes) =
            self.build_prompt(req, &goals, &parts, &files, &sources, frame_id);
        let pin = self.cfg.pin.then_some(pin_bytes);

        let t0 = Instant::now();
        let (reply, usage) = self.call(&prompt, pin, &grammar).await?;
        let wall_ms = t0.elapsed().as_millis();
        // Fidelity probe: the first call above was this family's Learn
        // (fresh frame id, first sighting); this one is its Restore.
        let fidelity = if self.cfg.fidelity_probe {
            let (again, _) = self.call(&prompt, pin, &grammar).await?;
            Some(again == reply)
        } else {
            None
        };
        let parsed = Self::parse(&reply);
        tracing::debug!(
            target: "recur",
            goal = %req.goal(),
            depth = req.path.depth(),
            prompt_bytes = prompt.len(),
            prompt_tokens = usage.prompt_tokens,
            completion_tokens = usage.completion_tokens,
            wall_ms,
            reply = %reply.lines().next().unwrap_or(""),
            "recur: model ask"
        );
        self.asks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(AskRecord {
                depth: req.path.depth(),
                prompt_bytes: prompt.len(),
                pin_bytes,
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                wall_ms,
                parsed: parsed.is_ok(),
                reply: reply.clone(),
                fidelity,
            });
        Ok(parsed.unwrap_or_else(|e| EvalResponse::GiveUp {
            reason: format!("unparseable reply: {e}"),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_arms_follow_the_closed_sets() {
        let t = GoalId::new("tests/a.py::t");
        let u = GoalId::new("tests/a.py::u");
        let g = ModelEvaluator::grammar(
            &[t.clone(), u.clone()],
            &[t.clone(), u.clone()],
            &["calc/h.py".into()],
        );
        assert!(
            g.starts_with("start: give_up | push | split | edit\n"),
            "{g}"
        );
        assert!(
            g.contains("GOAL: \"tests/a.py::t\" | \"tests/a.py::u\"\n"),
            "{g}"
        );
        assert!(
            g.contains("PART: \"tests/a.py::t\" | \"tests/a.py::u\"\n"),
            "{g}"
        );
        assert!(g.contains("PATH: \"calc/h.py\"\n"), "{g}");
        // A leaf cannot split; one part is not a split either.
        let leaf = ModelEvaluator::grammar(&[t.clone()], &[], &[]);
        assert!(leaf.starts_with("start: give_up | push\n"), "{leaf}");
        let one = ModelEvaluator::grammar(&[t.clone()], &[t.clone()], &[]);
        assert!(!one.contains("split"), "{one}");
        let none = ModelEvaluator::grammar(&[], &[], &[]);
        assert!(none.starts_with("start: give_up\n"), "{none}");
        let file = GoalId::new("tests/a.py");
        let dir = GoalId::new("tests");
        assert!(ModelEvaluator::is_part_of(&file, &t));
        assert!(ModelEvaluator::is_part_of(&dir, &file));
        assert!(ModelEvaluator::is_part_of(&dir, &t));
        assert!(!ModelEvaluator::is_part_of(&t, &u));
        assert!(!ModelEvaluator::is_part_of(
            &file,
            &GoalId::new("tests/ab.py::t")
        ));
    }

    #[test]
    fn imports_resolve_relative_and_package_forms() {
        let src = "from calc.g import g\nimport calc.h\nfrom .k import k\n";
        assert_eq!(
            ModelEvaluator::imports(src, "calc/f.py"),
            vec!["calc/g.py", "calc/h.py", "calc/k.py"]
        );
        assert_eq!(
            ModelEvaluator::imports("from .h import base\n", "calc/f.py"),
            vec!["calc/h.py"]
        );
    }

    #[test]
    fn parse_reads_every_move_and_refuses_the_rest() {
        assert_eq!(
            ModelEvaluator::parse("push tests/a.py::t\n").unwrap(),
            EvalResponse::Push {
                goal: GoalId::new("tests/a.py::t")
            }
        );
        assert_eq!(
            ModelEvaluator::parse("split tests/a.py::t tests/a.py::u").unwrap(),
            EvalResponse::Split {
                children: vec![GoalId::new("tests/a.py::t"), GoalId::new("tests/a.py::u")]
            }
        );
        assert_eq!(
            ModelEvaluator::parse("split tests/a.py::t tests/a.py::t").unwrap(),
            EvalResponse::Push {
                goal: GoalId::new("tests/a.py::t")
            }
        );
        assert_eq!(
            ModelEvaluator::parse("edit calc/h.py\ndef base(a, b):\n    return a + b").unwrap(),
            EvalResponse::Edit {
                path: "calc/h.py".into(),
                content: "def base(a, b):\n    return a + b\n".into()
            }
        );
        assert_eq!(
            ModelEvaluator::parse("give_up no move").unwrap(),
            EvalResponse::GiveUp {
                reason: "no move".into()
            }
        );
        assert!(ModelEvaluator::parse("verdict: passed").is_err());
        assert!(ModelEvaluator::parse("edit calc/h.py\n\n").is_err());
    }
}
