// SPDX-License-Identifier: AGPL-3.0-or-later
//! InferenceFn resolution for awareness subcommands.
//!
//! Three modes — **default is "talk to the daemon"**, the same code
//! path production extraction uses:
//!
//! - **default** — POST prompts to the running daemon's
//!   `/v1/chat/completions` endpoint via `DaemonInferenceClient`. The
//!   daemon must be running (`sovereign daemon run`); the CLI does
//!   not load a parallel embedded model. Chat-model id is taken from
//!   `--model <id>` or auto-selected from the daemon's `/v1/models`
//!   listing (first non-embedding entry).
//!
//! - **`--mock`** — a deterministic stub that returns canned JSON for
//!   each enrichment prompt shape. Kept for unit tests and offline
//!   dry-runs of the seed→extract→digest wiring; **not appropriate
//!   for tuning** because the canned heuristic accidentally matches
//!   the prompt's example payload, producing misleading signal.
//!
//! - **`--dry-run`** — prints prompts to stderr and returns `{}` so
//!   the pipeline tabulates a benign empty response. The developer
//!   uses this to inspect what the production prompt looks like
//!   without spending an inference call.
//!
//! `resolve_inference` is async because the daemon probe is — mock
//! and dry-run paths complete instantly.

use std::sync::Arc;

use corpus_engine::InferenceFn;

use super::args::{get_flag, has_flag};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InferenceMode {
    Real,
    Mock,
    DryRun,
}

/// Decide which inference mode applies given the subcommand's flags.
/// Mutually exclusive — `--mock` wins over `--dry-run` and both win
/// over the default real path; we surface a warning if both are set
/// so the developer doesn't think they're combined.
pub(super) fn pick_mode(flags: &[(String, String)]) -> InferenceMode {
    let mock = has_flag(flags, "mock");
    let dry = has_flag(flags, "dry-run");
    match (mock, dry) {
        (true, true) => {
            eprintln!("awareness: --mock and --dry-run both set; using --mock.");
            InferenceMode::Mock
        }
        (true, false) => InferenceMode::Mock,
        (false, true) => InferenceMode::DryRun,
        (false, false) => InferenceMode::Real,
    }
}

/// Build the InferenceFn for the chosen mode. Real mode loads the
/// embedded model the same way `sovereign chat` does — heavy, but
/// the awareness CLI defers that cost until the user explicitly
/// asks for real extraction quality.
pub(super) async fn resolve_inference(
    flags: &[(String, String)],
) -> Result<(InferenceFn, InferenceMode), String> {
    let mode = pick_mode(flags);
    match mode {
        InferenceMode::Mock => Ok((mock_inference(), mode)),
        InferenceMode::DryRun => Ok((dry_run_inference(), mode)),
        InferenceMode::Real => {
            let f = real_inference(flags).await?;
            Ok((f, mode))
        }
    }
}

