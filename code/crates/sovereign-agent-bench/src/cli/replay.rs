// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign agent-bench replay` — re-send a captured chat-completion
//! request with optional overrides. The mechanism that settles
//! "is it the prompt or the model?" debates without rerunning the
//! whole bench.
//!
//! Usage:
//!
//! ```text
//! sovereign agent-bench replay <artifact-dir> --turn N [options]
//! ```
//!
//! `<artifact-dir>` is the per-problem directory written by the run
//! command (e.g. `/tmp/r21fs/2.1-balanced-parens` for a single-trial
//! run, or `/tmp/r21k/2.1-balanced-parens/trial-3` for one trial of a
//! multi-trial run).
//!
//! Overrides land as edits to the captured POST body before re-sending:
//!
//!   --turn N                  (required) 1-based turn index.
//!   --temperature F           override sampling temperature
//!   --top-p F                 override top_p
//!   --max-tokens N            override max_tokens
//!   --tool-choice STR         "auto" | "none" | "forced:NAME"
//!   --no-tools                strip the tools array entirely
//!   --strip-history           drop everything except system + first user
//!                             (useful for "does the model write src/lib.rs
//!                             when context isn't poisoned by prior turns?")
//!   --model HANDLE            send to a different model id
//!   --base-url URL            (default: http://localhost:9741/v1)
//!   --print-original          also print the originally-captured response
//!                             alongside the new one, for diff
//!   --dump-request            print the final request body (after overrides)
//!                             before sending — useful for hand-curling.
//!
//! Exit 0 on success, 1 on error.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};

use crate::runner::ChatRequestRecord;

#[derive(Debug, Default)]
struct ReplayArgs {
    artifact_dir: PathBuf,
    turn: u32,
    temperature: Option<f64>,
    top_p: Option<f64>,
    max_tokens: Option<u64>,
    tool_choice: Option<String>,
    no_tools: bool,
    strip_history: bool,
    model: Option<String>,
    base_url: String,
    print_original: bool,
    dump_request: bool,
}

pub(crate) async fn run_command(args: &[String]) -> Result<(), String> {
    let parsed = parse_args(args)?;
    let record = load_record(&parsed.artifact_dir, parsed.turn)?;
    let mut body = record.request.clone();
    apply_overrides(&mut body, &parsed);
    if parsed.dump_request {
        println!("--- request (after overrides) ---");
        println!(
            "{}",
            serde_json::to_string_pretty(&body).unwrap_or_default()
        );
    }
    let url = format!("{}/chat/completions", parsed.base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let started = Instant::now();
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("read body: {e}"))?;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    if !status.is_success() {
        eprintln!("daemon {status}");
        eprintln!("{}", text.chars().take(2000).collect::<String>());
        return Err(format!("HTTP {status}"));
    }
    let new_resp: Value =
        serde_json::from_str(&text).map_err(|e| format!("parse response: {e}"))?;
    if parsed.print_original {
        println!(
            "--- original response (turn {}, role={}) ---",
            record.turn,
            record.role.as_deref().unwrap_or("-")
        );
        print_response_summary(&record.response);
        println!();
    }
    println!("--- new response ({elapsed_ms}ms) ---");
    print_response_summary(&new_resp);
    println!();
    println!("--- full new response (json) ---");
    println!(
        "{}",
        serde_json::to_string_pretty(&new_resp).unwrap_or_default()
    );
    Ok(())
}

fn parse_args(args: &[String]) -> Result<ReplayArgs, String> {
    let mut out = ReplayArgs::default();
    out.base_url = "http://localhost:9741/v1".to_string();
    let mut i = 0;
    let mut positional: Vec<String> = Vec::new();
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--turn" => {
                i += 1;
                out.turn = args
                    .get(i)
                    .ok_or("--turn requires value")?
                    .parse()
                    .map_err(|e| format!("--turn: {e}"))?;
            }
            "--temperature" => {
                i += 1;
                out.temperature = Some(
                    args.get(i)
                        .ok_or("--temperature requires value")?
                        .parse()
                        .map_err(|e| format!("--temperature: {e}"))?,
                );
            }
            "--top-p" => {
                i += 1;
                out.top_p = Some(
                    args.get(i)
                        .ok_or("--top-p requires value")?
                        .parse()
                        .map_err(|e| format!("--top-p: {e}"))?,
                );
            }
            "--max-tokens" => {
                i += 1;
                out.max_tokens = Some(
                    args.get(i)
                        .ok_or("--max-tokens requires value")?
                        .parse()
                        .map_err(|e| format!("--max-tokens: {e}"))?,
                );
            }
            "--tool-choice" => {
                i += 1;
                out.tool_choice = Some(args.get(i).ok_or("--tool-choice requires value")?.clone());
            }
            "--no-tools" => out.no_tools = true,
            "--strip-history" => out.strip_history = true,
            "--model" => {
                i += 1;
                out.model = Some(args.get(i).ok_or("--model requires value")?.clone());
            }
            "--base-url" => {
                i += 1;
                out.base_url = args.get(i).ok_or("--base-url requires value")?.clone();
            }
            "--print-original" => out.print_original = true,
            "--dump-request" => out.dump_request = true,
            "-h" | "--help" => {
                eprintln!("{}", help_text());
                std::process::exit(0);
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            _ => positional.push(a.clone()),
        }
        i += 1;
    }
    if positional.len() != 1 {
        return Err(format!(
            "expected exactly one positional argument (artifact-dir); got {}\n\n{}",
            positional.len(),
            help_text()
        ));
    }
    out.artifact_dir = PathBuf::from(&positional[0]);
    if out.turn == 0 {
        return Err("--turn is required (1-based)".to_string());
    }
    Ok(out)
}

