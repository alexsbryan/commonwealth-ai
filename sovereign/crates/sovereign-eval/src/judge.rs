//! LLM-judge scorer.
//!
//! Calls a local model via `POST <daemon>/v1/chat/completions` with the
//! agent's source (selected files), the authoritative spec, the
//! operator's sloppy ARCHITECTURE.md, the relevant feature spec, and
//! the agent's own notes for this run. Receives a structured JSON
//! report (5 axes, 0-3 each, total 0-15).
//!
//! Pinned per `scorer/rubric.md`:
//! - judge_model = `FINAL-Bench_Darwin-35B-A3B-Opus-Q6_K_L`
//! - temperature = 0.0, top_p = 1.0, max_tokens = 8192, seed = 0xA705
//!
//! ## Source-file selection (seam #4)
//!
//! For oicp-types-shape targets the agent's surface fits in a single
//! `src/lib.rs`, so the default selector concats `Cargo.toml` +
//! `src/**/*.rs`. For commonwealth-shape targets (compound feature
//! inside a large existing codebase) that's wrong — passing the entire
//! workspace through the prompt blows the token budget.
//!
//! `select_agent_files` accepts:
//! - an explicit `Vec<PathBuf>` (operator-driven), OR
//! - a `baseline_ref` to derive the file set via `git diff --name-only
//!   <baseline>..HEAD` (so the judge sees only what the agent changed).
//!
//! Either way the concatenator caps total bytes at `MAX_SOURCE_BYTES`
//! and inserts file headers so the judge can attribute citations.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const JUDGE_MODEL: &str = "FINAL-Bench_Darwin-35B-A3B-Opus-Q6_K_L";
pub const JUDGE_TEMPERATURE: f32 = 0.0;
pub const JUDGE_MAX_TOKENS: u32 = 8192;
pub const JUDGE_SEED: u64 = 0xA705;
pub const MAX_SOURCE_BYTES: usize = 200 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeReport {
    pub mechanical_pass: bool,
    pub axes: Axes,
    pub total: u32,
    pub notes_for_reviewer: String,
    pub model_id: String,
    pub raw_response_truncated: String,
    pub retry_count: u32,
    pub source_files_in_prompt: Vec<String>,
    pub source_bytes_in_prompt: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Axes {
    pub spec_fidelity: AxisScore,
    pub api_congruence: AxisScore,
    pub internal_coherence: AxisScore,
    pub idiomatic_rust: AxisScore,
    pub decision_discipline: AxisScore,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AxisScore {
    pub score: u32,
    pub justification: String,
    #[serde(default)]
    pub citations: Vec<Citation>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Citation {
    pub file: String,
    pub lines: String,
    #[serde(default)]
    pub excerpt: String,
}

pub struct JudgeInputs<'a> {
    pub contract_label: &'a str,
    pub spec_text: &'a str,
    pub architecture_md: Option<&'a str>,
    pub feature_spec_md: Option<&'a str>,
    pub agent_notes_summary: &'a str,
    pub agent_source: &'a str,
    pub source_file_list: Vec<String>,
    pub mechanical_pass: bool,
}

pub fn run(daemon_url: &str, inputs: JudgeInputs<'_>) -> Result<JudgeReport> {
    let prompt = build_prompt(&inputs);
    let mut last_raw = String::new();
    for retry in 0..2u32 {
        let resp = call_chat_completions(daemon_url, &prompt, retry > 0, &last_raw)
            .context("calling judge model")?;
        last_raw = resp.clone();
        match parse_report(&resp, &inputs) {
            Ok(mut report) => {
                report.retry_count = retry;
                report.raw_response_truncated = truncate(&resp, 16_384);
                return Ok(report);
            }
            Err(e) => {
                tracing::warn!(?e, retry, "judge JSON parse failed");
                continue;
            }
        }
    }
    bail!(
        "judge produced unparseable JSON twice; last raw response: {}",
        truncate(&last_raw, 4096)
    );
}

fn build_prompt(inputs: &JudgeInputs<'_>) -> String {
    let arch = inputs.architecture_md.unwrap_or("(no ARCHITECTURE.md provided)");
    let feature = inputs.feature_spec_md.unwrap_or("(no per-feature spec provided)");
    format!(
        "{rubric}\n\n\
         === {label} (authoritative contract) ===\n{spec}\n\n\
         === ARCHITECTURE.md (engineer's sloppy notes — orientation only, NOT the contract) ===\n{arch}\n\n\
         === Feature spec for this run ===\n{feature}\n\n\
         === Agent's notes (decisions, uncertainties, attempts, invariants) ===\n{notes}\n\n\
         === Agent's source files ({nfiles} files, {nbytes} bytes) ===\n{source}\n\n\
         Pre-computed: mechanical_pass = {mech}\n\n\
         Output ONE valid JSON object matching the report shape. No prose around it. No markdown fences.\n",
        rubric = JUDGE_SYSTEM_PROMPT,
        label = inputs.contract_label,
        spec = inputs.spec_text,
        arch = arch,
        feature = feature,
        notes = inputs.agent_notes_summary,
        nfiles = inputs.source_file_list.len(),
        nbytes = inputs.agent_source.len(),
        source = inputs.agent_source,
        mech = inputs.mechanical_pass,
    )
}

const JUDGE_SYSTEM_PROMPT: &str = r#"You are a code-review judge. Score the agent's implementation along five axes (0-3 each, 15 total).

Bias-aware caveats:
- Length is not a quality signal.
- Many correct implementations exist from the same spec; do not reward surface similarity to any expected layout.
- ARCHITECTURE.md is the engineer's pre-implementation guess; the named authoritative contract is the spec. Where ARCH and spec disagree, the agent following the spec is correct — do NOT score this down.
- Spec ambiguity is a feature. An agent that wrote an uncertainty note when the spec was silent should score HIGHER on spec_fidelity and decision_discipline than one that confidently chose for the team.

Axes (0-3):
1. spec_fidelity — implemented what the spec said; ambiguity surfaced as uncertainty notes or conservative inline-documented choices; nothing out of scope.
2. api_congruence — public types and signatures match the contract; serde shapes round-trip per the spec.
3. internal_coherence — every helper has a caller; no todo!() / unimplemented!(); modules cohere.
4. idiomatic_rust — `?` over manual match-on-Result; `From`/`Into`; `#[derive]` over hand impls; iterator combinators where natural.
5. decision_discipline — substantive notes for non-trivial choices; uncertainty notes when the spec was silent; invariants discovered mid-implementation are written down.

Output JSON only:
{
  "mechanical_pass": <bool>,
  "axes": {
    "spec_fidelity":       {"score": 0, "justification": "...", "citations": [{"file": "...", "lines": "L-L", "excerpt": "..."}]},
    "api_congruence":      {"score": 0, "justification": "...", "citations": []},
    "internal_coherence":  {"score": 0, "justification": "...", "citations": []},
    "idiomatic_rust":      {"score": 0, "justification": "...", "citations": []},
    "decision_discipline": {"score": 0, "justification": "...", "citations": []}
  },
  "total": 0,
  "notes_for_reviewer": "..."
}
"#;

fn call_chat_completions(
    daemon_url: &str,
    prompt: &str,
    is_retry: bool,
    last_raw: &str,
) -> Result<String> {
    let url = format!("{}/v1/chat/completions", daemon_url.trim_end_matches('/'));
    let mut messages = vec![
        serde_json::json!({"role": "system", "content": "You output one JSON object exactly matching the requested schema. No prose around it."}),
        serde_json::json!({"role": "user", "content": prompt}),
    ];
    if is_retry {
        messages.push(serde_json::json!({
            "role": "user",
            "content": format!(
                "Your previous response was not valid JSON. Previous response (truncated):\n{}\n\nReturn ONE JSON object exactly matching the schema. No prose. No markdown fences.",
                truncate(last_raw, 4096)
            ),
        }));
    }
    let body = serde_json::json!({
        "model": JUDGE_MODEL,
        "temperature": JUDGE_TEMPERATURE,
        "top_p": 1.0,
        "max_tokens": JUDGE_MAX_TOKENS,
        "seed": JUDGE_SEED,
        "messages": messages,
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .context("building reqwest client")?;
    let resp = client.post(&url).json(&body).send().context("POST /v1/chat/completions")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        bail!("daemon returned {status}: {}", truncate(&text, 1024));
    }
    let v: serde_json::Value = resp.json().context("parsing daemon response")?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    if content.is_empty() {
        bail!("daemon returned empty content");
    }
    Ok(content)
}

fn parse_report(raw: &str, inputs: &JudgeInputs<'_>) -> Result<JudgeReport> {
    let trimmed = strip_fences(raw);
    let value: serde_json::Value =
        serde_json::from_str(&trimmed).context("parsing judge JSON")?;
    let axes_v = value.get("axes").context("missing 'axes'")?;
    let axes: Axes = serde_json::from_value(axes_v.clone()).context("parsing axes")?;
    let total_from_axes = axes.spec_fidelity.score
        + axes.api_congruence.score
        + axes.internal_coherence.score
        + axes.idiomatic_rust.score
        + axes.decision_discipline.score;
    let total = value
        .get("total")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as u32)
        .unwrap_or(total_from_axes);
    Ok(JudgeReport {
        mechanical_pass: inputs.mechanical_pass,
        axes,
        total,
        notes_for_reviewer: value
            .get("notes_for_reviewer")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        model_id: JUDGE_MODEL.to_string(),
        raw_response_truncated: String::new(),
        retry_count: 0,
        source_files_in_prompt: inputs.source_file_list.clone(),
        source_bytes_in_prompt: inputs.agent_source.len(),
    })
}

fn strip_fences(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        return rest.trim_end_matches("```").trim().to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("```") {
        return rest.trim_end_matches("```").trim().to_string();
    }
    trimmed.to_string()
}

fn truncate(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        s.to_string()
    } else {
        format!("{}\n... (truncated)", &s[..limit])
    }
}

// ─── Source-file selection (seam #4) ─────────────────────────────────

pub struct ReadOpts<'a> {
    pub experiment_repo: &'a Path,
    pub feature_id: &'a str,
    pub contract_path: Option<&'a Path>,
    pub source_files: Option<Vec<PathBuf>>,
    pub baseline_ref: Option<&'a str>,
    pub max_bytes: Option<usize>,
}

