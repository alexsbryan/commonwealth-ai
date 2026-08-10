// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich promote <corpus> --phase <id> --run <path>
//!  --finding <id> --type positive|corrected|negative --rationale <text>`
//!
//! Lifts a specific finding from a run output JSON into the exemplar
//! bank for the named phase. The per-phase input/output shapes are
//! phase-specific (chapter_id for phase 1, cluster_id for phase 3,
//! etc); we extract them from the run file by primary key.

use std::path::PathBuf;

use corpus_engine::enrichment::pipeline::{Exemplar, ExemplarBank, ExemplarKind, PipelinePhase};

use super::config::EnrichConfig;
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn enrich promote",
    summary: "Lift a run finding into the per-phase exemplar bank.",
    sections: &[
        HelpSection::Usage(
            "svrn enrich promote <corpus-id> --phase <phase-id> --run <path> \\\n  --finding <id> --type <positive|corrected|negative> --rationale <text> \\\n  [--selector <text>] [--model-output <inline-json>]",
        ),
        HelpSection::Flags(&[
            ("--phase <id>", "Which phase's bank to append to (questions | concerns | positions | tensions | gaps)."),
            ("--run <path>", "Path to a run output JSON file under runs/."),
            ("--finding <id>", "The item id inside the run to promote (chapter_id, concern_id, etc)."),
            ("--type <kind>", "positive = authored target. corrected = model was wrong, user fixed. negative = reject."),
            ("--rationale <text>", "Why this exemplar matters. Required."),
            ("--selector <text>", "Text used to embed this exemplar for similarity selection. Auto-derived if absent."),
            ("--model-output <json>", "Inline JSON of what the model produced (for corrected + negative exemplars)."),
        ]),
        HelpSection::Notes(
            "The exemplar bank is hand-editable JSON at \
             ~/.svrnmesh/enrichment/<corpus>/exemplars/<phase-id>.json. `promote` is the \
             convenience path — you can also edit the file directly.",
        ),
    ],
};

pub async fn cmd_promote(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };
    if let Err(e) = EnrichConfig::require(&parsed.corpus_id) {
        eprintln!("error: {e}");
        return 1;
    }

    // Read + parse the run file.
    let raw = match std::fs::read_to_string(&parsed.run) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: reading run file {}: {e}", parsed.run.display());
            return 1;
        }
    };
    let run_json: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: run file is not valid JSON: {e}");
            return 1;
        }
    };

    // Pull the matching finding.
    let (input, output) = match extract_finding(&parsed.phase, &run_json, &parsed.finding) {
        Ok(x) => x,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 1;
        }
    };

    // Build the Exemplar. Output vs corrected_output vs model_output
    // rules depend on --type.
    let selector_text = parsed.selector.clone().or_else(|| {
        // Default selector: stringified input's first string-valued field
        // (chapter title, concern_text, position_text).
        input.as_object().and_then(|o| {
            o.iter()
                .find_map(|(_, v)| v.as_str().map(|s| s.to_string()))
        })
    });

    let (output_field, model_output_field, corrected_output_field) = match parsed.kind {
        ExemplarKind::Positive => (Some(output), None, None),
        ExemplarKind::Corrected => {
            let model_out = match parsed.model_output.clone() {
                Some(m) => Some(m),
                None => Some(output.clone()),
            };
            (None, model_out, Some(output))
        }
        ExemplarKind::Negative => {
            let model_out = parsed
                .model_output
                .clone()
                .unwrap_or_else(|| output.clone());
            (None, Some(model_out), None)
        }
    };

    let exemplar = Exemplar {
        id: next_exemplar_id(&parsed.corpus_id, parsed.phase, &parsed.finding),
        kind: parsed.kind,
        input,
        output: output_field,
        model_output: model_output_field,
        corrected_output: corrected_output_field,
        rationale: parsed.rationale.clone(),
        selector_text,
        created_at: String::new(),
        facet: None,
    };

    let path = paths::exemplars_dir(&parsed.corpus_id).join(format!("{}.json", parsed.phase.id()));
    let mut bank = match ExemplarBank::open(&path, parsed.phase) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: opening bank {}: {e}", path.display());
            return 1;
        }
    };
    bank.append(exemplar);
    if let Err(e) = bank.save() {
        eprintln!("error: saving bank {}: {e}", path.display());
        return 1;
    }
    println!(
        "  ✓ appended {} exemplar to {} (now {} total)",
        kind_label(parsed.kind),
        path.display(),
        bank.len()
    );
    0
}

fn kind_label(k: ExemplarKind) -> &'static str {
    match k {
        ExemplarKind::Positive => "positive",
        ExemplarKind::Corrected => "corrected",
        ExemplarKind::Negative => "negative",
    }
}