/// Canned inference: each call inspects the prompt and returns the
/// JSON shape the corresponding pipeline phase expects. Mirrors the
/// e2e harness's `stub_inference` plus an entity-extraction shape
/// for the personal/conversational domains.
fn mock_inference() -> InferenceFn {
    Arc::new(|prompt: &str, _schema: Option<&serde_json::Value>| {
        let p = prompt.to_string();
        Box::pin(async move {
            // Entity extraction (personal or conversational). The
            // marker phrase is the prompt preamble's claim about
            // named-entity work.
            if p.contains("named-entity extraction") || p.contains("identify the *people*") {
                return Ok(canned_entity_extraction(&p));
            }

            // Suggest-note detection (awareness suggest's focused
            // detection prompt). Marker phrase is the preamble.
            if p.contains("suggest_note pipeline") {
                return Ok(canned_suggest_detection(&p));
            }

            // Below: same shapes the e2e stub returns for older
            // pipeline phases. Kept so `--phase all` runs cleanly.
            if p.contains("semantically similar") || p.contains("cluster together") {
                return Ok(r#"{"topic":"general","position_name":"Default","is_argumentative":false,"is_objection":false,"is_open_question":false,"is_coherent":true}"#.to_string());
            }
            if p.contains("in tension") || p.contains("in dialogue") {
                return Ok(
                    r#"{"crux":"none","confidence":0.5,"resolution_condition":"n/a"}"#.to_string(),
                );
            }
            if p.contains("unresolved inquiry") || p.contains("returning to") {
                return Ok(r#"{"question":"none","why_unresolved":"n/a"}"#.to_string());
            }
            if p.contains("[Memory 1]") || p.contains("[Conversation 1") {
                return Ok(r#"[]"#.to_string());
            }
            Ok("{}".to_string())
        })
    })
}

/// Build a canned entity-extraction response from the prompt's
/// chunk text. Heuristic: walks the `[Memory N]` / `[Conversation N]`
/// blocks and scans for capitalized name candidates. Returns at
/// least an empty-but-valid JSON object.
fn canned_entity_extraction(prompt: &str) -> String {
    use std::collections::BTreeSet;

    let mut persons: BTreeSet<(String, usize)> = BTreeSet::new();
    let mut orgs: BTreeSet<(String, usize)> = BTreeSet::new();
    let mut initiatives: BTreeSet<(String, usize)> = BTreeSet::new();

    let mut current_block: usize = 0;
    let mut current_kind = "Memory";
    for line in prompt.lines() {
        if let Some(rest) = line.strip_prefix("[Memory ") {
            if let Some(idx_str) = rest.split(']').next() {
                if let Ok(n) = idx_str.parse::<usize>() {
                    current_block = n;
                    current_kind = "Memory";
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("[Conversation ") {
            if let Some(idx_str) = rest.split(' ').next() {
                if let Ok(n) = idx_str.parse::<usize>() {
                    current_block = n;
                    current_kind = "Conversation";
                }
            }
            continue;
        }
        if current_block == 0 {
            continue;
        }
        let _ = current_kind;
        scan_line(
            line,
            current_block,
            &mut persons,
            &mut orgs,
            &mut initiatives,
        );
    }

    let person_json: Vec<serde_json::Value> = persons
        .iter()
        .map(|(name, n)| {
            serde_json::json!({
                "name": name,
                "mentions": [format!("Memory {n}")],
            })
        })
        .collect();
    let org_json: Vec<serde_json::Value> = orgs
        .iter()
        .map(|(name, n)| {
            serde_json::json!({
                "name": name,
                "mentions": [format!("Memory {n}")],
            })
        })
        .collect();
    let init_json: Vec<serde_json::Value> = initiatives
        .iter()
        .map(|(name, n)| {
            serde_json::json!({
                "name": name,
                "mentions": [format!("Memory {n}")],
                "participants": [],
            })
        })
        .collect();

    serde_json::json!({
        "persons": person_json,
        "organizations": org_json,
        "initiatives": init_json,
    })
    .to_string()
}

/// Heuristic detector for the awareness suggest prompt. Walks the
/// current-turn marker to isolate the user's text, then looks for
/// canonical commitment / follow_up / goal phrases.
fn canned_suggest_detection(prompt: &str) -> String {
    // Find the "← current turn" line and pull the user content
    // that follows it.
    let current = match prompt.lines().position(|l| l.contains("← current turn")) {
        Some(idx) => {
            let mut buf = String::new();
            for line in prompt.lines().skip(idx + 1) {
                if line.starts_with('[') || line.starts_with("Respond ONLY") {
                    break;
                }
                buf.push_str(line);
                buf.push(' ');
            }
            buf
        }
        None => return r#"{"detections": []}"#.to_string(),
    };
    let lower = current.to_lowercase();
    let mut detections: Vec<serde_json::Value> = Vec::new();

    // Goal: "our goal is", "by Q3 we want", "by end of Qn", measurable
    // targets like "40% enterprise revenue".
    if (lower.contains("our goal is")
        || lower.contains("by end of q")
        || lower.contains("our target is"))
        && (lower.contains('%')
            || lower.contains("revenue")
            || lower.contains("by q")
            || lower.contains("by end of"))
    {
        detections.push(serde_json::json!({
            "kind": "goal",
            "content": short_clause(&current, &["our goal is", "our target is"]),
            "related_entity": null,
            "reasoning": "explicit measurable objective with timeframe",
        }));
    }
    // Commitment: "I'll send …", "I told her I'd …", "by Friday".
    if lower.contains("i'll send")
        || lower.contains("i told her i'd")
        || lower.contains("i told him i'd")
        || lower.contains("by friday")
        || lower.contains("agreed i'd")
    {
        detections.push(serde_json::json!({
            "kind": "commitment",
            "content": short_clause(&current, &["i'll", "i told", "agreed i'd"]),
            "related_entity": null,
            "reasoning": "explicit commitment with deliverable or deadline",
        }));
    }
    // Follow-up: "let's check back on", "remind me to", "circle back".
    if lower.contains("let's check back")
        || lower.contains("circle back")
        || lower.contains("remind me to")
    {
        detections.push(serde_json::json!({
            "kind": "follow_up",
            "content": short_clause(&current, &["let's check back", "circle back", "remind me to"]),
            "related_entity": null,
            "reasoning": "deferred-revisit signal",
        }));
    }

    serde_json::json!({"detections": detections}).to_string()
}

fn short_clause(content: &str, anchors: &[&str]) -> String {
    let lower = content.to_lowercase();
    for a in anchors {
        if let Some(start) = lower.find(a) {
            let tail: String = content[start..].chars().take(120).collect();
            return tail.trim().to_string();
        }
    }
    content
        .chars()
        .take(80)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Tiny entity scanner: capitalized two-word names → Person; words
/// ending in "Corp"/"Inc"/"Co" → Organization; phrases beginning with
/// "API "/"Q3 "/etc → Initiative. Heuristic only — the real signal
/// comes from the model in non-mock mode.
fn scan_line(
    line: &str,
    block: usize,
    persons: &mut std::collections::BTreeSet<(String, usize)>,
    orgs: &mut std::collections::BTreeSet<(String, usize)>,
    initiatives: &mut std::collections::BTreeSet<(String, usize)>,
) {
    let words: Vec<&str> = line.split_whitespace().collect();
    let mut i = 0;
    while i < words.len() {
        let w = words[i].trim_matches(|c: char| !c.is_alphanumeric());
        if w.is_empty() {
            i += 1;
            continue;
        }
        // Initiative: well-known prefixes ("API ", "Q3 ", …) take
        // priority over Person/Org because they don't share shape.
        if (w == "API" || w == "Q3" || w == "Q1" || w == "Q2" || w == "Q4") && i + 1 < words.len() {
            let next = words[i + 1].trim_matches(|c: char| !c.is_alphanumeric());
            if !next.is_empty() && next.chars().all(|c| c.is_alphabetic()) {
                initiatives.insert((format!("{w} {}", next.to_lowercase()), block));
                i += 2;
                continue;
            }
        }
        // Organization vs Person: peek at the next word. If it's a
        // legal-suffix marker (Corp/Inc/Co/Ltd), treat the pair as
        // an organization; otherwise fall through to the Person
        // two-capitalized-words check.
        if first_uppercase(w) && w.chars().all(|c| c.is_alphabetic()) && i + 1 < words.len() {
            let next = words[i + 1].trim_matches(|c: char| !c.is_alphanumeric());
            if matches!(next, "Corp" | "Inc" | "Co" | "Ltd") {
                orgs.insert((format!("{w} {next}"), block));
                i += 2;
                continue;
            }
            if first_uppercase(next)
                && next.chars().all(|c| c.is_alphabetic())
                && !matches!(next, "Corp" | "Inc" | "Co" | "Ltd")
            {
                let candidate = format!("{w} {next}");
                if !is_filler(&candidate) {
                    persons.insert((candidate, block));
                    i += 2;
                    continue;
                }
            }
        }
        i += 1;
    }
}

fn first_uppercase(s: &str) -> bool {
    s.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
}

fn is_filler(name: &str) -> bool {
    matches!(
        name,
        "I'll Send" | "The Q3" | "We Should" | "The Acme" | "The API"
    )
}

fn truncate_for_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let prefix: String = s.chars().take(max).collect();
    format!("{prefix}\n…[truncated, {} chars total]…", s.chars().count())
}

/// Print the prompt + return an empty JSON object. Lets the user
/// visually inspect what the production pipeline would send to the
/// model without spending an inference call.
fn dry_run_inference() -> InferenceFn {
    Arc::new(|prompt: &str, _schema: Option<&serde_json::Value>| {
        let p = prompt.to_string();
        eprintln!("─── awareness --dry-run ───────────────────────────");
        eprintln!("{p}");
        eprintln!("──────────────────────────────────────────────────");
        Box::pin(async move {
            let _ = p;
            // Choose a benign empty response shape that the parser
            // will accept for any extraction phase.
            Ok("{}".to_string())
        })
    })
}

/// Build the InferenceFn for the default (real) path: POST prompts
/// to the running daemon's `/v1/chat/completions`. The daemon is the
/// production inference surface — using it here means awareness
/// extraction exercises the same model + sampler the production
/// pipeline does, with no parallel model load (which would contend
/// for GPU memory). The daemon must be running before the call.
async fn real_inference(flags: &[(String, String)]) -> Result<InferenceFn, String> {
    use crate::enrich_cmd::inference_client::{
        probe_daemon, resolve_default_models, DaemonInferenceClient,
    };
    use crate::util::urls::{v1_url, DEFAULT_CLIENT_PORT};

    let base_url = get_flag(flags, "daemon-url")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            v1_url(DEFAULT_CLIENT_PORT)
                .trim_end_matches("/v1")
                .to_string()
        });

    if !probe_daemon(&base_url).await {
        return Err(format!(
            "daemon not reachable at {base_url}/v1/models — start it with \
             `sovereign daemon run` (or pass --daemon-url <url>); use --mock \
             only for offline wiring checks, not for tuning"
        ));
    }

    let chat_model = match get_flag(flags, "model").filter(|s| !s.is_empty()) {
        Some(m) => m,
        None => {
            let (chat, _embed) = resolve_default_models(&base_url).await;
            chat.ok_or_else(|| {
                format!(
                    "could not auto-select a chat model from {base_url}/v1/models; \
                     pass --model <id> with one of the daemon's registered models"
                )
            })?
        }
    };
    // Embed model id is a no-op for entity extraction (chat-only
    // path), but the client requires a non-empty value.
    let embed_model = "_unused_for_extraction".to_string();

    // Entity-extraction JSON can run several KB; default daemon
    // caps (often 256 tokens on llama.cpp) truncate mid-array.
    let max_output_tokens: u32 = get_flag(flags, "max-tokens")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(4096);

    let client = DaemonInferenceClient::new(base_url.clone(), chat_model.clone(), embed_model)
        .map_err(|e| format!("build daemon client: {e}"))?
        .with_max_output_tokens(max_output_tokens);

    eprintln!(
        "awareness inference: daemon at {base_url}, chat model = {chat_model}, \
         max_tokens = {max_output_tokens}"
    );

    let verbose = has_flag(flags, "verbose");
    let client = Arc::new(client);
    let f: InferenceFn = Arc::new(move |prompt: &str, _schema: Option<&serde_json::Value>| {
        let client = client.clone();
        let p = prompt.to_string();
        Box::pin(async move {
            if verbose {
                eprintln!("─── awareness daemon prompt ───────────────────────");
                eprintln!("{}", truncate_for_display(&p, 800));
                eprintln!("───────────────────────────────────────────────────");
            }
            let chat_prompt = corpus_engine::enrichment::pipeline::ChatPrompt::new("", &p);
            let resp = client
                .complete(&chat_prompt)
                .await
                .map_err(|e| corpus_engine::error::Error::Extraction(e.to_string()))?;
            if verbose {
                eprintln!("─── awareness daemon response ─────────────────────");
                eprintln!("{}", truncate_for_display(&resp, 4000));
                eprintln!("───────────────────────────────────────────────────");
            }
            Ok(resp)
        })
    });
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<(String, String)> {
        v.iter()
            .map(|f| (f.trim_start_matches("--").to_string(), String::new()))
            .collect()
    }

    #[test]
    fn pick_mode_defaults_to_real() {
        assert_eq!(pick_mode(&Vec::new()), InferenceMode::Real);
    }

    #[test]
    fn pick_mode_mock_wins_over_dry_run() {
        let f = s(&["--mock", "--dry-run"]);
        assert_eq!(pick_mode(&f), InferenceMode::Mock);
    }

    #[tokio::test]
    async fn mock_emits_well_formed_entity_extraction_json() {
        let inf = mock_inference();
        let prompt = r#"You are reading memories from one person's long-term record. Your
job is named-entity extraction.

Memories:
[Memory 1]
Had a great call with Sarah Chen at Acme Corp about the Q3 launch."#;
        let out = (inf)(prompt).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let persons = v["persons"].as_array().unwrap();
        let orgs = v["organizations"].as_array().unwrap();
        let inits = v["initiatives"].as_array().unwrap();
        // Heuristic should pick up Sarah Chen, Acme Corp, Q3 launch.
        let person_names: Vec<&str> = persons.iter().filter_map(|p| p["name"].as_str()).collect();
        let org_names: Vec<&str> = orgs.iter().filter_map(|p| p["name"].as_str()).collect();
        let init_names: Vec<&str> = inits.iter().filter_map(|p| p["name"].as_str()).collect();
        assert!(person_names.iter().any(|n| n.contains("Sarah")));
        assert!(org_names.iter().any(|n| n.contains("Acme")));
        assert!(init_names.iter().any(|n| n.starts_with("Q3")));
    }

    #[tokio::test]
    async fn mock_returns_empty_object_for_unknown_prompts() {
        let inf = mock_inference();
        let out = (inf)("a totally unrelated prompt").await.unwrap();
        assert_eq!(out, "{}");
    }
}
