//! Fixture iteration + daemon HTTP + multi-turn tool-call loop.
//!
//! Responsibility split (§3.2): this file *executes* fixtures. It
//! loads each fixture from disk, sends `tools=[...]` to the daemon
//! at `/v1/chat/completions`, observes the model's tool_calls,
//! resolves any `search` invocations through the mock backend, and
//! returns the full transcript to `score.rs`.
//!
//! It does NOT score. Predicate evaluation lives in `score.rs` so
//! the runner can be exercised end-to-end without depending on the
//! predicate vocabulary, and so a future "record only" mode (capture
//! transcripts without judging) is a free side-effect.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use sovereign_tools::web::search::{search as backend_search, SearchBackend};

use super::predicate::Predicate;

/// One fixture loaded from disk. The runner clones the `input` per
/// replay because the conversation history mutates as the model
/// emits tool_calls and we resolve them — each replay starts fresh.
#[derive(Debug, Clone)]
pub struct Fixture {
    pub slug: String,
    pub path: PathBuf,
    pub input: Value,
    pub predicate: Predicate,
}

impl Fixture {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let slug = dir
            .file_name()
            .ok_or_else(|| format!("fixture dir has no name: {}", dir.display()))?
            .to_string_lossy()
            .into_owned();

        let input_path = dir.join("input.json");
        let input_body = std::fs::read_to_string(&input_path)
            .map_err(|e| format!("read {}: {e}", input_path.display()))?;
        let input: Value = serde_json::from_str(&input_body)
            .map_err(|e| format!("parse {}: {e}", input_path.display()))?;

        let pass_path = dir.join("pass.toml");
        let pass_body = std::fs::read_to_string(&pass_path)
            .map_err(|e| format!("read {}: {e}", pass_path.display()))?;
        let predicate = Predicate::from_toml(&pass_body, &pass_path)?;

        Ok(Self {
            slug,
            path: dir.to_path_buf(),
            input,
            predicate,
        })
    }
}

/// Full record of one model interaction. Carries every tool_call the
/// model emitted, the resolved tool results we fed back, and the
/// final assistant message. The scorer reads off this; the JSON
/// report writer serialises it verbatim for post-hoc analysis.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Transcript {
    /// Tool calls observed across all turns, in order.
    pub tool_calls: Vec<ObservedToolCall>,
    /// Final assistant message content (last turn's text).
    pub final_message: String,
    /// All search-result URLs the mock returned across all search
    /// calls. The "must_cite_url_from_mock" predicate scores against
    /// this set.
    pub mock_urls: Vec<String>,
    /// Total wall-clock the model spent generating, summed across
    /// turns. Doesn't include local mock-search resolution time —
    /// that's instant by construction.
    pub model_ms: u128,
    /// Per-fixture failure reason if the runner itself errored
    /// (timeout, malformed daemon response, missing mock fixture for
    /// a query the model genuinely needed). Distinct from predicate
    /// failures, which the scorer reports.
    pub runner_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObservedToolCall {
    pub name: String,
    pub arguments: Value,
    /// Turn index (0 = first model turn). Useful for the
    /// `expected_first_tool` predicate.
    pub turn: usize,
}

/// Maximum number of model turns before the runner gives up. Two
/// turns covers the common shape (model calls search → model
/// synthesises). Three covers a follow-up refine. Beyond that is
/// almost always a loop bug, and the gym should fail loudly.
const MAX_TURNS: usize = 4;

/// Hard ceiling on the daemon HTTP request. Long enough for cold
/// primary-slot loads on commodity hardware; short enough that a
/// hung daemon doesn't stall the whole fixture sweep.
const HTTP_TIMEOUT: Duration = Duration::from_secs(180);

/// Which tool surface the gym exercises. `Mock` is the default and
/// matches Phase 1 behaviour. `Synth` calls the daemon's live
/// search backend (Tavily / Brave / DDG); it's gated behind a
/// budget check and currently bouncers out with a "not yet wired"
/// error — the synth-mode flag is parsed and routed today so that
/// when 2g lands the surface doesn't need restructuring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Mock,
    Synth,
}

