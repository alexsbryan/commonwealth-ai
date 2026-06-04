//! Single-trial LLM-judge against the local daemon.
//!
//! HTTP client over the OpenAI-compatible `/v1/chat/completions`
//! endpoint. No dep on sovereign-inference / Runtime — keeps the
//! crate's dep graph small and the judge testable with a stubbed
//! HTTP client.
//!
//! The judge is asked to pick a single anchor index 0..=3 for the
//! dimension under review, with a one-line rationale. Output is
//! constrained loosely via prompt + a permissive JSON parser
//! (`parse_judge_response`) that tolerates the model wrapping the
//! JSON in a code fence or prefixing it with prose.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;

use crate::problem::Problem;
use crate::runner::AgentRunArtifact;

/// One judge result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeTrialOutcome {
    pub anchor: u8, // 0..=3
    pub rationale: String,
}

/// Per-dimension judge inputs.
#[derive(Debug, Clone)]
pub struct JudgeRequest {
    pub problem_id: String,
    pub problem_prompt: String,
    pub dimension_name: String,
    pub rubric_anchors: [String; 4],
    /// The agent's final on-disk state, formatted as markdown
    /// `file: <relpath>` blocks. Built by `assemble_workspace_view`.
    pub workspace_view: String,
    pub final_assistant_text: String,
}

#[derive(Debug, Error)]
pub enum JudgeError {
    #[error("judge http request failed: {0}")]
    Http(String),
    #[error("judge returned non-2xx: {status} body={body}")]
    Status { status: u16, body: String },
    #[error("could not parse judge response as anchor JSON: {raw}")]
    Parse { raw: String },
    #[error("judge config error: {0}")]
    Config(String),
}

#[async_trait]
pub trait JudgeClient: Send + Sync {
    async fn judge(&self, req: &JudgeRequest) -> Result<JudgeTrialOutcome, JudgeError>;
}

/// Real HTTP judge — posts to `<base_url>/chat/completions`.
pub struct HttpJudgeClient {
    pub base_url: String,
    pub model: String,
    pub http: reqwest::Client,
    /// Sampling temperature. We default to a low number (0.2) because
    /// the judge should be near-deterministic when asked for an
    /// anchor index; multi-trial aggregation gets variance separately.
    pub temperature: f32,
    pub max_tokens: u32,
}

impl HttpJudgeClient {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(180))
                .build()
                .expect("reqwest client"),
            temperature: 0.2,
            max_tokens: 512,
        }
    }
}

#[async_trait]
impl JudgeClient for HttpJudgeClient {
    async fn judge(&self, req: &JudgeRequest) -> Result<JudgeTrialOutcome, JudgeError> {
        let prompt = build_judge_prompt(req);
        let body = serde_json::json!({
            "model": self.model,
            "temperature": self.temperature,
            "max_tokens": self.max_tokens,
            "messages": [
                {
                    "role": "system",
                    "content": "You are a strict programming-interview judge. Read the rubric anchors, pick exactly one anchor index that best fits the candidate's work, and return JSON of the form {\"anchor\": <0|1|2|3>, \"rationale\": \"<one or two sentences>\"}. Do not include any text outside the JSON object."
                },
                { "role": "user", "content": prompt }
            ]
        });
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .header("authorization", "Bearer dummy")
            .json(&body)
            .send()
            .await
            .map_err(|e| JudgeError::Http(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| JudgeError::Http(e.to_string()))?;
        if !status.is_success() {
            return Err(JudgeError::Status {
                status: status.as_u16(),
                body: truncate(&text, 1024),
            });
        }
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| JudgeError::Http(format!("body parse: {e}")))?;
        let content = parsed
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        parse_judge_response(&content)
    }
}

