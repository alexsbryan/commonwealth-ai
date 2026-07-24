// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn cache-audit` — glassbox telemetry for the fleet's context spend.
//!
//! WHY THIS EXISTS. A fleet agent reported spending ~70% of its budget on
//! "context caching". That number is a thermometer, not a disease: it is
//! *cache-read* cost (re-sending a large context every turn at 0.1x input
//! price), which is already the cheap path. The real driver is context-size x
//! turn-count — and the biggest, most-fixable contributor is agents acquiring
//! codebase understanding by pulling raw source into the harness context
//! (whole-file Reads, `cat`/`grep` via Bash) instead of routing through this
//! stack's own code-intelligence / RAG surface (`symbols`, `callers`,
//! `code_search`, `notes`, …). Every token a Read drops into context is then
//! re-read on every later turn, so the marginal cost of a stray file read is
//! multiplied by the remaining turn count.
//!
//! This command makes that visible per session, straight from the Claude Code
//! transcripts (`~/.claude/projects/<encoded-cwd>/*.jsonl`), which record
//! per-request `cache_read_input_tokens` / `cache_creation_input_tokens` and
//! the full tool-call stream. It reports, per session:
//!   - the cost breakdown (fresh input / cache-read / cache-create / output),
//!   - the peak context size and turn count that drive cache-read cost, and
//!   - the RAW-ACQUISITION RATIO: raw-read tokens pulled into context vs. the
//!     number of code-intelligence / RAG calls made. A session that ran 500
//!     `cat`s and 0 `symbols` calls lights up red.
//!
//! It reads only local transcript files — no daemon, no network, no mutation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Per-model token pricing in USD per million tokens. Sourced from the
/// Anthropic model catalog (Opus 4.8 tier). Cache reads are 0.1x input; 5m
/// cache writes are 1.25x input; 1h cache writes are 2x input.
#[derive(Clone, Copy)]
struct Pricing {
    input: f64,
    output: f64,
    cache_write_5m: f64,
    cache_write_1h: f64,
    cache_read: f64,
    /// True when we could not identify the model and fell back to Opus rates.
    assumed: bool,
}

impl Pricing {
    /// Resolve pricing from a model id string (e.g. `claude-opus-4-8[1m]`).
    /// Matches on the family substring so date/context suffixes don't matter.
    fn for_model(model: &str) -> Pricing {
        let m = model.to_ascii_lowercase();
        // Order matters: check the more specific families first.
        if m.contains("haiku") {
            Pricing { input: 1.0, output: 5.0, cache_write_5m: 1.25, cache_write_1h: 2.0, cache_read: 0.10, assumed: false }
        } else if m.contains("sonnet") {
            Pricing { input: 3.0, output: 15.0, cache_write_5m: 3.75, cache_write_1h: 6.0, cache_read: 0.30, assumed: false }
        } else if m.contains("fable") || m.contains("mythos") {
            Pricing { input: 10.0, output: 50.0, cache_write_5m: 12.5, cache_write_1h: 20.0, cache_read: 1.0, assumed: false }
        } else if m.contains("opus") {
            Pricing { input: 5.0, output: 25.0, cache_write_5m: 6.25, cache_write_1h: 10.0, cache_read: 0.50, assumed: false }
        } else {
            // Unknown model — assume Opus rates but flag it in the output.
            Pricing { input: 5.0, output: 25.0, cache_write_5m: 6.25, cache_write_1h: 10.0, cache_read: 0.50, assumed: true }
        }
    }
}

/// How a tool call is classified for the raw-acquisition analysis.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Bucket {
    /// Read / Glob / Grep — raw file/search acquisition into context.
    RawFileSearch,
    /// Bash — mixed, but `cat`/`grep`/`head`-style commands are raw reads too.
    Bash,
    /// symbols / callers / code_search / notes / … — the distilled path.
    CodeIntel,
    /// Any other `mcp__*` tool.
    OtherMcp,
    /// Edit / Write / MultiEdit.
    EditWrite,
    /// Task / Agent — subagent delegation (its own context, not the parent's).
    Subagent,
    Other,
}

impl Bucket {
    fn label(self) -> &'static str {
        match self {
            Bucket::RawFileSearch => "RAW file/search",
            Bucket::Bash => "Bash (mixed)",
            Bucket::CodeIntel => "CODE-INTEL/RAG",
            Bucket::OtherMcp => "other MCP",
            Bucket::EditWrite => "Edit/Write",
            Bucket::Subagent => "subagent",
            Bucket::Other => "other",
        }
    }
}

/// Names of the code-intelligence / RAG tools this stack exposes (both the
/// modern short names and the deprecated aliases). Matched case-insensitively,
/// and also against the suffix of any `mcp__<server>__<tool>` name.
const CODE_INTEL_TOOLS: &[&str] = &[
    "symbols", "callers", "callees", "code_search", "notes", "note", "blast",
    "project_context", "recent_changes", "drift_findings", "drift_posture",
    "symbol_lookup", "find_callers", "find_callees", "blast_radius", "read_notes",
    "write_note", "work_in_flight", "arch_report", "arch_posture",
];

fn classify(name: &str) -> Bucket {
    match name {
        "Read" | "Glob" | "Grep" => return Bucket::RawFileSearch,
        "Bash" | "BashOutput" => return Bucket::Bash,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => return Bucket::EditWrite,
        "Task" | "Agent" => return Bucket::Subagent,
        _ => {}
    }
    let lower = name.to_ascii_lowercase();
    // `mcp__<server>__<tool>` → compare the trailing tool segment.
    let tool_seg = lower.rsplit("__").next().unwrap_or(lower.as_str());
    if CODE_INTEL_TOOLS.iter().any(|t| *t == lower || *t == tool_seg)
        || lower.contains("sovereign")
    {
        return Bucket::CodeIntel;
    }
    if lower.starts_with("mcp__") {
        return Bucket::OtherMcp;
    }
    Bucket::Other
}

/// A raw-read-shaped Bash command bypasses the Read discipline entirely.
fn is_bash_read_like(cmd: &str) -> bool {
    const NEEDLES: &[&str] = &["cat ", "head ", "tail ", "sed -n", "less ", "grep ", "rg "];
    NEEDLES.iter().any(|n| cmd.contains(n))
}

