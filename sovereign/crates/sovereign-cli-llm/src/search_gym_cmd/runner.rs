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

use serde::Serialize;
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
    /// Full conversation transcript as a single rendered string
    /// (`User: …\nAssistant: …\n…\nAssistant (final): …`). Used as
    /// the subject for `final_message_satisfies` so the judge can
    /// verify assertions that reference earlier turns ("the user
    /// stated 96 GB earlier"). Empty until run_fixture sets it.
    #[serde(default)]
    pub conversation_view: String,
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
    /// Mock corpus root containing per-tool subdirs (`web/`,
    /// `knowledge/`, `files/`). Each subdir has its own
    /// `aliases.toml` + per-fixture `.json` files in the standard
    /// `{query, results: [{title, url, snippet}]}` shape. The
    /// runner routes a tool call's `name` to the matching subdir:
    ///
    ///   - `search` / `web_search` → `<mock_corpus>/web/`
    ///   - `knowledge`             → `<mock_corpus>/knowledge/`
    ///   - `files`                 → `<mock_corpus>/files/`
    ///
    /// All three tools share the same fixture-replay backend; only
    /// the subdir differs. This lets a fixture build a realistic
    /// production scenario where the model has to choose between
    /// local knowledge, local files, and web search.
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
    if cfg.mode == Mode::Synth {
        // 2g-pending bouncer: the live-backend wiring (read daemon
        // config / construct real Tavily/Brave backend, budget
        // gate, synth-only predicate paths) lands in the next
        // sprint. The flag exists today so callers compile against
        // the final API.
        tx.runner_error = Some(
            "search-gym: --synth mode not yet wired (Phase 2g). Use --mock for now.".to_string(),
        );
        return tx;
    }

    for turn in 0..MAX_TURNS {
        // Grammar-constrained URL emission: each turn's allowlist is
        // the cumulative set of URLs the mocked tools have surfaced
        // so far. Turn 0 ships with an empty list (no URLs known
        // yet — model is making its first call); subsequent turns
        // carry every URL the model has seen, making fabrication of
        // sibling URLs (e.g. `/after-years` next to a real
        // `/after-hours`) structurally impossible.
        if !tx.mock_urls.is_empty() {
            request["url_allowlist"] = Value::Array(
                tx.mock_urls
                    .iter()
                    .map(|u| Value::String(u.clone()))
                    .collect(),
            );
            tracing::info!(
                turn,
                url_count = tx.mock_urls.len(),
                "search-gym: url_allowlist injected"
            );
        }
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

        let message = match resp_json.pointer("/choices/0/message").cloned() {
            Some(m) => m,
            None => {
                tx.runner_error = Some(format!(
                    "daemon response missing choices[0].message turn={turn}"
                ));
                return tx;
            }
        };

        // Capture tool_calls (if any) into the transcript.
        let mut tool_calls = message
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // Fallback: some models emit tool calls as JSON in the
        // content field rather than in the structured `tool_calls`
        // array (chat-template + JSON-mode interaction). The daemon's
        // production canonicalizer only fixes argument shapes, not
        // wire location, so we promote here. Recognises the common
        // `{"name": "<tool>", "parameters": {...}}` shape and rebuilds
        // the proper tool_call entry. Without this, fixtures see
        // `tool_calls=[]` and falsely report "model didn't search".
        if tool_calls.is_empty() {
            if let Some(content) = message.get("content").and_then(|v| v.as_str()) {
                if let Some(promoted) = promote_content_tool_call(content) {
                    tool_calls.push(promoted);
                }
            }
        }

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
            let arguments: Value = serde_json::from_str(raw_args)
                .unwrap_or_else(|_| serde_json::json!({ "__unparseable_args__": raw_args }));
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
            // Snapshot the full conversation so the judge can
            // evaluate assertions that reference earlier turns
            // (e.g. "the user said 96 GB earlier"). Builds from
            // the request's messages array (which includes every
            // turn up through the last assistant-with-tool_calls)
            // plus the final assistant content we just captured.
            tx.conversation_view = render_conversation(&request, &tx.final_message);
            return tx;
        }

        // Execute every tool call the model emitted on this turn.
        // Each known tool name resolves to a mock-corpus subdir:
        //   - search / web_search → mock_corpus/web/
        //   - knowledge           → mock_corpus/knowledge/
        //   - files               → mock_corpus/files/
        // Only `web` URLs accumulate into tx.mock_urls (the URL-set
        // predicates score against the web search result set
        // specifically — citations from knowledge/files are
        // separately auditable through the conversation_view but
        // don't enter the citation-fabrication check today).
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

            let content = match resolve_mock_subdir(&name, &cfg.mock_corpus) {
                Some((subdir, is_web)) => {
                    let backend = SearchBackend::Mock {
                        corpus_path: subdir,
                    };
                    match exec_mock_search(client, &backend, raw_args, cfg.max_search_results).await
                    {
                        Ok((rendered, urls)) => {
                            if is_web {
                                tx.mock_urls.extend(urls);
                            }
                            rendered
                        }
                        Err(e) if e.contains("mock search fixture missing") => {
                            // The fixture author didn't write a mock
                            // for this query phrasing. Two legitimate
                            // reasons: (a) `should_call_search=false`
                            // fixtures don't need mocks because the
                            // model shouldn't be searching at all —
                            // the missing fixture IS the signal of
                            // model misbehavior, not infrastructure
                            // failure; (b) the fixture author missed
                            // a phrasing variant the model emitted.
                            // Either way, returning a synthetic 0-
                            // results response lets the conversation
                            // continue to the synthesis turn so the
                            // structural predicates (`should_call_search`,
                            // `must_cite_url_from_mock`) can fire
                            // cleanly. Bailing with `runner_error`
                            // here pollutes the score with what looks
                            // like an infrastructure problem when the
                            // real signal is in the predicate layer.
                            tracing::warn!(
                                tool = %name,
                                "search-gym: mock fixture missing for query — \
                                 returning synthetic 0 results so the conversation can \
                                 continue. If this fixture has should_call_search=false \
                                 the predicate scorer will flag the violation."
                            );
                            "Search returned 0 results.".to_string()
                        }
                        Err(e) => {
                            tx.runner_error = Some(e);
                            return tx;
                        }
                    }
                }
                None => format!(
                    "error: tool '{name}' is not mocked by the search-gym runner; \
                     this fixture should not depend on it"
                ),
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
        let messages = match request.get_mut("messages").and_then(|v| v.as_array_mut()) {
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

/// Map a model-emitted tool name to its mock-corpus subdir.
/// Returns `(subdir, is_web)`. `is_web=true` for the web-search
/// tools so the URL-set tracking in `mock_urls` only counts
/// citations against the web result set (knowledge/files have
/// URLs too but they're file://, wikipedia URLs, etc — a
/// different namespace than the URL-fabrication predicate is
/// designed to catch).
fn resolve_mock_subdir(
    tool_name: &str,
    mock_corpus_root: &std::path::Path,
) -> Option<(PathBuf, bool)> {
    match tool_name {
        "search" | "web_search" => Some((mock_corpus_root.join("web"), true)),
        "knowledge" => Some((mock_corpus_root.join("knowledge"), false)),
        "files" => Some((mock_corpus_root.join("files"), false)),
        _ => None,
    }
}

/// Recognise an in-content tool call and rebuild it in the
/// OpenAI-structured shape the rest of the runner expects.
///
/// Two shapes observed in practice:
///   1. `{"name":"<tool>","parameters":{...}}`  ← Qwen3 family JSON-mode
///   2. `{"name":"<tool>","arguments":{...}}`   ← native OpenAI naming
///
/// Both get rebuilt as `{"id": "synth_...", "type":"function",
/// "function": {"name":"<tool>","arguments":"<json-stringified>"}}`
/// which matches the wire format the structured `tool_calls` field
/// uses. Returns `None` if no recognisable shape is found.
fn promote_content_tool_call(content: &str) -> Option<Value> {
    // Strip leading `<think>...</think>` blocks and surrounding
    // whitespace — those don't affect parsing but they mean a naive
    // `from_str(content)` won't work.
    let trimmed = strip_think_blocks(content).trim().to_string();
    let parsed: Value = serde_json::from_str(&trimmed).ok()?;
    let name = parsed.get("name")?.as_str()?.to_string();
    if name.is_empty() {
        return None;
    }
    let args_value = parsed
        .get("parameters")
        .or_else(|| parsed.get("arguments"))?;
    let args_string = serde_json::to_string(args_value).ok()?;
    Some(serde_json::json!({
        "id": format!("synth_{}", &name),
        "type": "function",
        "function": {
            "name": name,
            "arguments": args_string,
        }
    }))
}

fn strip_think_blocks(s: &str) -> String {
    // Cheap state machine — handles single or multiple think blocks
    // anywhere in the string. Doesn't try to be clever about nested
    // or malformed tags; the inputs we see are simple.
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        if let Some(end) = rest.find("</think>") {
            rest = &rest[end + "</think>".len()..];
        } else {
            // Unterminated `<think>` block — discard the rest. The
            // alternative (emit the unterminated body) would feed
            // partial thinking content into the JSON parser, which
            // never benefits a tool-call detection.
            return out;
        }
    }
    out.push_str(rest);
    out
}

/// Render the conversation for the judge: every user / assistant
/// turn through the tool-calling phase, followed by the final
/// assistant message. System messages are omitted (they're
/// instructions the model received, not content for the judge to
/// evaluate). Tool-result messages are also omitted — they're
/// implementation detail; the judge cares about what the user
/// said and what the model said back.
fn render_conversation(request: &Value, final_message: &str) -> String {
    let mut out = String::new();
    if let Some(arr) = request.get("messages").and_then(|v| v.as_array()) {
        for msg in arr {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
            match role {
                "user" => {
                    if !content.is_empty() {
                        out.push_str(&format!("User: {content}\n"));
                    }
                }
                "assistant" => {
                    if !content.is_empty() {
                        out.push_str(&format!("Assistant: {content}\n"));
                    }
                }
                _ => {} // system, tool — skip
            }
        }
    }
    out.push_str(&format!("Assistant (final): {final_message}\n"));
    out
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
        .ok_or_else(|| format!("model search call missing 'query' field, args={raw_args}"))?;

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
    // Explicit allowlist trailer. Models routinely fabricate URLs by
    // pattern-extrapolation when they cite (`flight-14-recap` →
    // `flight-8-live`). Pre-committing the allowed-URL set as plain
    // text gives the model an in-context allowlist to draw against,
    // which is much closer to how it processes instructions than
    // hoping it'll reconstruct URLs from the numbered list above.
    // Production search results should render the same trailer when
    // citation-faithfulness matters.
    rendered.push_str(
        "\n--- ALLOWED URLS (use ONLY these verbatim in citations; do not invent or modify) ---\n",
    );
    for u in &urls {
        rendered.push_str(&format!("  {u}\n"));
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
    fn fixture_search_tool_descriptions_match_production_asset() {
        // Propagation discipline (Phase 2 productionization): the
        // gym's fixtures and the production `SearchTool` must use
        // the same model-facing tool description, so a change in
        // the production prompt is automatically exercised by the
        // gym (and vice versa: gym findings that tune the prompt
        // immediately ship to production via the asset file).
        //
        // The asset lives at
        // `sovereign/crates/sovereign-tools/assets/search_tool_description.md`
        // and is exported as `sovereign_tools::search::SEARCH_TOOL_DESCRIPTION`.
        //
        // Fixtures that intentionally diverge (testing a prompt
        // variant) should add their slug to the `INTENTIONAL_FORKS`
        // list below with a comment naming the variant being tested.
        let production_desc = sovereign_tools::search::SEARCH_TOOL_DESCRIPTION.trim();

        const INTENTIONAL_FORKS: &[&str] = &[
            // No forks today. Adding one means committing to maintain
            // a divergent prompt indefinitely — prefer evolving the
            // asset to match what the gym proves works.
        ];

        // Walk every fixture under sovereign/bench/search-gym/fixtures/.
        // Find the path the same way the gym does (workspace root +
        // fixed offset) so tests run from any cwd.
        let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .expect("workspace root resolvable");
        let fixtures_dir = workspace_root.join("sovereign/bench/search-gym/fixtures");

        let mut mismatches: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&fixtures_dir)
            .expect("fixtures dir readable")
            .flatten()
        {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let slug = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if INTENTIONAL_FORKS.iter().any(|f| *f == slug) {
                continue;
            }
            let input_path = path.join("input.json");
            let body = match std::fs::read_to_string(&input_path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let parsed: serde_json::Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let tools = match parsed.get("tools").and_then(|v| v.as_array()) {
                Some(a) => a,
                None => continue,
            };
            for tool in tools {
                let name = tool
                    .pointer("/function/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if name != "search" && name != "web_search" {
                    continue;
                }
                let desc = tool
                    .pointer("/function/description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if desc.trim() != production_desc {
                    mismatches.push(format!(
                        "{slug}: search tool description differs from production asset"
                    ));
                }
            }
        }

        assert!(
            mismatches.is_empty(),
            "Fixture↔production tool-description drift:\n{}\n\n\
             Fix: replace each fixture's `tools[search].function.description` \
             with the contents of \
             sovereign/crates/sovereign-tools/assets/search_tool_description.md \
             (also exported as sovereign_tools::search::SEARCH_TOOL_DESCRIPTION). \
             If the divergence is intentional, add the fixture slug to \
             INTENTIONAL_FORKS in this test with a one-line rationale.",
            mismatches.join("\n")
        );
    }

    #[test]
    fn fixture_system_prompts_match_production_asset() {
        // Same propagation discipline as the tool-description test:
        // the gym's fixture system messages and production-side
        // search-enabled chats must use the same SEARCH_SYSTEM_PROMPT
        // asset. Without this, the gym tunes one surface (tool
        // description) and production runs on a stale system prompt
        // — observed 2026-05-19 Phase 3c iter1, where the asset
        // tightening lifted only the zero-results fixture because
        // the model was anchoring on the old (looser) rules in the
        // system message.
        let production_sys = sovereign_tools::search::SEARCH_SYSTEM_PROMPT.trim();

        const INTENTIONAL_FORKS: &[&str] = &[];

        let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .expect("workspace root resolvable");
        let fixtures_dir = workspace_root.join("sovereign/bench/search-gym/fixtures");

        let mut mismatches: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&fixtures_dir)
            .expect("fixtures dir readable")
            .flatten()
        {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let slug = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if INTENTIONAL_FORKS.iter().any(|f| *f == slug) {
                continue;
            }
            let input_path = path.join("input.json");
            let body = match std::fs::read_to_string(&input_path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let parsed: serde_json::Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let messages = match parsed.get("messages").and_then(|v| v.as_array()) {
                Some(a) => a,
                None => continue,
            };
            // Find the FIRST system message; that's where the
            // search rules live. Fixture-specific user/assistant
            // context lives in later messages and is not gated.
            for msg in messages {
                if msg.get("role").and_then(|v| v.as_str()) != Some("system") {
                    continue;
                }
                let sys = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if sys.trim() != production_sys {
                    mismatches.push(format!(
                        "{slug}: system message differs from SEARCH_SYSTEM_PROMPT"
                    ));
                }
                break;
            }
        }

        assert!(
            mismatches.is_empty(),
            "Fixture↔production system-prompt drift:\n{}\n\n\
             Fix: replace each fixture's first `system` message content with the \
             contents of sovereign/crates/sovereign-tools/assets/search_system_prompt.md \
             (also exported as sovereign_tools::search::SEARCH_SYSTEM_PROMPT).",
            mismatches.join("\n")
        );
    }

    #[test]
    fn resolve_mock_subdir_maps_known_tools() {
        let root = std::path::Path::new("/mock");
        let (web, is_web) = resolve_mock_subdir("search", root).unwrap();
        assert_eq!(web, std::path::PathBuf::from("/mock/web"));
        assert!(is_web);
        let (web2, _) = resolve_mock_subdir("web_search", root).unwrap();
        assert_eq!(web2, std::path::PathBuf::from("/mock/web"));
        let (k, is_w_k) = resolve_mock_subdir("knowledge", root).unwrap();
        assert_eq!(k, std::path::PathBuf::from("/mock/knowledge"));
        assert!(!is_w_k);
        let (f, is_w_f) = resolve_mock_subdir("files", root).unwrap();
        assert_eq!(f, std::path::PathBuf::from("/mock/files"));
        assert!(!is_w_f);
        assert!(resolve_mock_subdir("calendar", root).is_none());
        assert!(resolve_mock_subdir("", root).is_none());
    }

    #[test]
    fn promote_content_tool_call_recognises_parameters_shape() {
        let content = r#"<think></think>

{"name":"search","parameters":{"query":"NVDA current stock price"}}"#;
        let v = promote_content_tool_call(content).expect("should promote");
        assert_eq!(
            v.pointer("/function/name").unwrap().as_str().unwrap(),
            "search"
        );
        let args: Value =
            serde_json::from_str(v.pointer("/function/arguments").unwrap().as_str().unwrap())
                .unwrap();
        assert_eq!(
            args.get("query").unwrap().as_str().unwrap(),
            "NVDA current stock price"
        );
    }

    #[test]
    fn promote_content_tool_call_recognises_arguments_shape() {
        let content = r#"{"name":"search","arguments":{"query":"x"}}"#;
        let v = promote_content_tool_call(content).expect("should promote");
        assert_eq!(
            v.pointer("/function/name").unwrap().as_str().unwrap(),
            "search"
        );
    }

    #[test]
    fn promote_content_tool_call_rejects_non_tool_content() {
        assert!(promote_content_tool_call("Hello, how can I help?").is_none());
        assert!(promote_content_tool_call("{\"unrelated\": true}").is_none());
        assert!(promote_content_tool_call("").is_none());
    }

    #[test]
    fn strip_think_blocks_handles_multiple() {
        assert_eq!(
            strip_think_blocks("<think>a</think>x<think>b</think>y"),
            "xy"
        );
        assert_eq!(strip_think_blocks("plain"), "plain");
        assert_eq!(strip_think_blocks("<think>unclosed and..."), "");
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