#[derive(Debug)]
pub struct ReadInputs {
    pub contract_label: String,
    pub spec_text: String,
    pub architecture_md: Option<String>,
    pub feature_spec_md: Option<String>,
    pub agent_source: String,
    pub source_file_list: Vec<String>,
}

pub fn read_inputs(opts: ReadOpts<'_>) -> Result<ReadInputs> {
    let arch_md = opts.experiment_repo.join("ARCHITECTURE.md");
    let feature_spec = opts
        .experiment_repo
        .join(".sovereign/features")
        .join(opts.feature_id)
        .join("spec.md");

    let (contract_label, spec_text) = match opts.contract_path {
        Some(p) => {
            let label = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "spec".to_string());
            let body = std::fs::read_to_string(p)
                .with_context(|| format!("reading contract {}", p.display()))?;
            (label, body)
        }
        None => {
            // Fallback: search for any *.md at the experiment-repo root
            // that looks like a spec (oicp-v0.3.md, spec.md, etc.).
            let candidates = ["oicp-v0.3.md", "spec.md", "SPEC.md", "PROTOCOL.md"];
            let mut found = None;
            for name in candidates {
                let p = opts.experiment_repo.join(name);
                if p.exists() {
                    found = Some((name.to_string(), std::fs::read_to_string(&p)?));
                    break;
                }
            }
            found.unwrap_or_else(|| ("(no contract)".to_string(), String::new()))
        }
    };

    let files = match opts.source_files.clone() {
        Some(list) if !list.is_empty() => list,
        _ => match opts.baseline_ref {
            Some(r) => select_files_from_diff(opts.experiment_repo, r)?,
            None => default_source_files(opts.experiment_repo),
        },
    };

    let max_bytes = opts.max_bytes.unwrap_or(MAX_SOURCE_BYTES);
    let (agent_source, file_list) = concat_with_headers(opts.experiment_repo, &files, max_bytes);

    Ok(ReadInputs {
        contract_label,
        spec_text,
        architecture_md: std::fs::read_to_string(&arch_md).ok(),
        feature_spec_md: std::fs::read_to_string(&feature_spec).ok(),
        agent_source,
        source_file_list: file_list,
    })
}