/// Build a stable-looking exemplar id: `<phase>.<finding>.<ordinal>`
/// where ordinal disambiguates repeated promotions of the same
/// finding. Simpler than scanning the bank — we use timestamp
/// fallback if the bank load failed earlier.
fn next_exemplar_id(corpus_id: &str, phase: PipelinePhase, finding: &str) -> String {
    let bank_path = paths::exemplars_dir(corpus_id).join(format!("{}.json", phase.id()));
    let count = ExemplarBank::open(&bank_path, phase)
        .map(|b| b.len())
        .unwrap_or(0);
    let slug: String = finding
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .take(32)
        .collect();
    format!("ex_{}_{:03}_{}", phase.id(), count + 1, slug)
}

/// Locate the finding inside a phase's run output by primary key.
/// Returns `(input, output)` — the raw JSON pieces that go into the
/// Exemplar.
fn extract_finding(
    phase: &PipelinePhase,
    run: &serde_json::Value,
    finding_id: &str,
) -> Result<(serde_json::Value, serde_json::Value), String> {
    use serde_json::json;
    match phase {
        PipelinePhase::Questions => {
            let arr = run
                .get("questions_by_chapter")
                .and_then(|v| v.as_array())
                .ok_or_else(|| "run file missing `questions_by_chapter`".to_string())?;
            let entry = arr
                .iter()
                .find(|e| e.get("chapter_id").and_then(|s| s.as_str()) == Some(finding_id))
                .ok_or_else(|| format!("no chapter with id '{finding_id}' in run"))?;
            let questions = entry.get("questions").cloned().unwrap_or_else(|| json!([]));
            let reveals = entry.get("reveals").cloned();
            let thematic = entry
                .get("thematic_carriers")
                .cloned()
                .unwrap_or_else(|| json!([]));
            let mut out = serde_json::Map::new();
            out.insert("questions".into(), questions);
            if let Some(r) = reveals {
                out.insert("reveals".into(), r);
            }
            out.insert("thematic_carriers".into(), thematic);
            let input = json!({
                "chapter_id": finding_id,
                "title": entry.get("title").cloned().unwrap_or_else(|| json!("")),
            });
            Ok((input, serde_json::Value::Object(out)))
        }
        PipelinePhase::Concerns => extract_by_id_field(
            run,
            "concerns",
            finding_id,
            &["concern_text", "scope", "primary_arcs"],
        ),
        PipelinePhase::Positions => extract_by_id_field(
            run,
            "positions",
            finding_id,
            &["position_text", "grounding", "extensions"],
        ),
        PipelinePhase::Tensions => extract_by_id_field(
            run,
            "tensions",
            finding_id,
            &["description", "specific_disagreement", "structural_type"],
        ),
        PipelinePhase::Gaps => extract_by_id_field(
            run,
            "gaps",
            finding_id,
            &["gap_text", "evidence", "significance"],
        ),
        other => Err(format!(
            "phase '{}' is not promotable (no LLM-shaped output)",
            other.id()
        )),
    }
}

fn extract_by_id_field(
    run: &serde_json::Value,
    array_key: &str,
    finding_id: &str,
    output_keys: &[&str],
) -> Result<(serde_json::Value, serde_json::Value), String> {
    use serde_json::{json, Map, Value};
    let arr = run
        .get(array_key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("run file missing `{array_key}`"))?;
    let entry = arr
        .iter()
        .find(|e| e.get("id").and_then(|s| s.as_str()) == Some(finding_id))
        .ok_or_else(|| format!("no item with id '{finding_id}' in `{array_key}`"))?;
    let mut output = Map::new();
    for k in output_keys {
        if let Some(v) = entry.get(*k) {
            output.insert((*k).to_string(), v.clone());
        }
    }
    let input = json!({ "id": finding_id });
    Ok((input, Value::Object(output)))
}

#[derive(Debug)]
struct ParsedPromote {
    corpus_id: String,
    phase: PipelinePhase,
    run: PathBuf,
    finding: String,
    kind: ExemplarKind,
    rationale: String,
    selector: Option<String>,
    model_output: Option<serde_json::Value>,
}

fn parse_args(args: &[String]) -> Result<ParsedPromote, String> {
    let mut corpus_id: Option<String> = None;
    let mut phase: Option<PipelinePhase> = None;
    let mut run: Option<PathBuf> = None;
    let mut finding: Option<String> = None;
    let mut kind: Option<ExemplarKind> = None;
    let mut rationale: Option<String> = None;
    let mut selector: Option<String> = None;
    let mut model_output: Option<serde_json::Value> = None;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--phase" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--phase requires a value".to_string())?;
                phase = Some(
                    v.parse::<PipelinePhase>()
                        .map_err(|e| format!("--phase: {e}"))?,
                );
                i += 2;
            }
            "--run" => {
                run = Some(PathBuf::from(
                    args.get(i + 1).ok_or("--run requires a path".to_string())?,
                ));
                i += 2;
            }
            "--finding" => {
                finding = Some(
                    args.get(i + 1)
                        .ok_or("--finding requires a value".to_string())?
                        .clone(),
                );
                i += 2;
            }
            "--type" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--type requires a value".to_string())?;
                kind = Some(parse_kind(v)?);
                i += 2;
            }
            "--rationale" => {
                rationale = Some(
                    args.get(i + 1)
                        .ok_or("--rationale requires text".to_string())?
                        .clone(),
                );
                i += 2;
            }
            "--selector" => {
                selector = Some(
                    args.get(i + 1)
                        .ok_or("--selector requires text".to_string())?
                        .clone(),
                );
                i += 2;
            }
            "--model-output" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--model-output requires JSON".to_string())?;
                model_output = Some(
                    serde_json::from_str::<serde_json::Value>(v)
                        .map_err(|e| format!("--model-output is not JSON: {e}"))?,
                );
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                    i += 1;
                } else {
                    return Err(format!("unexpected positional: {other}"));
                }
            }
        }
    }
    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    let phase = phase.ok_or_else(|| "missing --phase".to_string())?;
    let run = run.ok_or_else(|| "missing --run".to_string())?;
    let finding = finding.ok_or_else(|| "missing --finding".to_string())?;
    let kind = kind.ok_or_else(|| "missing --type".to_string())?;
    let rationale = rationale
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing non-empty --rationale".to_string())?;
    Ok(ParsedPromote {
        corpus_id,
        phase,
        run,
        finding,
        kind,
        rationale,
        selector,
        model_output,
    })
}