/// Lenient parser for the judge's reply. Accepts:
///   1. Raw JSON object `{"anchor":2,"rationale":"…"}`
///   2. Fenced ```json …``` block containing such an object
///   3. Any prefix-prose followed by a JSON object as the last
///      braced segment
pub fn parse_judge_response(raw: &str) -> Result<JudgeTrialOutcome, JudgeError> {
    let candidate = extract_json_object(raw).ok_or_else(|| JudgeError::Parse {
        raw: raw.to_string(),
    })?;
    // Judge models frequently embed LaTeX (`$O(n \cdot 2^n)$`) or
    // Windows paths in `rationale`, emitting backslash sequences (`\c`,
    // `\2`) that are not valid JSON escapes — `serde_json` then rejects
    // an otherwise well-formed verdict. Try strict parse first; on
    // failure, repair invalid `\`-escapes and retry. Observed
    // 2026-06-03 (122B judge scoring 0 on 3.2-lights-out-python because
    // the rationale cited `O(n \cdot 2^n)`).
    let v: serde_json::Value = serde_json::from_str(&candidate)
        .or_else(|_| serde_json::from_str(&repair_json_escapes(&candidate)))
        .map_err(|_| JudgeError::Parse {
            raw: raw.to_string(),
        })?;
    let anchor_raw = v
        .get("anchor")
        .and_then(|x| x.as_i64())
        .ok_or_else(|| JudgeError::Parse {
            raw: raw.to_string(),
        })?;
    if !(0..=3).contains(&anchor_raw) {
        return Err(JudgeError::Parse {
            raw: raw.to_string(),
        });
    }
    let rationale = v
        .get("rationale")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    Ok(JudgeTrialOutcome {
        anchor: anchor_raw as u8,
        rationale,
    })
}

/// Repair invalid JSON backslash escapes in a candidate object string so
/// `serde_json` can parse it. For each backslash that is NOT the start
/// of a valid JSON escape (`" \ / b f n r t u`), double it so it becomes
/// a literal backslash in the parsed string. Valid escapes — including
/// `\\` and `\uXXXX` — pass through untouched. This rescues verdicts
/// whose `rationale` contains LaTeX (`\cdot`, `\sum`) or Windows paths
/// (`\Users`), which would otherwise be a hard parse failure scored 0.
/// Applied only as a fallback after a strict parse fails, so well-formed
/// JSON is never altered.
fn repair_json_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') => {
                    // Valid escape: emit the backslash and the escaped
                    // char verbatim so the pair stays intact.
                    out.push('\\');
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
                _ => {
                    // Invalid escape (e.g. `\c` from `\cdot`): double the
                    // backslash so it parses as a literal backslash.
                    out.push('\\');
                    out.push('\\');
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Find the last balanced JSON object substring in `raw`. Lenient over
/// fenced blocks and prefix prose.
fn extract_json_object(raw: &str) -> Option<String> {
    // Strip code fences if present.
    let stripped = strip_code_fence(raw);
    // Scan for the last '{' that opens a balanced object.
    let bytes = stripped.as_bytes();
    let mut best: Option<(usize, usize)> = None;
    let mut stack: Vec<usize> = Vec::new();
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => stack.push(i),
            b'}' => {
                if let Some(start) = stack.pop() {
                    if stack.is_empty() {
                        best = Some((start, i + 1));
                    }
                }
            }
            _ => {}
        }
    }
    best.map(|(a, b)| stripped[a..b].to_string())
}

fn strip_code_fence(raw: &str) -> &str {
    // Look for ```json … ``` and return the inner. Otherwise return raw.
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        if let Some(end) = rest.rfind("```") {
            return &rest[..end];
        }
    }
    if let Some(rest) = trimmed.strip_prefix("```") {
        if let Some(end) = rest.rfind("```") {
            return &rest[..end];
        }
    }
    raw
}