fn help_text() -> String {
    r#"sovereign agent-bench replay <artifact-dir> --turn N [options]

Re-send a captured chat-completion request from a bench run, optionally with
overrides. <artifact-dir> is the per-problem directory containing
`requests.jsonl` (e.g. /tmp/r/2.1-balanced-parens/trial-3).

Required:
  --turn N                  1-based turn index to replay.

Overrides:
  --temperature F           replace sampling temperature
  --top-p F                 replace top_p
  --max-tokens N            replace max_tokens
  --tool-choice STR         "auto" | "none" | "forced:NAME"
  --no-tools                strip tools array entirely
  --strip-history           drop everything except first system + first user
  --model HANDLE            send to a different model id
  --base-url URL            (default: http://localhost:9741/v1)

Output:
  --print-original          also print the originally-captured response
  --dump-request            print the final request body before sending
"#
    .to_string()
}

fn load_record(artifact_dir: &Path, turn: u32) -> Result<ChatRequestRecord, String> {
    let path = artifact_dir.join("requests.jsonl");
    let body = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rec: ChatRequestRecord =
            serde_json::from_str(line).map_err(|e| format!("parse record: {e}"))?;
        if rec.turn == turn {
            return Ok(rec);
        }
    }
    let turns: Vec<u32> = body
        .lines()
        .filter_map(|l| {
            serde_json::from_str::<ChatRequestRecord>(l)
                .ok()
                .map(|r| r.turn)
        })
        .collect();
    Err(format!(
        "no record with --turn {turn} in {}; available turns: {:?}",
        path.display(),
        turns
    ))
}

fn apply_overrides(body: &mut Value, args: &ReplayArgs) {
    if let Some(t) = args.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = args.top_p {
        body["top_p"] = json!(p);
    }
    if let Some(m) = args.max_tokens {
        body["max_tokens"] = json!(m);
    }
    if let Some(handle) = &args.model {
        body["model"] = json!(handle);
    }
    if args.no_tools {
        body.as_object_mut().map(|o| o.remove("tools"));
        body.as_object_mut().map(|o| o.remove("tool_choice"));
    }
    if let Some(tc) = args.tool_choice.as_deref() {
        match tc {
            "auto" => {
                body["tool_choice"] = json!("auto");
            }
            "none" => {
                body["tool_choice"] = json!("none");
            }
            forced if forced.starts_with("forced:") => {
                let name = &forced["forced:".len()..];
                body["tool_choice"] = json!({
                    "type": "function",
                    "function": {"name": name},
                });
            }
            _ => {
                eprintln!("warn: unknown --tool-choice `{tc}` — leaving original");
            }
        }
    }
    if args.strip_history {
        if let Some(msgs) = body["messages"].as_array() {
            let mut kept: Vec<Value> = Vec::new();
            // Keep first system + first user; drop the rest.
            let mut have_system = false;
            let mut have_user = false;
            for m in msgs {
                let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
                match role {
                    "system" if !have_system => {
                        kept.push(m.clone());
                        have_system = true;
                    }
                    "user" if !have_user => {
                        kept.push(m.clone());
                        have_user = true;
                    }
                    _ => {}
                }
                if have_system && have_user {
                    break;
                }
            }
            body["messages"] = Value::Array(kept);
        }
    }
}