fn parse_kind(s: &str) -> Result<ExemplarKind, String> {
    match s {
        "positive" | "pos" => Ok(ExemplarKind::Positive),
        "corrected" | "cor" => Ok(ExemplarKind::Corrected),
        "negative" | "neg" => Ok(ExemplarKind::Negative),
        other => Err(format!(
            "--type must be 'positive', 'corrected', or 'negative' (got '{other}')"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_promote_minimum() {
        let args: Vec<String> = [
            "ak",
            "--phase",
            "questions",
            "--run",
            "/x/r.json",
            "--finding",
            "sec_0001",
            "--type",
            "positive",
            "--rationale",
            "why",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let p = parse_args(&args).unwrap();
        assert_eq!(p.corpus_id, "ak");
        assert_eq!(p.phase, PipelinePhase::Questions);
        assert_eq!(p.finding, "sec_0001");
        assert!(matches!(p.kind, ExemplarKind::Positive));
    }

    #[test]
    fn parse_promote_rejects_bad_type() {
        let args: Vec<String> = [
            "ak",
            "--phase",
            "questions",
            "--run",
            "/x",
            "--finding",
            "s",
            "--type",
            "other",
            "--rationale",
            "why",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let err = parse_args(&args).unwrap_err();
        assert!(err.contains("--type"));
    }

    #[test]
    fn parse_promote_requires_non_empty_rationale() {
        let args: Vec<String> = [
            "ak",
            "--phase",
            "questions",
            "--run",
            "/x",
            "--finding",
            "s",
            "--type",
            "positive",
            "--rationale",
            "   ",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let err = parse_args(&args).unwrap_err();
        assert!(err.contains("rationale"));
    }

    #[test]
    fn extract_finding_for_phase1() {
        let run = serde_json::json!({
            "questions_by_chapter": [
                {"chapter_id":"sec_0001","title":"Ch 1","questions":["q1"],"reveals":"r","thematic_carriers":["A"]},
                {"chapter_id":"sec_0002","title":"Ch 2","questions":["q2"]}
            ]
        });
        let (inp, out) = extract_finding(&PipelinePhase::Questions, &run, "sec_0001").unwrap();
        assert_eq!(inp["chapter_id"], "sec_0001");
        assert!(out["questions"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("q1")));
        assert_eq!(out["reveals"], "r");
    }

    #[test]
    fn extract_finding_phase3_by_id() {
        let run = serde_json::json!({
            "concerns": [
                {"id":"cc_01","cluster_id":"qc_01","concern_text":"hmm","scope":"novel-wide","primary_arcs":["X"]}
            ]
        });
        let (inp, out) = extract_finding(&PipelinePhase::Concerns, &run, "cc_01").unwrap();
        assert_eq!(inp["id"], "cc_01");
        assert_eq!(out["concern_text"], "hmm");
    }

    #[test]
    fn extract_finding_errors_on_unknown_id() {
        let run = serde_json::json!({"concerns": []});
        let err = extract_finding(&PipelinePhase::Concerns, &run, "cc_01").unwrap_err();
        assert!(err.contains("cc_01"));
    }

    #[test]
    fn extract_finding_rejects_non_promotable_phase() {
        let err = extract_finding(
            &PipelinePhase::QuestionClusters,
            &serde_json::json!({}),
            "qc_01",
        )
        .unwrap_err();
        assert!(err.contains("not promotable"));
    }

    #[test]
    fn next_exemplar_id_has_phase_and_ordinal() {
        let id = next_exemplar_id("nonexistent-corpus", PipelinePhase::Questions, "sec_0001");
        assert!(id.starts_with("ex_questions_"));
        assert!(id.ends_with("sec_0001"));
    }

    #[test]
    fn hold_path_compiles() {
        // Smoke check that Path handling works.
        let _: &std::path::Path = std::path::Path::new("/x");
    }
}