pub fn build_judge_prompt(req: &JudgeRequest) -> String {
    let anchors = req
        .rubric_anchors
        .iter()
        .enumerate()
        .map(|(i, a)| format!("- Anchor {i}: {a}"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "# Problem `{problem_id}` — dimension `{dim}`\n\n\
         ## Task statement (handed to the candidate)\n{problem_prompt}\n\n\
         ## Candidate's final on-disk workspace\n{workspace}\n\n\
         ## Candidate's final assistant message\n{final_text}\n\n\
         ## Rubric anchors (pick exactly one)\n{anchors}\n\n\
         Return ONLY a JSON object on a single line: \
         `{{\"anchor\": <0|1|2|3>, \"rationale\": \"<short justification>\"}}`.",
        problem_id = req.problem_id,
        dim = req.dimension_name,
        problem_prompt = req.problem_prompt,
        workspace = if req.workspace_view.is_empty() {
            "(no files emitted)"
        } else {
            req.workspace_view.as_str()
        },
        final_text = if req.final_assistant_text.is_empty() {
            "(none captured)"
        } else {
            req.final_assistant_text.as_str()
        },
        anchors = anchors,
    )
}

/// Walk the workdir and concatenate every text-looking file into a
/// markdown block. Per-file caps + a global cap keep the prompt
/// bounded.
pub fn assemble_workspace_view(workdir: &std::path::Path) -> String {
    const PER_FILE_CAP: usize = 16 * 1024;
    const GLOBAL_CAP: usize = 128 * 1024;
    let mut sections: BTreeMap<String, String> = BTreeMap::new();
    let mut total: usize = 0;
    let mut stack: Vec<std::path::PathBuf> = vec![workdir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let it = match std::fs::read_dir(&dir) {
            Ok(it) => it,
            Err(_) => continue,
        };
        for entry in it.flatten() {
            let path = entry.path();
            // Skip noise: target/, node_modules/, .git/, hidden, __pycache__/
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.starts_with('.')
                    || name == "target"
                    || name == "node_modules"
                    || name == "__pycache__"
                {
                    continue;
                }
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(workdir)
                .unwrap_or(&path)
                .display()
                .to_string();
            // Heuristic: skip obviously-binary files. We sniff up to
            // 4 KiB and reject if non-UTF8 or contains NUL bytes.
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            if bytes.iter().take(4096).any(|b| *b == 0) {
                continue;
            }
            let mut text = String::from_utf8_lossy(&bytes).into_owned();
            if text.len() > PER_FILE_CAP {
                text.truncate(PER_FILE_CAP);
                text.push_str("\n... (truncated)\n");
            }
            let lang = lang_hint_for(&rel);
            let section = format!("### `{rel}`\n\n```{lang}\n{text}\n```\n");
            total = total.saturating_add(section.len());
            sections.insert(rel, section);
            if total > GLOBAL_CAP {
                debug!(total, GLOBAL_CAP, "agent_bench: workspace_view truncated");
                break;
            }
        }
    }
    sections.into_values().collect::<Vec<_>>().join("\n")
}

fn lang_hint_for(path: &str) -> &'static str {
    if path.ends_with(".rs") {
        "rust"
    } else if path.ends_with(".go") {
        "go"
    } else if path.ends_with(".ts") || path.ends_with(".tsx") {
        "ts"
    } else if path.ends_with(".js") || path.ends_with(".jsx") {
        "js"
    } else if path.ends_with(".py") {
        "python"
    } else if path.ends_with(".toml") {
        "toml"
    } else if path.ends_with(".json") {
        "json"
    } else if path.ends_with(".md") {
        "markdown"
    } else {
        ""
    }
}