impl Mode {
    pub fn id(&self) -> &'static str {
        match self {
            Self::Mock => "mock",
            Self::Synth => "synth",
        }
    }
}

pub struct RunnerCfg {
    pub mode: Mode,
    pub base_url: String,
    pub mock_corpus: PathBuf,
    pub max_search_results: usize,
}

/// Run one fixture end-to-end. The fixture's `input.json` is the
/// initial ChatCompletionRequest — its `messages` get extended in
/// place as the model emits tool_calls and we feed results back.
pub async fn run_fixture(
    client: &reqwest::Client,
    cfg: &RunnerCfg,
    fixture: &Fixture,
) -> Transcript {
    let mut tx = Transcript::default();

    // Clone the input — each replay must start clean.
    let mut request = fixture.input.clone();

    // The gym is non-streaming. Code gym pins this at line 159; we
    // do the same so the response is one JSON object.
    request["stream"] = Value::Bool(false);

    let endpoint = format!("{}/v1/chat/completions", cfg.base_url.trim_end_matches('/'));
    let backend = match cfg.mode {
        Mode::Mock => SearchBackend::Mock {
            corpus_path: cfg.mock_corpus.clone(),
        },
        Mode::Synth => {
            // 2g-pending bouncer: the live-backend wiring (read
            // daemon config / construct real Tavily/Brave backend,
            // budget gate, synth-only predicate paths) lands in the
            // next sprint. The flag exists today so callers compile
            // against the final API.
            tx.runner_error = Some(
                "search-gym: --synth mode not yet wired (Phase 2g). Use --mock for now."
                    .to_string(),
            );
            return tx;
        }
    };

    for turn in 0..MAX_TURNS {
        let started = Instant::now();
        let resp = match client
            .post(&endpoint)
            .json(&request)
            .timeout(HTTP_TIMEOUT)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tx.runner_error = Some(format!("http error turn={turn}: {e}"));
                return tx;
            }
        };

        let status = resp.status();
        let body_text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                tx.runner_error = Some(format!("read body turn={turn}: {e}"));
                return tx;
            }
        };
        tx.model_ms += started.elapsed().as_millis();

        if !status.is_success() {
            tx.runner_error = Some(format!(
                "daemon http {} turn={turn}: {}",
                status.as_u16(),
                body_text.chars().take(400).collect::<String>()
            ));
            return tx;
        }

        let resp_json: Value = match serde_json::from_str(&body_text) {
            Ok(v) => v,
            Err(e) => {
                tx.runner_error = Some(format!("parse daemon response turn={turn}: {e}"));
                return tx;
            }
        };

        let message = match resp_json
            .pointer("/choices/0/message")
            .cloned()
        {
            Some(m) => m,
            None => {
                tx.runner_error =
                    Some(format!("daemon response missing choices[0].message turn={turn}"));
                return tx;
            }
        };

        // Capture tool_calls (if any) into the transcript.
        let tool_calls = message
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        for tc in &tool_calls {
            let name = tc
                .pointer("/function/name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Arguments arrive as a JSON-string-encoded object per
            // OpenAI's wire format. We parse it; if malformed, store
            // the raw string under a synthetic key so the predicate
            // can still see something.
            let raw_args = tc
                .pointer("/function/arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let arguments: Value = serde_json::from_str(raw_args).unwrap_or_else(|_| {
                serde_json::json!({ "__unparseable_args__": raw_args })
            });
            tx.tool_calls.push(ObservedToolCall {
                name,
                arguments,
                turn,
            });
        }

        // No tool calls → this is the final assistant message.
        if tool_calls.is_empty() {
            tx.final_message = message
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            return tx;
        }

        // Execute every tool call the model emitted on this turn.
        // For tools we don't know how to mock (anything other than
        // `search`), return a stub error result so the model can
        // recover — the runner is not the test target for those.
        let mut tool_results: Vec<Value> = Vec::with_capacity(tool_calls.len());
        for tc in &tool_calls {
            let id = tc.get("id").cloned().unwrap_or(Value::Null);
            let name = tc
                .pointer("/function/name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let raw_args = tc
                .pointer("/function/arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");

            let content = if name == "search" || name == "web_search" {
                match exec_mock_search(client, &backend, raw_args, cfg.max_search_results).await {
                    Ok((rendered, urls)) => {
                        tx.mock_urls.extend(urls);
                        rendered
                    }
                    Err(e) => {
                        tx.runner_error = Some(e);
                        return tx;
                    }
                }
            } else {
                format!(
                    "error: tool '{name}' is not mocked by the search-gym runner; \
                     this fixture should not depend on it"
                )
            };

            tool_results.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": id,
                "name": name,
                "content": content,
            }));
        }

        // Extend the conversation: append the assistant turn that
        // emitted the tool_calls, then the tool-result messages.
        // OpenAI wire format requires assistant-with-tool_calls
        // followed by role:tool messages, one per call.
        let messages = match request
            .get_mut("messages")
            .and_then(|v| v.as_array_mut())
        {
            Some(m) => m,
            None => {
                tx.runner_error = Some("input.json has no messages array".into());
                return tx;
            }
        };
        messages.push(message);
        for r in tool_results {
            messages.push(r);
        }
    }

    tx.runner_error = Some(format!(
        "exceeded MAX_TURNS={MAX_TURNS} — model looped on tool calls"
    ));
    tx
}