fn print_response_summary(resp: &Value) {
    let choices = resp.get("choices").and_then(|v| v.as_array());
    let msg = choices
        .and_then(|c| c.first())
        .and_then(|c| c.get("message"));
    let content = msg
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_calls = msg
        .and_then(|m| m.get("tool_calls"))
        .and_then(|v| v.as_array());
    let usage = resp.get("usage");
    if let Some(u) = usage {
        let pt = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let ct = u
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        println!("usage: prompt={pt} completion={ct}");
    }
    if !content.is_empty() {
        let trimmed: String = content.chars().take(400).collect();
        println!("content[:400]: {trimmed}");
    }
    if let Some(tc) = tool_calls {
        println!("tool_calls: {}", tc.len());
        for (i, t) in tc.iter().enumerate() {
            let fn_obj = t.get("function");
            let name = fn_obj
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let args_str = fn_obj
                .and_then(|f| f.get("arguments"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let preview: String = args_str.chars().take(200).collect();
            println!("  tc[{i}]: {name} args={preview}");
        }
    } else {
        println!("tool_calls: 0");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_overrides_temperature_and_model() {
        let mut body = json!({
            "model": "old/model",
            "temperature": 0.7,
            "messages": [],
        });
        let args = ReplayArgs {
            artifact_dir: PathBuf::new(),
            turn: 1,
            temperature: Some(0.2),
            model: Some("new/model".into()),
            base_url: "http://x".into(),
            ..Default::default()
        };
        apply_overrides(&mut body, &args);
        assert_eq!(body["temperature"], json!(0.2));
        assert_eq!(body["model"], json!("new/model"));
    }

    #[test]
    fn apply_overrides_strip_history_keeps_first_system_and_user() {
        let mut body = json!({
            "messages": [
                {"role": "system", "content": "S"},
                {"role": "user", "content": "U1"},
                {"role": "assistant", "content": "A1", "tool_calls": []},
                {"role": "tool", "content": "R", "tool_call_id": "x"},
                {"role": "user", "content": "U2"},
            ]
        });
        let args = ReplayArgs {
            artifact_dir: PathBuf::new(),
            turn: 1,
            strip_history: true,
            base_url: "http://x".into(),
            ..Default::default()
        };
        apply_overrides(&mut body, &args);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "U1");
    }

    #[test]
    fn apply_overrides_no_tools_drops_array() {
        let mut body = json!({
            "tools": [{"type": "function", "function": {"name": "x"}}],
            "tool_choice": "auto",
        });
        let args = ReplayArgs {
            artifact_dir: PathBuf::new(),
            turn: 1,
            no_tools: true,
            base_url: "http://x".into(),
            ..Default::default()
        };
        apply_overrides(&mut body, &args);
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn apply_overrides_tool_choice_forced_format() {
        let mut body = json!({});
        let args = ReplayArgs {
            artifact_dir: PathBuf::new(),
            turn: 1,
            tool_choice: Some("forced:write_file".into()),
            base_url: "http://x".into(),
            ..Default::default()
        };
        apply_overrides(&mut body, &args);
        assert_eq!(body["tool_choice"]["type"], "function");
        assert_eq!(body["tool_choice"]["function"]["name"], "write_file");
    }

    #[test]
    fn load_record_picks_matching_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let r1 = ChatRequestRecord {
            turn: 1,
            role: Some("planner".into()),
            request: json!({"model": "x"}),
            response: json!({}),
            elapsed_ms: 100,
        };
        let r2 = ChatRequestRecord {
            turn: 5,
            role: Some("implementer".into()),
            request: json!({"model": "y"}),
            response: json!({}),
            elapsed_ms: 200,
        };
        let mut jsonl = String::new();
        jsonl.push_str(&serde_json::to_string(&r1).unwrap());
        jsonl.push('\n');
        jsonl.push_str(&serde_json::to_string(&r2).unwrap());
        jsonl.push('\n');
        std::fs::write(tmp.path().join("requests.jsonl"), jsonl).unwrap();
        let got = load_record(tmp.path(), 5).unwrap();
        assert_eq!(got.turn, 5);
        assert_eq!(got.role.as_deref(), Some("implementer"));
        assert_eq!(got.request["model"], "y");
    }

    #[test]
    fn load_record_missing_turn_reports_available() {
        let tmp = tempfile::tempdir().unwrap();
        let r1 = ChatRequestRecord {
            turn: 1,
            role: None,
            request: json!({}),
            response: json!({}),
            elapsed_ms: 0,
        };
        std::fs::write(
            tmp.path().join("requests.jsonl"),
            serde_json::to_string(&r1).unwrap() + "\n",
        )
        .unwrap();
        let err = load_record(tmp.path(), 99).unwrap_err();
        assert!(err.contains("99"));
        assert!(err.contains("[1]") || err.contains("1"));
    }
}
