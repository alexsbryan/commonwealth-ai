// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich spec-intel <spec.md>` — turn a software-spec markdown file
//! into a resumable cache of *conditioned claims*.
//!
//! This mirrors `enrich code-intel`, but the unit of work is a spec SECTION
//! rather than a code symbol. The spec is split on `## ` (h2) headers; each
//! section over ~200 chars (capped at ~4000) is handed to the daemon's chat
//! model in ONE grammar-constrained call that extracts a JSON array of claims —
//! BOTH validated findings (`normativity = "contract"`) and planned / intended
//! behavior (`normativity = "proposal"`). Each claim records the independently-
//! checkable `conditions` it asserts and the code `referenced_entities` it names.
//!
//! Glassbox + patchable, like code-intel: the cache (`claims.json`) is keyed on a
//! `blake3` hash of `(section title + body)`, so a re-run skips every section that
//! hasn't changed — including sections that legitimately yielded zero claims — and
//! a checkpoint is written after each section so a crash only costs the un-done
//! tail. The Python prototype (`scratch/spec_intel.py`) parsed JSON leniently and
//! sometimes failed; here we force valid JSON via the daemon's `response_schema`
//! seam (llguidance-enforced) and keep a tolerant fallback parser for safety.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use corpus_engine::enrichment::code_intel::body_hash;
use corpus_engine::enrichment::pipeline::ChatPrompt;
use serde::{Deserialize, Serialize};

use super::config::EnrichConfig;
use super::inference_client::{probe_daemon, DaemonInferenceClient};
use sovereign_cli_shared::help::{self, Help, HelpSection};

/// Minimum section-body length (Unicode scalar values) to extract from —
/// shorter sections are list-of-headings noise. Mirrors the prototype's
/// `len(body) > 200` keep test.
const MIN_SECTION_CHARS: usize = 200;
/// Per-section input cap fed to the model — bounds the prompt size. Mirrors the
/// prototype's `body[:4000]`.
const MAX_SECTION_CHARS: usize = 4000;
/// Output-token budget for one section's claim array. Matches the prototype's
/// `max_tokens=1600`.
const MAX_OUTPUT_TOKENS: u32 = 1600;
/// Low temperature — deterministic claim extraction, not creative prose.
const TEMPERATURE: f32 = 0.1;
/// Phase id carried on every prompt so the chat client can route this bulk pass
/// to an operator-declared fast model and the daemon heartbeat logs are labelled.
const PHASE_ID: &str = "spec_intel";

/// The extraction system prompt. Generalizes to unstructured input: it reads
/// behaviors from three sources — explicit prose, DESCRIPTIONS that imply behavior
/// ("a parser for X" → "parses X"), and CODE EXAMPLES that demonstrate it — so a
/// real README (often descriptive / example-driven, not a step-by-step spec) still
/// yields claims. Extracts BOTH contracts (validated) and proposals (planned);
/// `conditions` = the ACTIONS to look for in the code (not preconditions).
const EXTRACTION_SYSTEM: &str = r#"You extract behavioral claims about a software system from text that is OFTEN informal — a terse README, a description, or usage examples, not a structured spec. List EVERY distinct thing the system does or should do, one claim per behavior, from THREE sources: (1) explicit prose ("it validates input, computes a total, and saves it" is THREE claims); (2) DESCRIPTIONS that imply behavior ("a parser and evaluator for X" means the code PARSES X and EVALUATES X — extract both); (3) CODE EXAMPLES that demonstrate behavior (a call `Foo::parse(s)` shows "parses a Foo from a string"; `a.matches(b)` shows "checks whether a matches b"). Be inclusive. Output ONLY a JSON array. Each item: {"statement": "<one behavior the code should exhibit>", "conditions": ["<a specific ACTION the code performs — what you would look for in the code, never a precondition, input, or restated context>", ...], "referenced_entities": ["<code symbol/type/file the claim names>", ...], "normativity": "contract|proposal"}. Use normativity=contract for stated/validated behavior, proposal for planned or intended behavior. Output [] only if there is genuinely no behavior described or shown."#;