/// A Bash invocation of the sovereign CLI IS a code-intelligence call — the
/// harness-side MCP surface is not the only door to the brain. Sessions that
/// route acquisition through `sovereign tools call symbols` / `svrn notes` /
/// `session distill` were previously scored "0 code-intel calls" (observed
/// 2026-07-23 on a session with dozens of such calls), making the compliant
/// path indistinguishable from the leak the audit exists to expose.
///
/// Detection is by COMMAND POSITION, not substring: for each `&&`/`;`/`|`
/// segment, skip leading `VAR=…` env assignments and test the first real
/// token. This keeps `cargo build -p sovereign-cli` (an argument) and
/// `rg sovereign` (a pattern) out.
fn is_sovereign_cli(cmd: &str) -> bool {
    cmd.split(|c| c == ';' || c == '|' || c == '&')
        .filter(|seg| !seg.trim().is_empty())
        .any(|seg| {
            let first = seg
                .split_whitespace()
                .find(|tok| !tok.contains('=')); // skip VAR=val prefixes
            match first {
                Some(tok) => {
                    tok == "sovereign"
                        || tok == "svrn"
                        || tok == "sovereign-cli"
                        || tok.ends_with("/sovereign-cli")
                        || tok.ends_with("/sovereign")
                        || tok.ends_with("/svrn")
                }
                None => false,
            }
        })
}

/// Pseudo tool name under which sovereign-CLI Bash calls are tallied, so the
/// per-tool table and the CodeIntel bucket both see them.
const SOVEREIGN_CLI_PSEUDO_TOOL: &str = "sovereign-cli (via Bash)";

/// Approximate token count of a chunk of text (~4 chars/token).
fn approx_tokens(s: &str) -> u64 {
    (s.len() as u64) / 4
}

#[derive(Default)]
struct BucketStat {
    calls: u64,
    ctx_tokens: u64,
}

/// Accumulated analysis for a single session transcript.
struct SessionReport {
    file: String,
    turns: u64,
    max_ctx: u64,
    model: String,
    model_assumed: bool,
    // token sums
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write_5m: u64,
    cache_write_1h: u64,
    // cost sums (USD)
    cost_input: f64,
    cost_output: f64,
    cost_cache_read: f64,
    cost_cache_write: f64,
    // tool mix
    buckets: BTreeMap<Bucket, BucketStat>,
    code_intel_calls: u64,
    raw_acq_tokens: u64,
    bash_read_like: u64,
    top_tools: Vec<(String, u64, u64)>, // (name, calls, ctx_tokens)
    mtime_unix: i64,
}

impl SessionReport {
    fn total_cost(&self) -> f64 {
        self.cost_input + self.cost_output + self.cost_cache_read + self.cost_cache_write
    }
    /// Dollars this session WOULD have cost if the cache-read tokens had been
    /// billed as fresh input (i.e. what caching saved).
    fn counterfactual_no_cache_read(&self) -> f64 {
        let p = Pricing::for_model(&self.model);
        (self.cache_read as f64) / 1_000_000.0 * p.input
    }
}