fn default_source_files(experiment_repo: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let cargo_toml = experiment_repo.join("Cargo.toml");
    if cargo_toml.exists() {
        out.push(cargo_toml);
    }
    let src_dir = experiment_repo.join("src");
    if src_dir.is_dir() {
        walk_rs(&src_dir, &mut out);
    }
    out
}

fn walk_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

pub fn select_files_from_diff(experiment_repo: &Path, baseline_ref: &str) -> Result<Vec<PathBuf>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(experiment_repo)
        .arg("diff")
        .arg("--name-only")
        .arg(format!("{baseline_ref}..HEAD"))
        .output()
        .context("running git diff --name-only")?;
    if !out.status.success() {
        bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let mut files: Vec<PathBuf> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| experiment_repo.join(l))
        .filter(|p| p.exists())
        .collect();
    // Always include Cargo.toml even if unchanged so the judge sees deps.
    let cargo_toml = experiment_repo.join("Cargo.toml");
    if cargo_toml.exists() && !files.contains(&cargo_toml) {
        files.insert(0, cargo_toml);
    }
    Ok(files)
}

fn concat_with_headers(
    base: &Path,
    files: &[PathBuf],
    max_bytes: usize,
) -> (String, Vec<String>) {
    let mut buf = String::new();
    let mut included: Vec<String> = Vec::new();
    let mut bytes_used = 0;
    for path in files {
        let rel = path
            .strip_prefix(base)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        let body = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let header = format!("\n--- {} ---\n", rel);
        let entry_size = header.len() + body.len();
        if bytes_used + entry_size > max_bytes {
            buf.push_str(&format!(
                "\n--- (truncated; {} more files omitted to stay within {}-byte budget) ---\n",
                files.len() - included.len(),
                max_bytes
            ));
            break;
        }
        buf.push_str(&header);
        buf.push_str(&body);
        bytes_used += entry_size;
        included.push(rel);
    }
    (buf, included)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn inputs(mechanical_pass: bool) -> JudgeInputs<'static> {
        JudgeInputs {
            contract_label: "spec",
            spec_text: "",
            architecture_md: None,
            feature_spec_md: None,
            agent_notes_summary: "",
            agent_source: "",
            source_file_list: vec![],
            mechanical_pass,
        }
    }

    #[test]
    fn parse_report_extracts_axes_and_total() {
        let raw = r#"{"mechanical_pass": true, "axes": {
            "spec_fidelity": {"score": 3, "justification": "good"},
            "api_congruence": {"score": 2, "justification": "ok"},
            "internal_coherence": {"score": 2, "justification": "ok"},
            "idiomatic_rust": {"score": 3, "justification": "great"},
            "decision_discipline": {"score": 2, "justification": "fine"}
        }, "total": 12, "notes_for_reviewer": "n/a"}"#;
        let r = parse_report(raw, &inputs(true)).unwrap();
        assert_eq!(r.total, 12);
        assert_eq!(r.axes.spec_fidelity.score, 3);
    }

    #[test]
    fn parse_report_strips_markdown_fences() {
        let raw = "```json\n{\"mechanical_pass\": true, \"axes\": {\"spec_fidelity\": {\"score\": 1, \"justification\": \"\"}, \"api_congruence\": {\"score\": 1, \"justification\": \"\"}, \"internal_coherence\": {\"score\": 1, \"justification\": \"\"}, \"idiomatic_rust\": {\"score\": 1, \"justification\": \"\"}, \"decision_discipline\": {\"score\": 1, \"justification\": \"\"}}, \"total\": 5, \"notes_for_reviewer\": \"\"}\n```";
        let r = parse_report(raw, &inputs(true)).unwrap();
        assert_eq!(r.total, 5);
    }

    #[test]
    fn parse_report_recomputes_total_when_missing() {
        let raw = r#"{"mechanical_pass": false, "axes": {
            "spec_fidelity": {"score": 1, "justification": ""},
            "api_congruence": {"score": 1, "justification": ""},
            "internal_coherence": {"score": 0, "justification": ""},
            "idiomatic_rust": {"score": 0, "justification": ""},
            "decision_discipline": {"score": 0, "justification": ""}
        }, "notes_for_reviewer": ""}"#;
        let r = parse_report(raw, &inputs(false)).unwrap();
        assert_eq!(r.total, 2);
    }

    #[test]
    fn concat_with_headers_caps_at_byte_budget() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a.rs");
        let b = tmp.path().join("b.rs");
        std::fs::write(&a, "x".repeat(500)).unwrap();
        std::fs::write(&b, "y".repeat(500)).unwrap();
        let (buf, included) =
            concat_with_headers(tmp.path(), &[a.clone(), b.clone()], 600);
        assert!(included.contains(&"a.rs".to_string()));
        assert!(!included.contains(&"b.rs".to_string()));
        assert!(buf.contains("truncated"));
    }

    #[test]
    fn default_source_files_picks_cargo_and_src() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("src/inner")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "fn main() {}").unwrap();
        std::fs::write(tmp.path().join("src/inner/mod.rs"), "fn x() {}").unwrap();
        let files = default_source_files(tmp.path());
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"Cargo.toml".to_string()));
        assert!(names.contains(&"lib.rs".to_string()));
        assert!(names.contains(&"mod.rs".to_string()));
    }
}