const HELP: Help = Help {
    command: "svrn enrich spec-intel",
    summary: "Extract conditioned claims (validated findings + planned behavior) from a spec .md, section by section.",
    sections: &[
        HelpSection::Usage("svrn enrich spec-intel <spec.md> [--corpus=<id>]"),
        HelpSection::Flags(&[
            (
                "<spec.md>",
                "Path to a spec markdown file. Split on `## ` headers; each section over \
                 200 chars (capped at 4000) yields a grammar-constrained array of claims.",
            ),
            (
                "--corpus=<id>",
                "Corpus whose enrichment config supplies the chat model + daemon base_url \
                 (id, name, or unique substring). Default: the single installed corpus, else \
                 `commonwealth-ai`.",
            ),
        ]),
        HelpSection::Notes(
            "Requires the daemon at localhost:9741. Claims are written to \
             <data_dir>/specs/<spec-stem>/claims.json, keyed on a (title+body) hash so a \
             re-run skips unchanged sections (resumable; checkpointed per section).",
        ),
    ],
};

/// One conditioned claim extracted from a spec section. The first four fields are
/// emitted by the model (and forced by [`claims_schema`]); `source` is filled in
/// by the driver with the originating section title (the model never emits it),
/// matching the prototype's `c["source"]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Claim {
    /// One-sentence claim about how the system behaves / is meant to behave.
    statement: String,
    /// The independently-checkable parts of the claim.
    #[serde(default)]
    conditions: Vec<String>,
    /// Code symbols / types / files the claim names.
    #[serde(default)]
    referenced_entities: Vec<String>,
    /// `"contract"` (validated finding) or `"proposal"` (planned behavior).
    #[serde(default)]
    normativity: String,
    /// Section title the claim came from. Set post-extraction.
    #[serde(default)]
    source: String,
}

/// The resumable claims cache for one spec file. Keyed (on load) by
/// `section_hash`, the section-as-unit analog of code-intel's body-hash cache —
/// so an unchanged section (incl. one that yielded zero claims) is skipped.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ClaimsCache {
    /// Spec file stem this cache was built from (provenance).
    spec: String,
    /// One record per processed section, in spec order.
    sections: Vec<SectionClaims>,
}

/// The claims extracted from one section, plus the gate that keys it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SectionClaims {
    title: String,
    /// `blake3(title + "\n" + body)[..16]` — the resumability gate.
    section_hash: String,
    claims: Vec<Claim>,
}

/// JSON Schema for an array of claims — handed to the daemon as
/// `response_format.json_schema` so generation is grammar-constrained
/// (llguidance). `source` is NOT in the schema; the driver adds it after parsing.
fn claims_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "statement": { "type": "string" },
                "conditions": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "referenced_entities": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "normativity": {
                    "type": "string",
                    "enum": ["contract", "proposal"]
                }
            },
            "required": ["statement", "conditions", "referenced_entities", "normativity"],
            "additionalProperties": false
        }
    })
}

/// Split a spec markdown into `(title, body)` sections on `## ` (h2) headers.
///
/// Mirrors the prototype's `re.split(r"(?m)^## ", md)[1:]`: only lines beginning
/// with exactly `## ` are boundaries — `# ` (h1) and `### ` (h3+) lines fold into
/// the enclosing section's body. Sections whose full body is <= [`MIN_SECTION_CHARS`]
/// are dropped; longer bodies are capped at [`MAX_SECTION_CHARS`].
fn split_sections(md: &str) -> Vec<(String, String)> {
    // Byte offsets of every line that opens an h2 section.
    let mut header_offsets: Vec<usize> = Vec::new();
    let mut pos = 0usize;
    for line in md.split_inclusive('\n') {
        // `starts_with("## ")` is true only for h2: "### " has '#' (not ' ') at
        // index 2, and "# " has ' ' at index 1 — both fail the match.
        if line.starts_with("## ") {
            header_offsets.push(pos);
        }
        pos += line.len();
    }

    let mut out = Vec::new();
    // Preamble: everything before the first `## ` header. README intros carry the
    // core claims ("a parser and evaluator for X") that a `## `-only split silently
    // drops. When the doc has no `## ` at all, the preamble IS the whole document —
    // so this also covers flat specs, with no separate fallback needed.
    let first_h = header_offsets.first().copied().unwrap_or(md.len());
    let pre = &md[..first_h];
    if pre.chars().count() > MIN_SECTION_CHARS {
        let title = pre
            .lines()
            .find(|l| l.starts_with("# "))
            .map(|l| l.trim_start_matches('#').trim().to_string())
            .unwrap_or_else(|| "overview".to_string());
        out.push((title, pre.chars().take(MAX_SECTION_CHARS).collect()));
    }
    for (i, &start) in header_offsets.iter().enumerate() {
        let end = header_offsets.get(i + 1).copied().unwrap_or(md.len());
        let body_full = &md[start..end];
        if body_full.chars().count() <= MIN_SECTION_CHARS {
            continue;
        }
        let title = body_full
            .lines()
            .next()
            .unwrap_or("")
            .strip_prefix("## ")
            .unwrap_or("")
            .trim()
            .to_string();
        let body: String = body_full.chars().take(MAX_SECTION_CHARS).collect();
        out.push((title, body));
    }
    out
}