/// Extract concatenated text from a tool_result `content` value (string or
/// list of blocks) for token estimation.
fn result_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => arr
            .iter()
            .map(|b| {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                    t.to_string()
                } else if let Some(inner) = b.get("content") {
                    result_text(inner)
                } else {
                    String::new()
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Object(_) => v
            .get("content")
            .map(result_text)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Parse and analyze one transcript file. Returns None if it carries no usage.
fn analyze(path: &Path) -> Option<SessionReport> {
    let text = std::fs::read_to_string(path).ok()?;
    let mtime_unix = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut turns = 0u64;
    let mut max_ctx = 0u64;
    let (mut input, mut output, mut cache_read) = (0u64, 0u64, 0u64);
    let (mut cw5m, mut cw1h) = (0u64, 0u64);
    let (mut cost_input, mut cost_output, mut cost_cr, mut cost_cw) = (0.0, 0.0, 0.0, 0.0);
    let mut model_counts: BTreeMap<String, u64> = BTreeMap::new();

    let mut buckets: BTreeMap<Bucket, BucketStat> = BTreeMap::new();
    let mut per_tool: BTreeMap<String, (u64, u64)> = BTreeMap::new(); // name -> (calls, tokens)
    let mut tool_id_name: BTreeMap<String, String> = BTreeMap::new();
    let mut code_intel_calls = 0u64;
    let mut raw_acq_tokens = 0u64;
    let mut bash_read_like = 0u64;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let obj: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let msg = match obj.get("message") {
            Some(m) if m.is_object() => m,
            _ => continue,
        };

        // ---- usage / cost ----
        if let Some(usage) = msg.get("usage").filter(|u| u.is_object()) {
            let a = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let o = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let cr = usage.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let cw = usage.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            // Precise 5m/1h split when the transcript carries it.
            let (this_5m, this_1h) = match usage.get("cache_creation").filter(|c| c.is_object()) {
                Some(c) => (
                    c.get("ephemeral_5m_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                    c.get("ephemeral_1h_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                ),
                None => (cw, 0), // unknown TTL -> price as 5m (the cheaper write)
            };

            turns += 1;
            input += a;
            output += o;
            cache_read += cr;
            cw5m += this_5m;
            cw1h += this_1h;
            max_ctx = max_ctx.max(a + cr + cw);

            let model = msg.get("model").and_then(|m| m.as_str()).unwrap_or("");
            if !model.is_empty() {
                *model_counts.entry(model.to_string()).or_default() += 1;
            }
            let p = Pricing::for_model(model);
            cost_input += (a as f64) / 1_000_000.0 * p.input;
            cost_output += (o as f64) / 1_000_000.0 * p.output;
            cost_cr += (cr as f64) / 1_000_000.0 * p.cache_read;
            cost_cw += (this_5m as f64) / 1_000_000.0 * p.cache_write_5m
                + (this_1h as f64) / 1_000_000.0 * p.cache_write_1h;
        }

        // ---- tool use / results ----
        if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
            for block in content {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("tool_use") => {
                        let mut name = block.get("name").and_then(|n| n.as_str()).unwrap_or("?").to_string();
                        if name == "Bash" {
                            if let Some(cmd) = block.get("input").and_then(|i| i.get("command")).and_then(|c| c.as_str()) {
                                if is_sovereign_cli(cmd) {
                                    // CLI-path brain calls are code-intel, and
                                    // their result tokens must route there too
                                    // (via the id→name map below) — never into
                                    // the raw-acquisition tally.
                                    name = SOVEREIGN_CLI_PSEUDO_TOOL.to_string();
                                } else if is_bash_read_like(cmd) {
                                    bash_read_like += 1;
                                }
                            }
                        }
                        if let Some(id) = block.get("id").and_then(|i| i.as_str()) {
                            tool_id_name.insert(id.to_string(), name.clone());
                        }
                        per_tool.entry(name.clone()).or_default().0 += 1;
                        let bucket = classify(&name);
                        buckets.entry(bucket).or_default().calls += 1;
                        if bucket == Bucket::CodeIntel {
                            code_intel_calls += 1;
                        }
                    }
                    Some("tool_result") => {
                        let tid = block.get("tool_use_id").and_then(|t| t.as_str()).unwrap_or("");
                        let name = tool_id_name.get(tid).cloned().unwrap_or_else(|| "?".to_string());
                        let toks = block
                            .get("content")
                            .map(result_text)
                            .map(|s| approx_tokens(&s))
                            .unwrap_or(0);
                        per_tool.entry(name.clone()).or_default().1 += toks;
                        let bucket = classify(&name);
                        buckets.entry(bucket).or_default().ctx_tokens += toks;
                        if matches!(bucket, Bucket::RawFileSearch | Bucket::Bash) {
                            raw_acq_tokens += toks;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if turns == 0 {
        return None;
    }

    let (model, model_assumed) = model_counts
        .iter()
        .max_by_key(|(_, c)| **c)
        .map(|(m, _)| (m.clone(), Pricing::for_model(m).assumed))
        .unwrap_or_else(|| ("unknown".to_string(), true));

    let mut top_tools: Vec<(String, u64, u64)> = per_tool
        .into_iter()
        .map(|(name, (calls, toks))| (name, calls, toks))
        .collect();
    top_tools.sort_by(|a, b| b.2.cmp(&a.2));
    top_tools.truncate(8);

    Some(SessionReport {
        file: path.file_name().and_then(|f| f.to_str()).unwrap_or("").to_string(),
        turns,
        max_ctx,
        model,
        model_assumed,
        input,
        output,
        cache_read,
        cache_write_5m: cw5m,
        cache_write_1h: cw1h,
        cost_input,
        cost_output,
        cost_cache_read: cost_cr,
        cost_cache_write: cost_cw,
        buckets,
        code_intel_calls,
        raw_acq_tokens,
        bash_read_like,
        top_tools,
        mtime_unix,
    })
}

/// Encode a filesystem path the way Claude Code names its transcript dir:
/// every character that is not `[A-Za-z0-9-]` becomes `-`.
fn encode_project_path(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect()
}

/// Shared with `session_cmd` — both read the same transcript layout.
pub(crate) fn short_session_id(file: &str) -> String {
    let stem = file.strip_suffix(".jsonl").unwrap_or(file);
    stem.chars().take(8).collect()
}

// ── Ramp-up cost ─────────────────────────────────────────────────────────
//
// What a session spent getting oriented before its first productive action
// (first Edit/Write): acquisition tokens by kind, plus repeated file Reads
// (the "re-familiarizing with the monorepo" tax). This is the split-safety
// gauge: a successor session that boots from a good frame should ramp on
// a few k tokens (measured benchmark: 2.3k for the hand-written-handoff
// FactStore session); a cold session burns 15–56k (fleet measurements,
// 2026-07-23). Upper bound by construction — pre-implementation research
// on a genuinely new task also lands in the window.

struct RampReport {
    file: String,
    requests: u64,
    first_edit_req: Option<u64>,
    raw_tokens: u64,
    raw_calls: u64,
    intel_tokens: u64,
    intel_calls: u64,
    /// Read calls during ramp whose file_path was already Read in ramp.
    repeat_reads: u64,
}

fn analyze_ramp(path: &Path) -> Option<RampReport> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut requests = 0u64;
    let mut first_edit_req: Option<u64> = None;
    let (mut raw_tokens, mut raw_calls) = (0u64, 0u64);
    let (mut intel_tokens, mut intel_calls) = (0u64, 0u64);
    let mut repeat_reads = 0u64;
    let mut read_paths: std::collections::HashSet<String> = Default::default();
    // tool_use_id -> true=raw, false=intel (ramp window only)
    let mut pending: BTreeMap<String, bool> = BTreeMap::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let obj: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let msg = match obj.get("message") {
            Some(m) if m.is_object() => m,
            _ => continue,
        };
        if msg.get("usage").filter(|u| u.is_object()).is_some() {
            requests += 1;
        }
        let Some(content) = msg.get("content").and_then(|c| c.as_array()) else {
            continue;
        };
        for block in content {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("tool_use") => {
                    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                    if matches!(name, "Edit" | "Write" | "NotebookEdit") && first_edit_req.is_none()
                    {
                        first_edit_req = Some(requests);
                    }
                    if first_edit_req.is_some() {
                        continue;
                    }
                    if name == "Read" {
                        if let Some(fp) = block
                            .get("input")
                            .and_then(|i| i.get("file_path"))
                            .and_then(|f| f.as_str())
                        {
                            if !read_paths.insert(fp.to_string()) {
                                repeat_reads += 1;
                            }
                        }
                    }
                    let kind = if name == "Bash" {
                        block
                            .get("input")
                            .and_then(|i| i.get("command"))
                            .and_then(|c| c.as_str())
                            .and_then(|cmd| {
                                if is_sovereign_cli(cmd) {
                                    Some(false)
                                } else if is_bash_read_like(cmd) {
                                    Some(true)
                                } else {
                                    None
                                }
                            })
                    } else if classify(name) == Bucket::RawFileSearch {
                        Some(true)
                    } else if classify(name) == Bucket::CodeIntel {
                        Some(false)
                    } else {
                        None
                    };
                    if let (Some(raw), Some(id)) =
                        (kind, block.get("id").and_then(|i| i.as_str()))
                    {
                        pending.insert(id.to_string(), raw);
                    }
                }
                Some("tool_result") if first_edit_req.is_none() => {
                    let tid = block.get("tool_use_id").and_then(|t| t.as_str()).unwrap_or("");
                    if let Some(raw) = pending.remove(tid) {
                        let toks = block
                            .get("content")
                            .map(result_text)
                            .map(|s| approx_tokens(&s))
                            .unwrap_or(0);
                        if raw {
                            raw_tokens += toks;
                            raw_calls += 1;
                        } else {
                            intel_tokens += toks;
                            intel_calls += 1;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if requests == 0 {
        return None;
    }
    Some(RampReport {
        file: path.file_name().and_then(|f| f.to_str()).unwrap_or("").to_string(),
        requests,
        first_edit_req,
        raw_tokens,
        raw_calls,
        intel_tokens,
        intel_calls,
        repeat_reads,
    })
}

fn print_ramp(reports: &[RampReport]) {
    println!(
        "{:<10} {:>6} {:>11} {:>10} {:>11} {:>10} {:>7}",
        "session", "reqs", "1stEdit@req", "ramp raw", "ramp intel", "calls r:i", "repeats"
    );
    println!("{}", "-".repeat(72));
    for r in reports {
        let fe = r
            .first_edit_req
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<10} {:>6} {:>11} {:>9}t {:>10}t {:>7}:{:<3} {:>5}",
            short_session_id(&r.file),
            r.requests,
            fe,
            r.raw_tokens,
            r.intel_tokens,
            r.raw_calls,
            r.intel_calls,
            r.repeat_reads
        );
    }
    println!(
        "\nramp = acquisition before the first Edit/Write (upper bound: includes genuine\n\
         pre-implementation research). Split-safety gate: a frame-booted successor should\n\
         ramp ≤5k tokens with 0 repeats; measured cold baseline 15–56k; hand-written\n\
         handoff benchmark 2.3k."
    );
}

// ── Counterfactual replay ────────────────────────────────────────────────
//
// Re-prices existing transcripts under four independent cost levers so the
// "what should the suit build next?" question is answered by arithmetic on
// sessions already paid for, not by intuition. Pure replay: no LLM, no
// network — the same JSONL walk `analyze` does, but keeping a per-request
// timeline instead of aggregates, because a token's true cost is
// (requests remaining after it entered) × cache-read price.
//
// The levers are priced INDEPENDENTLY — they overlap (splitting also
// evicts raw-read tokens), so the numbers must not be summed.

/// One API request on the session timeline.
struct ReqPoint {
    /// Total context this request carried (input + cache_read + cache_write).
    ctx: u64,
    /// Tokens actually billed as cache-read on this request.
    cache_read: u64,
    /// Cache-read $/MTok for this request's model.
    price_cr: f64,
}

/// Tokens that entered context at a known timeline position.
struct TimedTokens {
    /// Index of the NEXT request (the first one that re-transmits them).
    req_index: usize,
    tokens: u64,
}

struct CfReport {
    file: String,
    actual_total_cost: f64,
    actual_cr_cost: f64,
    preamble0: u64,
    n_requests: usize,
    /// (threshold, splits, net $ saved)
    splits: Vec<(u64, u64, f64)>,
    halve_preamble_saved: f64,
    injection_tokens: u64,
    cap_injections_saved: f64,
    batch_upper_bound_saved: f64,
    raw_tokens: u64,
    route_raw_saved: f64,
}

/// Markers whose presence in a user-record text block identifies content
/// injected by the suit's own hooks rather than typed by the human.
const INJECTION_MARKERS: &[&str] = &[
    "hook success",
    "## Sovereign notes",
    "<system-reminder>",
    "# Project context:",
    "session-frame/v1",
];

/// Fixed context a successor session starts with after a split, beyond the
/// re-loaded preamble: frame (~2k) + boot brief (~1.5k) + re-acquisition
/// slop (~2.5k). Deliberately conservative.
const SPLIT_SEED_EXTRA: u64 = 6_000;
/// Per-prompt injection budget for the "cap injections" counterfactual.
const INJECTION_CAP: u64 = 500;
/// A request that grew context by less than this is a "small turn" —
/// candidate for batching with its neighbours.
const SMALL_TURN_GROWTH: u64 = 1_500;
/// Code-intel answers the same question in ~1/5 the tokens of a raw read
/// (observed: symbols/callers responses vs whole-file Reads).
const INTEL_COMPRESSION: u64 = 5;

fn analyze_counterfactual(path: &Path) -> Option<CfReport> {
    let text = std::fs::read_to_string(path).ok()?;

    let mut points: Vec<ReqPoint> = Vec::new();
    let mut injections: Vec<TimedTokens> = Vec::new();
    let mut raw_acq: Vec<TimedTokens> = Vec::new();
    let mut tool_id_raw: BTreeMap<String, bool> = BTreeMap::new();
    let (mut actual_total, mut actual_cr_cost) = (0.0_f64, 0.0_f64);
    let mut model_counts: BTreeMap<String, u64> = BTreeMap::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let obj: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let msg = match obj.get("message") {
            Some(m) if m.is_object() => m,
            _ => continue,
        };

        if let Some(usage) = msg.get("usage").filter(|u| u.is_object()) {
            let a = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let o = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let cr = usage.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let cw = usage.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let model = msg.get("model").and_then(|m| m.as_str()).unwrap_or("");
            if !model.is_empty() {
                *model_counts.entry(model.to_string()).or_default() += 1;
            }
            let p = Pricing::for_model(model);
            actual_total += (a as f64) / 1e6 * p.input
                + (o as f64) / 1e6 * p.output
                + (cr as f64) / 1e6 * p.cache_read
                + (cw as f64) / 1e6 * p.cache_write_5m;
            actual_cr_cost += (cr as f64) / 1e6 * p.cache_read;
            points.push(ReqPoint { ctx: a + cr + cw, cache_read: cr, price_cr: p.cache_read });
            // NO `continue` — assistant records carry BOTH usage and the
            // tool_use blocks; skipping their content scan would leave the
            // id→raw map empty and silently zero H4 (the first fleet run
            // did exactly that: "0k raw" against sessions the plain audit
            // attributes 66k–284k raw tokens to).
        }

        // Tool results and user turns attribute to the NEXT request index
        // (they enter context there first).
        let req_index = points.len();
        if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
            for block in content {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("tool_use") => {
                        let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                        let raw = match name {
                            "Bash" => block
                                .get("input")
                                .and_then(|i| i.get("command"))
                                .and_then(|c| c.as_str())
                                .map(|c| !is_sovereign_cli(c) && is_bash_read_like(c))
                                .unwrap_or(false),
                            _ => classify(name) == Bucket::RawFileSearch,
                        };
                        if let Some(id) = block.get("id").and_then(|i| i.as_str()) {
                            tool_id_raw.insert(id.to_string(), raw);
                        }
                    }
                    Some("tool_result") => {
                        let tid = block.get("tool_use_id").and_then(|t| t.as_str()).unwrap_or("");
                        if tool_id_raw.get(tid).copied().unwrap_or(false) {
                            let toks = block
                                .get("content")
                                .map(result_text)
                                .map(|s| approx_tokens(&s))
                                .unwrap_or(0);
                            if toks > 0 {
                                raw_acq.push(TimedTokens { req_index, tokens: toks });
                            }
                        }
                    }
                    Some("text") => {
                        let t = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        if INJECTION_MARKERS.iter().any(|m| t.contains(m)) {
                            injections.push(TimedTokens { req_index, tokens: approx_tokens(t) });
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if points.is_empty() {
        return None;
    }
    let n = points.len();
    let preamble0 = points[0].ctx;
    let dominant_price = model_counts
        .iter()
        .max_by_key(|(_, c)| **c)
        .map(|(m, _)| Pricing::for_model(m))
        .unwrap_or_else(|| Pricing::for_model(""));
    let p_dom = dominant_price.cache_read;

    // H1 — split at threshold K. A split evicts (ctx − seed) tokens from
    // every subsequent request until the next split; each split re-writes
    // the seed once (priced as a 1h cache write).
    let seed = preamble0 + SPLIT_SEED_EXTRA;
    let mut splits_out = Vec::new();
    for k in [100_000u64, 140_000, 200_000] {
        let mut offset = 0u64;
        let mut splits = 0u64;
        let mut saved = 0.0_f64;
        for pt in &points {
            let mut eff = pt.ctx.saturating_sub(offset);
            if eff > k && pt.ctx > seed {
                splits += 1;
                offset = pt.ctx - seed;
                eff = seed;
            }
            let evicted = pt.ctx - eff; // == offset, clamped by saturating math
            let saved_cr = evicted.min(pt.cache_read);
            saved += (saved_cr as f64) / 1e6 * pt.price_cr;
        }
        let split_cost = (splits as f64) * (seed as f64) / 1e6 * dominant_price.cache_write_1h;
        splits_out.push((k, splits, saved - split_cost));
    }

    // H2a — halve the turn-0 preamble (system prompt + CLAUDE.md + boot
    // injections). It rides every request's cache-read after the first.
    let halve_preamble_saved: f64 = points
        .iter()
        .skip(1)
        .map(|pt| ((preamble0 / 2).min(pt.cache_read) as f64) / 1e6 * pt.price_cr)
        .sum();

    // H2b — cap each per-prompt hook injection at INJECTION_CAP tokens.
    let injection_tokens: u64 = injections.iter().map(|i| i.tokens).sum();
    let cap_injections_saved: f64 = injections
        .iter()
        .map(|i| {
            let excess = i.tokens.saturating_sub(INJECTION_CAP);
            (excess as f64) * ((n.saturating_sub(i.req_index)) as f64) / 1e6 * p_dom
        })
        .sum();

    // H3 — batch runs of ≥3 consecutive small-growth requests 3:1. Each
    // removed request saves its entire cache-read line. UPPER BOUND: replay
    // cannot prove the calls were independent enough to batch.
    let mut batch_saved = 0.0_f64;
    {
        let mut close_run = |start: usize, end_incl: usize| {
            let run_len = end_incl + 1 - start;
            if run_len >= 3 {
                let removable = run_len - run_len.div_ceil(3);
                for pt in points[start..=end_incl].iter().rev().take(removable) {
                    batch_saved += (pt.cache_read as f64) / 1e6 * pt.price_cr;
                }
            }
        };
        let mut run_start = 0usize;
        for i in 0..n - 1 {
            let growth = points[i + 1].ctx.saturating_sub(points[i].ctx);
            if growth >= SMALL_TURN_GROWTH {
                close_run(run_start, i);
                run_start = i + 1;
            }
        }
        close_run(run_start, n - 1);
    }

    // H4 — route raw reads through code-intel at 1/5 the tokens. Each
    // avoided token would have been re-read on every remaining request.
    let raw_tokens: u64 = raw_acq.iter().map(|r| r.tokens).sum();
    let route_raw_saved: f64 = raw_acq
        .iter()
        .map(|r| {
            let avoided = r.tokens - r.tokens / INTEL_COMPRESSION;
            (avoided as f64) * ((n.saturating_sub(r.req_index)) as f64) / 1e6 * p_dom
        })
        .sum();

    Some(CfReport {
        file: path.file_name().and_then(|f| f.to_str()).unwrap_or("").to_string(),
        actual_total_cost: actual_total,
        actual_cr_cost,
        preamble0,
        n_requests: n,
        splits: splits_out,
        halve_preamble_saved,
        injection_tokens,
        cap_injections_saved,
        batch_upper_bound_saved: batch_saved,
        raw_tokens,
        route_raw_saved,
    })
}

fn print_counterfactual(reports: &[CfReport]) {
    let total: f64 = reports.iter().map(|r| r.actual_total_cost).sum();
    let total_cr: f64 = reports.iter().map(|r| r.actual_cr_cost).sum();
    println!(
        "counterfactual replay — {} session(s), actual total ${total:.2} (cache-read ${total_cr:.2})\n",
        reports.len()
    );

    let lever = |label: String, saved: f64| {
        println!("  {label:<44} ${saved:>9.2}   {:>5.1}%", saved / total * 100.0);
    };

    for (idx, k) in [100_000u64, 140_000, 200_000].iter().enumerate() {
        let saved: f64 = reports.iter().map(|r| r.splits[idx].2).sum();
        let splits: u64 = reports.iter().map(|r| r.splits[idx].1).sum();
        lever(format!("H1 split sessions at {}k ({splits} splits)", k / 1000), saved);
    }
    let avg_preamble: u64 =
        reports.iter().map(|r| r.preamble0).sum::<u64>() / reports.len().max(1) as u64;
    lever(
        format!("H2a halve turn-0 preamble (avg {}k tok)", avg_preamble / 1000),
        reports.iter().map(|r| r.halve_preamble_saved).sum(),
    );
    let inj_total: u64 = reports.iter().map(|r| r.injection_tokens).sum();
    lever(
        format!("H2b cap hook injections at {INJECTION_CAP} tok ({}k injected)", inj_total / 1000),
        reports.iter().map(|r| r.cap_injections_saved).sum(),
    );
    lever(
        "H3 batch small turns 3:1 (UPPER BOUND)".to_string(),
        reports.iter().map(|r| r.batch_upper_bound_saved).sum(),
    );
    let raw_total: u64 = reports.iter().map(|r| r.raw_tokens).sum();
    lever(
        format!("H4 route raw reads via code-intel ({}k raw)", raw_total / 1000),
        reports.iter().map(|r| r.route_raw_saved).sum(),
    );

    println!(
        "\nassumptions: split seed = preamble + {}k (frame+brief+re-acquisition), splits \
         re-written as 1h cache; injections detected by hook markers; small turn = ctx \
         growth < {}; intel compression = {}x. Levers are INDEPENDENT counterfactuals — \
         they overlap (a split also evicts raw-read tokens); do not sum them.",
        SPLIT_SEED_EXTRA / 1000,
        SMALL_TURN_GROWTH,
        INTEL_COMPRESSION
    );
    println!("per-session: svrn cache-audit --counterfactual --session <id>");
}

fn print_counterfactual_detail(r: &CfReport) {
    println!(
        "=== {} — counterfactual ===\n{} requests, preamble {}k tok, actual ${:.2} (cache-read ${:.2})\n",
        short_session_id(&r.file),
        r.n_requests,
        r.preamble0 / 1000,
        r.actual_total_cost,
        r.actual_cr_cost
    );
    for (k, splits, saved) in &r.splits {
        println!("  H1 split at {:>4}k   {splits:>3} split(s)   ${saved:>8.2}", k / 1000);
    }
    println!("  H2a halve preamble               ${:>8.2}", r.halve_preamble_saved);
    println!(
        "  H2b cap injections ({:>4}k tok)   ${:>8.2}",
        r.injection_tokens / 1000,
        r.cap_injections_saved
    );
    println!("  H3 batch small turns (UPPER)     ${:>8.2}", r.batch_upper_bound_saved);
    println!(
        "  H4 route raw reads ({:>4}k tok)   ${:>8.2}",
        r.raw_tokens / 1000,
        r.route_raw_saved
    );
}

fn print_help() {
    println!(
        "Usage: svrn cache-audit [options]\n\n\
         Glassbox telemetry for the fleet's context spend. Parses Claude Code\n\
         transcripts and reports, per session, where the cache/token budget went\n\
         and the RAW-ACQUISITION RATIO (raw file/grep reads pulled into context\n\
         vs. code-intelligence / RAG calls made).\n\n\
         Options:\n\
         \x20 --project <path>   Audit the project whose working dir is <path>\n\
         \x20                    (default: the current working directory).\n\
         \x20 --dir <path>       Audit a specific directory of .jsonl transcripts\n\
         \x20                    (overrides --project).\n\
         \x20 --session <id>     Detailed breakdown for one session; <id> matches\n\
         \x20                    a filename prefix (e.g. the short session id).\n\
         \x20 --last <N>         Show the N most recent sessions (default 10).\n\
         \x20 --sort <key>       cost | recent | ratio  (default cost).\n\
         \x20 --json             Machine-readable output.\n\
         \x20 --ramp             Ramp-up cost per session: acquisition tokens + calls\n\
         \x20                    before the first Edit/Write, and repeated file Reads.\n\
         \x20                    The split-safety gauge (successor should ramp <=5k, 0\n\
         \x20                    repeats). Combine with --session <id> for one session.\n\
         \x20 --counterfactual   Replay sessions under four independent cost levers\n\
         \x20                    (splitting, preamble/injection overhead, turn\n\
         \x20                    batching, acquisition routing) and price each —\n\
         \x20                    pure arithmetic on existing transcripts, no LLM.\n\
         \x20                    Combine with --session <id> for per-session detail.\n\
         \x20 --help, -h         Show this message.\n\n\
         Examples:\n\
         \x20 svrn cache-audit                         # recent sessions, this project\n\
         \x20 svrn cache-audit --sort ratio            # worst raw-acquisition first\n\
         \x20 svrn cache-audit --session 06551399      # deep-dive one session\n"
    );
}

/// Shared with `session_cmd` — both read the same transcript layout.
pub(crate) fn resolve_transcript_dir(project: Option<&str>, dir: Option<&str>) -> Result<PathBuf, String> {
    if let Some(d) = dir {
        return Ok(PathBuf::from(d));
    }
    let cwd = project
        .map(|p| p.to_string())
        .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()))
        .ok_or_else(|| "could not determine the current working directory".to_string())?;
    let home = dirs::home_dir().ok_or_else(|| "could not locate the home directory".to_string())?;
    let encoded = encode_project_path(&cwd);
    Ok(home.join(".claude").join("projects").join(encoded))
}

fn collect_reports(dir: &Path) -> Result<Vec<SessionReport>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        format!(
            "no transcripts at {} ({e}).\n\
             Pass --dir <path> to point at a transcript directory, or --project <path>\n\
             to name the project's working directory.",
            dir.display()
        )
    })?;
    let mut reports = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            if let Some(r) = analyze(&path) {
                reports.push(r);
            }
        }
    }
    Ok(reports)
}

fn fmt_ratio(r: &SessionReport) -> String {
    // reads(k tokens) : code-intel calls
    let reads_k = r.raw_acq_tokens / 1000;
    format!("{reads_k}k:{}", r.code_intel_calls)
}

fn print_table(reports: &[SessionReport]) {
    println!(
        "{:<10} {:>6} {:>10} {:>8} {:>10} {:>12}  {}",
        "session", "turns", "peak_ctx", "cache%", "cost($)", "reads:intel", "model"
    );
    println!("{}", "-".repeat(78));
    let mut tot = 0.0;
    let mut tot_saved = 0.0;
    for r in reports {
        let total = r.total_cost();
        tot += total;
        tot_saved += r.counterfactual_no_cache_read() - r.cost_cache_read;
        let cache_pct = if total > 0.0 { r.cost_cache_read / total * 100.0 } else { 0.0 };
        let model = r.model.split('[').next().unwrap_or(&r.model);
        println!(
            "{:<10} {:>6} {:>10} {:>7.1}% {:>10.2} {:>12}  {}{}",
            short_session_id(&r.file),
            r.turns,
            r.max_ctx,
            cache_pct,
            total,
            fmt_ratio(r),
            model,
            if r.model_assumed { " (assumed)" } else { "" },
        );
    }
    println!("{}", "-".repeat(78));
    println!(
        "{} session(s)  total ${:.2}  (caching already saved ~${:.2} vs. uncached input)",
        reports.len(),
        tot,
        tot_saved
    );
    println!(
        "reads:intel = raw file/grep tokens pulled into context : code-intelligence calls made.\n\
         A high left number with 0 on the right is the leak — route acquisition through\n\
         `symbols`/`callers`/`code_search`/`notes` (MCP or `sovereign tools call ...`)."
    );
}

fn print_detail(r: &SessionReport) {
    let total = r.total_cost();
    let model = r.model.split('[').next().unwrap_or(&r.model);
    println!(
        "=== {} ===\n{} turns, peak context {} tok, model {}{}",
        short_session_id(&r.file),
        r.turns,
        r.max_ctx,
        model,
        if r.model_assumed { " (pricing assumed)" } else { "" },
    );
    println!("\ntokens:");
    println!("  fresh input   {:>14}", r.input);
    println!("  cache read    {:>14}", r.cache_read);
    println!("  cache create  {:>14}  (5m {} / 1h {})", r.cache_write_5m + r.cache_write_1h, r.cache_write_5m, r.cache_write_1h);
    println!("  output        {:>14}", r.output);

    println!("\ncost breakdown (total ${:.2}):", total);
    let row = |label: &str, c: f64| {
        let pct = if total > 0.0 { c / total * 100.0 } else { 0.0 };
        println!("  {:<14} ${:>9.2}   {:>5.1}%", label, c, pct);
    };
    row("fresh input", r.cost_input);
    row("CACHE READ", r.cost_cache_read);
    row("cache create", r.cost_cache_write);
    row("output", r.cost_output);
    println!(
        "  caching already saved ~${:.2} vs. billing those reads as fresh input.",
        r.counterfactual_no_cache_read() - r.cost_cache_read
    );

    println!("\ncontext pulled by tool category:");
    let tot_ctx: u64 = r.buckets.values().map(|b| b.ctx_tokens).sum::<u64>().max(1);
    let mut rows: Vec<(&Bucket, &BucketStat)> = r.buckets.iter().collect();
    rows.sort_by(|a, b| b.1.ctx_tokens.cmp(&a.1.ctx_tokens));
    for (bucket, stat) in rows {
        println!(
            "  {:<16} {:>5} calls  {:>12} tok  {:>5.1}%",
            bucket.label(),
            stat.calls,
            stat.ctx_tokens,
            stat.ctx_tokens as f64 / tot_ctx as f64 * 100.0
        );
    }
    println!("  [Bash cat/head/grep-style raw reads: {}]", r.bash_read_like);

    println!("\nraw-acquisition ratio: {} raw-read tokens : {} code-intelligence calls", r.raw_acq_tokens, r.code_intel_calls);
    if r.code_intel_calls == 0 && r.raw_acq_tokens > 0 {
        println!("  ^ 0 code-intel calls. This session acquired codebase context entirely via raw");
        println!("    reads; every one rides the cache-read tail for the rest of the session.");
    }

    println!("\ntop tools by context pulled:");
    for (name, calls, toks) in &r.top_tools {
        println!("  {:<28} {:>5} calls  {:>12} tok", name, calls, toks);
    }
}

/// Escape a string for embedding in JSON output (minimal — strings here are
/// filenames and model ids, no control chars expected, but be safe).
fn json_str(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

fn print_json(reports: &[SessionReport]) {
    let mut out = String::from("[");
    for (i, r) in reports.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let cache_pct = if r.total_cost() > 0.0 { r.cost_cache_read / r.total_cost() * 100.0 } else { 0.0 };
        out.push_str(&format!(
            "{{\"session\":{},\"file\":{},\"model\":{},\"model_assumed\":{},\
             \"turns\":{},\"peak_ctx\":{},\"tokens\":{{\"input\":{},\"cache_read\":{},\
             \"cache_write_5m\":{},\"cache_write_1h\":{},\"output\":{}}},\
             \"cost_usd\":{{\"input\":{:.4},\"cache_read\":{:.4},\"cache_write\":{:.4},\"output\":{:.4},\"total\":{:.4}}},\
             \"cache_read_pct\":{:.1},\"raw_acq_tokens\":{},\"code_intel_calls\":{},\"bash_read_like\":{},\"mtime_unix\":{}}}",
            json_str(&short_session_id(&r.file)),
            json_str(&r.file),
            json_str(&r.model),
            r.model_assumed,
            r.turns,
            r.max_ctx,
            r.input,
            r.cache_read,
            r.cache_write_5m,
            r.cache_write_1h,
            r.output,
            r.cost_input,
            r.cost_cache_read,
            r.cost_cache_write,
            r.cost_output,
            r.total_cost(),
            cache_pct,
            r.raw_acq_tokens,
            r.code_intel_calls,
            r.bash_read_like,
            r.mtime_unix,
        ));
    }
    out.push(']');
    println!("{out}");
}

pub async fn run(args: &[String]) -> i32 {
    // -------- arg parsing --------
    let mut project: Option<String> = None;
    let mut dir: Option<String> = None;
    let mut session: Option<String> = None;
    let mut last: usize = 10;
    let mut sort = "cost".to_string();
    let mut json = false;
    let mut counterfactual = false;
    let mut ramp = false;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" | "help" => {
                print_help();
                return 0;
            }
            "--json" => json = true,
            "--counterfactual" => counterfactual = true,
            "--ramp" => ramp = true,
            "--project" => project = it.next().cloned(),
            "--dir" => dir = it.next().cloned(),
            "--session" => session = it.next().cloned(),
            "--sort" => {
                if let Some(v) = it.next() {
                    sort = v.clone();
                }
            }
            "--last" => {
                if let Some(v) = it.next() {
                    match v.parse::<usize>() {
                        Ok(n) => last = n,
                        Err(_) => {
                            eprintln!("cache-audit: --last expects a number, got `{v}`");
                            return 2;
                        }
                    }
                }
            }
            other => {
                eprintln!("cache-audit: unknown argument `{other}` (try --help)");
                return 2;
            }
        }
    }

    let target_dir = match resolve_transcript_dir(project.as_deref(), dir.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cache-audit: {e}");
            return 2;
        }
    };

    let mut reports = match collect_reports(&target_dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cache-audit: {e}");
            return 2;
        }
    };

    if reports.is_empty() {
        eprintln!("cache-audit: no sessions with usage data in {}", target_dir.display());
        return 1;
    }

    // -------- ramp-up cost --------
    if ramp {
        reports.sort_by(|a, b| b.mtime_unix.cmp(&a.mtime_unix));
        let selected: Vec<&SessionReport> = match &session {
            Some(sel) => reports
                .iter()
                .filter(|r| r.file.starts_with(sel.as_str()) || short_session_id(&r.file) == *sel)
                .collect(),
            None => reports.iter().take(last).collect(),
        };
        let ramps: Vec<RampReport> = selected
            .iter()
            .filter_map(|r| analyze_ramp(&target_dir.join(&r.file)))
            .collect();
        if ramps.is_empty() {
            eprintln!("cache-audit: no ramp data for the selection");
            return 1;
        }
        print_ramp(&ramps);
        return 0;
    }

    // -------- counterfactual replay --------
    if counterfactual {
        // Same session selection as the ranked table: most recent `last`.
        reports.sort_by(|a, b| b.mtime_unix.cmp(&a.mtime_unix));
        let selected: Vec<&SessionReport> = match &session {
            Some(sel) => reports
                .iter()
                .filter(|r| r.file.starts_with(sel.as_str()) || short_session_id(&r.file) == *sel)
                .collect(),
            None => reports.iter().take(last).collect(),
        };
        if selected.is_empty() {
            eprintln!("cache-audit: no session matching the selection");
            return 1;
        }
        let cf: Vec<CfReport> = selected
            .iter()
            .filter_map(|r| analyze_counterfactual(&target_dir.join(&r.file)))
            .collect();
        if cf.is_empty() {
            eprintln!("cache-audit: counterfactual replay produced no data");
            return 1;
        }
        if session.is_some() {
            for r in &cf {
                print_counterfactual_detail(r);
            }
        } else {
            print_counterfactual(&cf);
        }
        return 0;
    }

    // -------- single-session deep dive --------
    if let Some(sel) = &session {
        let matches: Vec<&SessionReport> = reports
            .iter()
            .filter(|r| r.file.starts_with(sel.as_str()) || short_session_id(&r.file) == *sel)
            .collect();
        match matches.as_slice() {
            [] => {
                eprintln!("cache-audit: no session matching `{sel}` in {}", target_dir.display());
                return 1;
            }
            [one] => {
                print_detail(one);
                return 0;
            }
            many => {
                eprintln!("cache-audit: `{sel}` is ambiguous ({} matches):", many.len());
                for m in many {
                    eprintln!("  {}", short_session_id(&m.file));
                }
                return 2;
            }
        }
    }

    // -------- ranked table --------
    match sort.as_str() {
        "recent" => reports.sort_by(|a, b| b.mtime_unix.cmp(&a.mtime_unix)),
        "ratio" => reports.sort_by(|a, b| {
            // Worst first: most raw-read tokens per code-intel call.
            let ra = a.raw_acq_tokens as f64 / (a.code_intel_calls as f64 + 1.0);
            let rb = b.raw_acq_tokens as f64 / (b.code_intel_calls as f64 + 1.0);
            rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
        }),
        "cost" => reports.sort_by(|a, b| {
            b.total_cost().partial_cmp(&a.total_cost()).unwrap_or(std::cmp::Ordering::Equal)
        }),
        other => {
            eprintln!("cache-audit: unknown --sort key `{other}` (cost | recent | ratio)");
            return 2;
        }
    }
    reports.truncate(last);

    if json {
        print_json(&reports);
    } else {
        print_table(&reports);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_routes_tools_to_buckets() {
        assert_eq!(classify("Read"), Bucket::RawFileSearch);
        assert_eq!(classify("Bash"), Bucket::Bash);
        assert_eq!(classify("Edit"), Bucket::EditWrite);
        assert_eq!(classify("Task"), Bucket::Subagent);
        assert_eq!(classify("symbols"), Bucket::CodeIntel);
        assert_eq!(classify("mcp__sovereign__code_search"), Bucket::CodeIntel);
        assert_eq!(classify("callers"), Bucket::CodeIntel);
        assert_eq!(classify("mcp__other__frobnicate"), Bucket::OtherMcp);
        assert_eq!(classify("SomethingElse"), Bucket::Other);
    }

    #[test]
    fn pricing_matches_model_family() {
        assert!(!Pricing::for_model("claude-opus-4-8[1m]").assumed);
        assert_eq!(Pricing::for_model("claude-opus-4-8").input, 5.0);
        assert_eq!(Pricing::for_model("claude-sonnet-5").input, 3.0);
        assert_eq!(Pricing::for_model("claude-haiku-4-5").input, 1.0);
        assert_eq!(Pricing::for_model("claude-fable-5").input, 10.0);
        assert!(Pricing::for_model("gpt-4").assumed); // unknown -> flagged
    }

    #[test]
    fn bash_read_like_detects_raw_reads() {
        assert!(is_bash_read_like("cat foo.rs"));
        assert!(is_bash_read_like("rg pattern src/"));
        assert!(!is_bash_read_like("cargo build --release"));
    }

    #[test]
    fn sovereign_cli_detected_by_command_position_only() {
        assert!(is_sovereign_cli("sovereign tools call symbols --name=Foo"));
        assert!(is_sovereign_cli("svrn notes add --kind decision -m x"));
        assert!(is_sovereign_cli("SOVEREIGN_NO_STALE_WARN=1 sovereign cache-audit"));
        assert!(is_sovereign_cli("cd /repo && sovereign tools call callers --symbol=f"));
        assert!(is_sovereign_cli("target/debug/sovereign-cli session list"));
        assert!(is_sovereign_cli("sovereign tools call lint_status 2>&1 | head -5"));
        // Argument/pattern positions must NOT match.
        assert!(!is_sovereign_cli("cargo build -p sovereign-cli"));
        assert!(!is_sovereign_cli("rg sovereign src/"));
        assert!(!is_sovereign_cli("cat sovereign/SYSTEM_OVERVIEW.md"));
    }

    #[test]
    fn analyze_counts_sovereign_bash_as_code_intel_not_raw() {
        // A sovereign-CLI Bash call: its call AND its result tokens must land
        // in CodeIntel, with zero raw acquisition — the compliant CLI path
        // must not be indistinguishable from the leak.
        let body = concat!(
            r#"{"message":{"role":"assistant","model":"claude-opus-4-8","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"sovereign tools call symbols --name=Foo"}}]}}"#,
            "\n",
            r#"{"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}]}}"#,
            "\n"
        );
        let dir = std::env::temp_dir().join(format!("cache_audit_svrn_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dcba4321-session.jsonl");
        std::fs::write(&path, body).unwrap();

        let r = analyze(&path).expect("has usage");
        assert_eq!(r.code_intel_calls, 1);
        assert_eq!(r.raw_acq_tokens, 0);
        assert_eq!(r.bash_read_like, 0);
        let intel = r.buckets.get(&Bucket::CodeIntel).expect("intel bucket");
        assert_eq!(intel.calls, 1);
        assert!(intel.ctx_tokens > 0, "result tokens route to CodeIntel");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn encode_project_path_matches_claude_code_slug() {
        assert_eq!(
            encode_project_path("/Users/alexsbryan/dev/commonwealth-ai"),
            "-Users-alexsbryan-dev-commonwealth-ai"
        );
    }

    #[test]
    fn analyze_computes_cost_and_ratio_from_a_synthetic_transcript() {
        // One assistant turn with usage + a Read tool_use, then a user turn
        // with the matching tool_result carrying ~400 chars (~100 tokens).
        let body = concat!(
            r#"{"message":{"role":"assistant","model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":1000000,"cache_creation_input_tokens":2000},"content":[{"type":"tool_use","id":"t1","name":"Read"}]}}"#,
            "\n",
            r#"{"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}]}}"#,
            "\n"
        );
        let dir = std::env::temp_dir().join(format!("cache_audit_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("abcd1234-session.jsonl");
        std::fs::write(&path, body).unwrap();

        let r = analyze(&path).expect("transcript has usage");
        assert_eq!(r.turns, 1);
        assert_eq!(r.cache_read, 1_000_000);
        // cache-read cost at Opus $0.50/MTok = $0.50; it should dominate.
        assert!((r.cost_cache_read - 0.50).abs() < 1e-6);
        assert!(r.cost_cache_read > r.cost_input + r.cost_output);
        // One Read produced raw-acquisition tokens and zero code-intel calls.
        assert!(r.raw_acq_tokens > 0);
        assert_eq!(r.code_intel_calls, 0);

        std::fs::remove_dir_all(&dir).ok();
    }
}