/// Execute a single mock-search call and render the result list as
/// the tool-result text the model will see on its next turn. Renders
/// as a numbered list of `[N] title — url\n   snippet` blocks so the
/// model can cite numerically. Returns (rendered, urls).
async fn exec_mock_search(
    client: &reqwest::Client,
    backend: &SearchBackend,
    raw_args: &str,
    max_results: usize,
) -> Result<(String, Vec<String>), String> {
    let args: Value = serde_json::from_str(raw_args)
        .map_err(|e| format!("model emitted unparseable search args: {e} raw={raw_args:?}"))?;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!("model search call missing 'query' field, args={raw_args}")
        })?;

    let results = backend_search(client, backend, query, max_results)
        .await
        .map_err(|e| format!("mock backend failure: {e}"))?;

    if results.is_empty() {
        return Ok((
            format!("Search for {query:?} returned 0 results."),
            Vec::new(),
        ));
    }

    let mut rendered = String::with_capacity(results.len() * 200);
    let mut urls = Vec::with_capacity(results.len());
    for (i, r) in results.iter().enumerate() {
        rendered.push_str(&format!(
            "[{n}] {title} — {url}\n   {snippet}\n",
            n = i + 1,
            title = r.title,
            url = r.url,
            snippet = r.snippet
        ));
        urls.push(r.url.clone());
    }
    Ok((rendered, urls))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn fixture_load_reports_missing_files_with_path() {
        let tmp = tempfile::tempdir().unwrap();
        let err = Fixture::load(tmp.path()).unwrap_err();
        assert!(err.contains("input.json"), "err={err}");
    }

    #[test]
    fn fixture_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("01_test");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("input.json"),
            r#"{"model":"primary","messages":[{"role":"user","content":"hello"}]}"#,
        )
        .unwrap();
        std::fs::write(dir.join("pass.toml"), "should_call_search = false\n").unwrap();

        let f = Fixture::load(&dir).unwrap();
        assert_eq!(f.slug, "01_test");
        assert_eq!(f.predicate.should_call_search, Some(false));
    }

    #[test]
    fn transcript_default_is_empty() {
        let tx = Transcript::default();
        assert!(tx.tool_calls.is_empty());
        assert!(tx.runner_error.is_none());
        // Compile-time check that Path is in scope (used by callers).
        let _ = Path::new("/");
    }
}