/// Build a judge request from a problem + artifact + dimension id.
pub fn request_for_dimension(
    problem: &Problem,
    artifact: &AgentRunArtifact,
    dimension_name: &str,
    rubric_id: &str,
) -> Result<JudgeRequest, JudgeError> {
    let anchors = problem
        .rubric_for(rubric_id)
        .ok_or_else(|| JudgeError::Config(format!("rubric `{rubric_id}` not found in problem")))?;
    let workspace_view = assemble_workspace_view(artifact.workdir.path());
    Ok(JudgeRequest {
        problem_id: problem.meta.id.clone(),
        problem_prompt: problem.prompt_text.clone(),
        dimension_name: dimension_name.to_string(),
        rubric_anchors: anchors.clone(),
        workspace_view,
        final_assistant_text: artifact.final_assistant_text.clone(),
    })
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_judge_response_repairs_latex_backslashes() {
        // The 122B judge embeds LaTeX in `rationale`; `\cdot` is an
        // invalid JSON escape that strict serde rejects. The repair
        // fallback must rescue it (observed 2026-06-03 on
        // 3.2-lights-out-python, dim_b wrongly scored 0).
        let s = r#"{"anchor": 2, "rationale": "O(n \cdot 2^n) is correct"}"#;
        let out = parse_judge_response(s).unwrap();
        assert_eq!(out.anchor, 2);
        assert!(out.rationale.contains("cdot"));
    }

    #[test]
    fn repair_json_escapes_preserves_valid_escapes() {
        // Valid escapes (\n, \", \\) survive the repair untouched.
        let repaired = repair_json_escapes(r#"{"a":"line\nbreak \"q\" end"}"#);
        let v: serde_json::Value = serde_json::from_str(&repaired).unwrap();
        assert_eq!(v.get("a").unwrap().as_str().unwrap(), "line\nbreak \"q\" end");
    }

    #[test]
    fn parse_judge_response_raw_json() {
        let s = r#"{"anchor": 2, "rationale": "right family, suboptimal complexity"}"#;
        let out = parse_judge_response(s).unwrap();
        assert_eq!(out.anchor, 2);
        assert!(out.rationale.contains("right family"));
    }

    #[test]
    fn parse_judge_response_fenced_block() {
        let s = "```json\n{\"anchor\": 3, \"rationale\": \"optimal\"}\n```";
        let out = parse_judge_response(s).unwrap();
        assert_eq!(out.anchor, 3);
    }

    #[test]
    fn parse_judge_response_prefix_prose() {
        let s = "Here is my judgment.\n\n{\"anchor\": 1, \"rationale\": \"weak\"}";
        let out = parse_judge_response(s).unwrap();
        assert_eq!(out.anchor, 1);
    }

    #[test]
    fn parse_judge_response_rejects_out_of_range_anchor() {
        let s = r#"{"anchor": 5, "rationale": "x"}"#;
        let err = parse_judge_response(s).unwrap_err();
        assert!(matches!(err, JudgeError::Parse { .. }));
    }

    #[test]
    fn parse_judge_response_rejects_missing_anchor() {
        let s = r#"{"rationale": "x"}"#;
        let err = parse_judge_response(s).unwrap_err();
        assert!(matches!(err, JudgeError::Parse { .. }));
    }

    #[test]
    fn assemble_workspace_view_skips_target_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("target/debug")).unwrap();
        std::fs::write(tmp.path().join("src.rs"), "pub fn x() {}").unwrap();
        std::fs::write(tmp.path().join("target/debug/blob.bin"), [0u8, 1, 2]).unwrap();
        let view = assemble_workspace_view(tmp.path());
        assert!(view.contains("src.rs"));
        assert!(!view.contains("blob.bin"));
    }

    #[test]
    fn assemble_workspace_view_handles_empty_workdir() {
        let tmp = tempfile::tempdir().unwrap();
        let view = assemble_workspace_view(tmp.path());
        assert!(view.is_empty());
    }

    #[test]
    fn build_judge_prompt_includes_anchors() {
        let req = JudgeRequest {
            problem_id: "1.1".into(),
            problem_prompt: "P".into(),
            dimension_name: "Approach".into(),
            rubric_anchors: ["wrong".into(), "ok".into(), "good".into(), "optimal".into()],
            workspace_view: "WS".into(),
            final_assistant_text: "FT".into(),
        };
        let p = build_judge_prompt(&req);
        assert!(p.contains("Anchor 0: wrong"));
        assert!(p.contains("Anchor 3: optimal"));
        assert!(p.contains("`1.1`"));
        assert!(p.contains("WS"));
    }
}