/// Parse the model's response into a claim array. Fast path: the whole response
/// is the JSON array (the grammar-constrained common case). Tolerant fallback:
/// slice the outermost `[...]` span, stripping any code fence / stray prose the
/// model wrapped around it. `None` signals an unparseable response.
fn parse_claims(text: &str) -> Option<Vec<Claim>> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Vec<Claim>>(t) {
        return Some(v);
    }
    let start = t.find('[')?;
    let end = t.rfind(']')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Vec<Claim>>(&t[start..=end]).ok()
}

/// Load the prior cache keyed by `section_hash` (empty map when absent/corrupt).
fn load_cache(path: &Path) -> HashMap<String, SectionClaims> {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str::<ClaimsCache>(&s)
            .map(|c| {
                c.sections
                    .into_iter()
                    .map(|sc| (sc.section_hash.clone(), sc))
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Write the cache (pretty JSON) — called after every section as a checkpoint.
fn save_cache(path: &Path, spec: &str, sections: &[SectionClaims]) -> Result<(), String> {
    let cache = ClaimsCache {
        spec: spec.to_string(),
        sections: sections.to_vec(),
    };
    let json = serde_json::to_string_pretty(&cache).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

/// Parse a `--key=value` / `--key value` flag out of the arg list.
fn parse_flag(args: &[String], key: &str) -> Option<String> {
    let eq = format!("{key}=");
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix(&eq) {
            return Some(v.to_string());
        }
        if a == key {
            return args.get(i + 1).cloned();
        }
    }
    None
}

pub async fn cmd_spec_intel(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }

    // First non-flag arg is the spec file path.
    let Some(spec_arg) = args.iter().find(|a| !a.starts_with('-')) else {
        eprintln!("error: missing <spec.md>");
        eprintln!();
        help::print(&HELP);
        return 2;
    };
    let spec_path = Path::new(spec_arg);
    let spec_text = match fs::read_to_string(spec_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read spec file {}: {e}", spec_path.display());
            return 1;
        }
    };
    let spec_stem = spec_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "spec".to_string());
    let spec_name = spec_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| spec_stem.clone());

    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".sovereign"));
    let indexes_dir = data_dir.join("indexes");

    // Resolve the corpus whose EnrichConfig supplies the chat model + base_url —
    // spec-intel needs the config only for those. Explicit `--corpus` is resolved
    // (id / name / substring) with a literal fallback; the default is the single
    // installed corpus, else `commonwealth-ai`.
    let corpus_id = match parse_flag(args, "--corpus") {
        Some(c) => crate::corpus_resolve::resolve_corpus_id(&indexes_dir, &c).unwrap_or(c),
        None => match crate::corpus_resolve::list_installed(&indexes_dir).as_slice() {
            [only] => only.corpus_id.clone(),
            _ => "commonwealth-ai".to_string(),
        },
    };

    let cfg = match EnrichConfig::require(&corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if !probe_daemon(&cfg.base_url).await {
        eprintln!(
            "error: daemon is not responding at {} — start it first",
            cfg.base_url
        );
        return 2;
    }
    let client = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (_embed, chat) = client.into_closures();

    let sections = split_sections(&spec_text);
    println!(
        "spec-intel: model={}  spec={}  corpus={}",
        cfg.chat_model, spec_name, corpus_id
    );
    println!(
        "{} section(s) over {} chars (capped at {} chars each)\n",
        sections.len(),
        MIN_SECTION_CHARS,
        MAX_SECTION_CHARS,
    );

    // Corpus-scoped so two repos with same-named specs (e.g. both `README.md`) don't
    // clobber each other's claims at specs/<stem>/.
    let cache_dir = data_dir.join("specs").join(&corpus_id).join(&spec_stem);
    if let Err(e) = fs::create_dir_all(&cache_dir) {
        eprintln!("error: creating {}: {e}", cache_dir.display());
        return 1;
    }
    let cache_path = cache_dir.join("claims.json");
    let prior = load_cache(&cache_path);

    let schema = claims_schema();
    let mut results: Vec<SectionClaims> = Vec::new();
    let (mut extracted, mut reused, mut failed) = (0usize, 0usize, 0usize);

    for (title, body) in &sections {
        let section_hash = body_hash(&format!("{title}\n{body}"));

        // Resume: an unchanged (title+body) is reused with no model call.
        if let Some(prev) = prior.get(&section_hash) {
            reused += 1;
            println!("## {}  [cached] ({} claims)", title, prev.claims.len());
            results.push(prev.clone());
            continue;
        }

        let prompt = ChatPrompt::new(EXTRACTION_SYSTEM, body.as_str())
            .with_response_schema("SpecClaims", schema.clone())
            .with_phase_id(PHASE_ID)
            .with_temperature(TEMPERATURE)
            .with_max_output_tokens(MAX_OUTPUT_TOKENS);

        let raw = match (chat)(&prompt).await {
            Ok(s) => s,
            Err(e) => {
                failed += 1;
                println!("## {}  [error: {e}]", title);
                continue;
            }
        };
        let Some(mut claims) = parse_claims(&raw) else {
            failed += 1;
            println!("## {}  [parse-fail]", title);
            continue;
        };
        for c in &mut claims {
            c.source = title.clone();
        }
        extracted += 1;
        println!("## {}  ({} claims)", title, claims.len());
        for c in &claims {
            let stmt: String = c.statement.chars().take(96).collect();
            let norm = if c.normativity.is_empty() {
                "?"
            } else {
                c.normativity.as_str()
            };
            println!("  [{norm}] {stmt}");
        }
        results.push(SectionClaims {
            title: title.clone(),
            section_hash,
            claims,
        });
        // Checkpoint after every section so a crash costs only the un-done tail.
        if let Err(e) = save_cache(&cache_path, &spec_stem, &results) {
            eprintln!("warning: could not checkpoint cache: {e}");
        }
        println!();
    }

    // Final write — also covers the all-cached / no-section case.
    if let Err(e) = save_cache(&cache_path, &spec_stem, &results) {
        eprintln!("error: writing {}: {e}", cache_path.display());
        return 1;
    }

    let total_claims: usize = results.iter().map(|s| s.claims.len()).sum();
    let contracts = results
        .iter()
        .flat_map(|s| &s.claims)
        .filter(|c| c.normativity == "contract")
        .count();
    let proposals = results
        .iter()
        .flat_map(|s| &s.claims)
        .filter(|c| c.normativity == "proposal")
        .count();
    println!(
        "=== spec-intel: {} claims ({} contract, {} proposal) across {} section(s) \
         [{} extracted, {} reused, {} failed] -> {} ===",
        total_claims,
        contracts,
        proposals,
        results.len(),
        extracted,
        reused,
        failed,
        cache_path.display(),
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_sections_skips_short_and_caps_long() {
        let big_body = "x".repeat(500);
        let md = format!(
            "# Title\npreamble\n\n## Short\ntoo short\n\n## Big\n{big_body}\n\n### Sub stays inside\nmore\n"
        );
        let secs = split_sections(&md);
        // "Short" is < 200 chars total -> skipped; "Big" kept.
        assert_eq!(secs.len(), 1);
        assert_eq!(secs[0].0, "Big");
        assert!(secs[0].1.chars().count() <= MAX_SECTION_CHARS);
        // Only `## ` splits — the `### Sub` subsection folds into Big's body.
        assert!(secs[0].1.contains("Sub stays inside"));
    }

    #[test]
    fn split_sections_only_h2_is_a_boundary() {
        let body = "y".repeat(300);
        let md = format!("## One\n{body}\n### NotABoundary\n{body}\n## Two\n{body}\n");
        let secs = split_sections(&md);
        assert_eq!(secs.len(), 2, "only `## ` headers split");
        assert_eq!(secs[0].0, "One");
        assert_eq!(secs[1].0, "Two");
    }

    #[test]
    fn parse_claims_reads_plain_array() {
        let json = r#"[{"statement":"S","conditions":["c1"],"referenced_entities":["E"],"normativity":"contract"}]"#;
        let claims = parse_claims(json).expect("parsed");
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].statement, "S");
        assert_eq!(claims[0].normativity, "contract");
        assert_eq!(claims[0].conditions, vec!["c1".to_string()]);
        // `source` defaults empty — the driver sets it post-parse.
        assert!(claims[0].source.is_empty());
    }

    #[test]
    fn parse_claims_tolerates_code_fences() {
        let wrapped = "```json\n[{\"statement\":\"S\",\"conditions\":[],\"referenced_entities\":[],\"normativity\":\"proposal\"}]\n```";
        let claims = parse_claims(wrapped).expect("parsed");
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].normativity, "proposal");
    }

    #[test]
    fn parse_claims_handles_empty_array_and_garbage() {
        assert_eq!(parse_claims("[]").unwrap().len(), 0);
        assert!(parse_claims("not json at all").is_none());
    }
}
