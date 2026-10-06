// SPDX-License-Identifier: AGPL-3.0-or-later
//! judge-funnel-gate — ONE place builds a forced-choice request body, and
//! every caller of it hands the request to a census.
//!
//! # The blindness this ends
//!
//! Counted at HEAD `8750c0442` on 2026-09-12: THREE judge-side sites built
//! their own `x_forced_choice` request body — `grounding/judge.rs`,
//! `bench_cmd/live_runner.rs` (its own copy) and `runtime/evidence_loop`
//! (inline, no function, its own parse). Only the first reached
//! `grounding::call_census::gate_call`, so two thirds of the judge traffic
//! this system issues was invisible to the census that prices judge cost and
//! attributes judge failure — and the header of `judge.rs` claimed the bench
//! copy was byte-identical, a claim that was true only while nobody edited one
//! side. (It had already gone stale once, on the claim-extraction register:
//! production grew an `entity_anchored` branch the bench copy never got, so
//! tau was calibrated on a prompt production does not send. Measured
//! 2026-08-19.)
//!
//! Rung `vl-1` of `quality/campaigns/verifier-loop.toml`, bar
//! `vl-one-primitive`.
//!
//! # Two subsystems, one constructor (2026-10-06)
//!
//! Until 2026-10-06 the one site was `grounding/judge.rs`, and rule 2 asked
//! that its own file call `gate_call`. RESOLVE in ingest now asks forced
//! choices too, and ingest cannot reach the grounding gate's per-turn census.
//! So the body moved down to the wire contract, beside the detector that reads
//! it: `oicp_types::forced_choice::schema` is the one construction site, and
//! rule 2 binds its CALLERS: every file that calls the constructor must call a
//! census funnel ([`FUNNELS`]: the grounding gate's `gate_call`, RESOLVE's
//! `decision_call`). Two censuses for two subsystems, for the reason
//! `call_census.rs` gives for not merging into the stage ledger: they answer
//! different questions (which call in a turn; which decision in a document).
//!
//! # The rule, and why it is two counts rather than one
//!
//! A single "sites == 1" count passes the day someone writes a second body in
//! the same file. A single "everything reaches the funnel" count passes the
//! day two files each build a body and each call `gate_call` — one register
//! with two spellings, which is how the last drift happened. So:
//!
//!   1. **construction sites == 1** — a non-comment, non-test line declaring
//!      the sentinel (`"x_forced_choice": true`). Reading the sentinel is not
//!      constructing one, which is why `CompletionRequest::
//!      forced_choice_candidates` (the detector) and every doc comment naming
//!      the key fall out of the census by the rule rather than by an exception.
//!   2. **callers of the constructor whose file never calls a funnel == 0** —
//!      a body issued where no census is is a call the census cannot see.
//!
//! # Scope, and the one exception, which lives at the site
//!
//! `src/` of every workspace member. `tests/`, `examples/` and `#[cfg(test)]`
//! modules are skipped for the reason `lifecycle-gate` skips them: neither
//! reaches a shipped artifact, and the engine's own sentinel tests
//! legitimately build the shape they are testing.
//!
//! One live site is neither a gate call nor a caller of the primitive:
//! `bench_cmd/mechanism_fidelity.rs`, the harness that measures whether a model
//! honours the sentinel at all. An instrument that measures a mechanism must
//! build the wire shape itself or it measures its own caller (ARCH principle 7).
//! That exception is carried as a MARKER COMMENT at the site —
//! `judge-funnel: instrument-of-the-mechanism` — not as a list in this file:
//! a reader of that code sees the exception, the gate reports the count
//! separately rather than dropping it silently (ARCH principle 6), and it
//! cannot drift out of sight the way an authored allow-list does.

use std::path::Path;

use crate::common;

/// The marker a site carries when it builds the wire shape ON PURPOSE, because
/// it is measuring the mechanism rather than using it.
const INSTRUMENT_MARKER: &str = "judge-funnel: instrument-of-the-mechanism";

/// The sentinel key, as it appears in a constructed body.
const SENTINEL: &str = "x_forced_choice";

/// The one constructor every caller builds its body with.
const CONSTRUCTOR: &str = "forced_choice::schema(";

/// The census funnels a caller's file must issue its calls through.
const FUNNELS: &[&str] = &["gate_call(", "decision_call("];

#[derive(Debug, PartialEq, Eq)]
struct Site {
    file: String,
    line: usize,
    /// Declared as measuring the mechanism rather than using it.
    instrument: bool,
}

pub fn run(args: &[String]) -> i32 {
    if args
        .iter()
        .any(|a| a == "--update-baseline" || a == "--tighten")
    {
        eprintln!(
            "judge-funnel-gate has no baseline, deliberately: its target is ONE, and a \
             ratchet that can be raised to two is a second register with a green gate over \
             it. The exception lives at the site as a `{INSTRUMENT_MARKER}` comment."
        );
        return 1;
    }

    let root = common::repo_root();
    let scope = match common::SourceTree::discover(&root) {
        Ok(s) => s,
        Err(e) => {
            // Could-not-judge, not fail: the instrument could not reach its
            // evidence, which is a different fact from a dirty tree.
            eprintln!("judge-funnel-gate: {e}");
            return 3;
        }
    };

    let members = crate::manifests::workspace_members(&root);
    let mut found = Found::default();
    for m in &members {
        let src = root.join(&m.dir).join("src");
        collect(&src, &root, &scope, &mut found);
    }
    let Found {
        sites,
        callers,
        files_read,
        funnel_files,
    } = found;

    if files_read == 0 {
        // Never-ran, not passed. A walk that read nothing renders exactly like
        // a clean tree (ARCH principle 6).
        eprintln!(
            "judge-funnel-gate: NOTHING WAS CENSUSED — {} workspace member(s) resolved and no \
             .rs file was read. This run makes no claim.",
            members.len()
        );
        return 4;
    }

    let used: Vec<&Site> = sites.iter().filter(|s| !s.instrument).collect();
    let instruments: Vec<&Site> = sites.iter().filter(|s| s.instrument).collect();
    let unfunnelled: Vec<&Site> = callers
        .iter()
        .filter(|s| !funnel_files.iter().any(|f| *f == s.file))
        .collect();

    eprintln!(
        "judge-funnel-gate: {files_read} file(s) censused across {} workspace member(s) \
         ({} ignored dir(s) skipped)",
        members.len(),
        scope.ignored_dir_count()
    );
    for s in &used {
        eprintln!("  builds a forced-choice body  {}:{}", s.file, s.line);
    }
    for s in &callers {
        let funnelled = if unfunnelled.iter().any(|u| *u == s) {
            "NOT handed to a census"
        } else {
            "reaches a census"
        };
        eprintln!(
            "  calls the constructor        {}:{}  — {funnelled}",
            s.file, s.line
        );
    }
    for s in &instruments {
        eprintln!(
            "  instrument-of-the-mechanism  {}:{}  — declared at the site, not counted",
            s.file, s.line
        );
    }
    eprintln!(
        "  construction sites: {} (target 1) · callers not reaching a census: {} (target 0)",
        used.len(),
        unfunnelled.len()
    );

    if used.len() == 1 && unfunnelled.is_empty() {
        eprintln!("  ✓ one register, and it is the one the census sees");
        return 0;
    }

    eprintln!();
    if used.len() != 1 {
        eprintln!(
            "FAIL: {} sites construct an `{SENTINEL}` request body. There may be ONE: build \
             the body with `oicp_types::forced_choice::schema` and issue it through a census \
             (`sovereign_core::runtime::forced_choice_ab` for a judge). If the site is \
             measuring the sentinel rather than using it, say so at the site with a \
             `{INSTRUMENT_MARKER}` comment and the reason.",
            used.len()
        );
    }
    for s in &unfunnelled {
        eprintln!(
            "FAIL: {}:{} calls the constructor in a file that calls no census ({}) — every \
             call it issues is one no census can see, which is the exact blindness this gate \
             exists to end.",
            s.file,
            s.line,
            FUNNELS.join(", ")
        );
    }
    // Deliberately NOT `common::fix_footer` — this gate has no baseline to
    // update, and offering one would name a command that refuses.
    eprintln!(
        "Fix: build the body with `oicp_types::forced_choice::schema` and issue it through a \
         census funnel, or declare a measuring site with `{INSTRUMENT_MARKER}` and the reason."
    );
    1
}

/// What one walk found: construction sites, constructor calls, the files
/// that call a census funnel, and how many files it read.
#[derive(Default)]
struct Found {
    sites: Vec<Site>,
    callers: Vec<Site>,
    files_read: usize,
    funnel_files: Vec<String>,
}

fn collect(dir: &Path, root: &Path, scope: &common::SourceTree, found: &mut Found) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let rel = common::rel_path(&path, root);
        if path.is_dir() {
            if !scope.excludes_dir(&rel) && path.file_name().is_some_and(|n| n != "tests") {
                collect(&path, root, scope, found);
            }
            continue;
        }
        if path.extension().is_some_and(|e| e == "rs") && !rel.ends_with("/tests.rs") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                found.files_read += 1;
                if FUNNELS.iter().any(|f| text.contains(f)) {
                    found.funnel_files.push(rel.clone());
                }
                scan(&rel, &text, &mut found.sites, &mut found.callers);
            }
        }
    }
}

/// Scan one file for construction sites and constructor calls, skipping
/// comments and `#[cfg(test)]` modules. The marker may sit on the site's own
/// line or in the comment block directly above it — where a reason belongs.
fn scan(rel: &str, text: &str, out: &mut Vec<Site>, callers: &mut Vec<Site>) {
    let mut test_depth: Option<i32> = None;
    let mut pending_test_mod = false;
    let mut marker_pending = false;
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();

        if let Some(depth) = test_depth.as_mut() {
            *depth += line.matches('{').count() as i32;
            *depth -= line.matches('}').count() as i32;
            if *depth <= 0 {
                test_depth = None;
            }
            continue;
        }
        if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("#[cfg(all(test") {
            pending_test_mod = true;
            continue;
        }
        if pending_test_mod {
            if trimmed.starts_with("mod ") && line.contains('{') {
                let d = line.matches('{').count() as i32 - line.matches('}').count() as i32;
                if d > 0 {
                    test_depth = Some(d);
                }
                pending_test_mod = false;
                continue;
            }
            pending_test_mod = false;
        }

        // The marker governs the contiguous block it opens, up to the next
        // blank line — so the REASON can be the paragraph above the body,
        // which is where a reader looks for it, rather than a suffix crammed
        // onto the sentinel's own line.
        if line.contains(INSTRUMENT_MARKER) {
            marker_pending = true;
        }
        if trimmed.is_empty() {
            marker_pending = false;
        }
        // A comment naming the sentinel does not construct one — and the
        // module docs of four files in this tree are exactly that.
        if trimmed.starts_with("//") {
            continue;
        }
        if is_construction(line) {
            out.push(Site {
                file: rel.to_string(),
                line: i + 1,
                instrument: marker_pending,
            });
        }
        // A call, not the definition and not a string naming it (this gate's own).
        if line.contains(CONSTRUCTOR)
            && !line.contains("fn schema(")
            && !line.contains(&format!("\"{CONSTRUCTOR}"))
        {
            callers.push(Site {
                file: rel.to_string(),
                line: i + 1,
                instrument: false,
            });
        }
    }
}

/// Is this line DECLARING the sentinel, as opposed to reading it?
///
/// `"x_forced_choice": true` in a body under construction; not
/// `so.get("x_forced_choice")`, which is the detector, and not the engine's
/// `!= Some(true)` comparison against it.
fn is_construction(line: &str) -> bool {
    let Some(at) = line.find(SENTINEL) else {
        return false;
    };
    let rest = &line[at + SENTINEL.len()..];
    let rest = rest.trim_start_matches(['"', '\'']).trim_start();
    let Some(after_colon) = rest.strip_prefix(':') else {
        return false;
    };
    after_colon.trim_start().starts_with("true")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(text: &str) -> Vec<(usize, bool)> {
        let (mut out, mut callers) = (Vec::new(), Vec::new());
        scan("x.rs", text, &mut out, &mut callers);
        out.iter().map(|s| (s.line, s.instrument)).collect()
    }

    fn calls(text: &str) -> Vec<usize> {
        let (mut out, mut callers) = (Vec::new(), Vec::new());
        scan("x.rs", text, &mut out, &mut callers);
        callers.iter().map(|s| s.line).collect()
    }

    /// A caller of the constructor is found wherever it is live, and neither
    /// prose about it nor a test module counts. The run then holds each
    /// caller's file to a census funnel.
    #[test]
    fn a_call_to_the_constructor_is_a_caller_and_prose_or_tests_are_not() {
        let text = r#"
//! Built by `oicp_types::forced_choice::schema(&labels)`.
fn ask() {
    let s = oicp_types::forced_choice::schema(&["A", "B"]);
}

#[cfg(test)]
mod tests {
    fn t() { let _ = forced_choice::schema(&["A"]); }
}
"#;
        assert_eq!(calls(text), [4]);
        assert!(calls("pub fn schema(labels: &[&str]) -> serde_json::Value {").is_empty());
    }

    /// The body under construction fires; the detector reading the same key
    /// does not. Both shapes are live in this tree — `judge.rs:119` and
    /// `shared/crates/oicp-types/src/completion.rs:464` — so a rule that could not tell them
    /// apart would either red the detector or need an exception for it.
    #[test]
    fn a_declared_sentinel_is_a_site_and_a_read_one_is_not() {
        assert!(is_construction(
            r#"            "type": "string", "enum": ["A", "B"], "x_forced_choice": true"#
        ));
        assert!(is_construction(r#"        "x_forced_choice": true"#));
        assert!(!is_construction(
            r#"        if so.get("x_forced_choice").and_then(|v| v.as_bool()) != Some(true) {"#
        ));
        assert!(!is_construction(
            r#"        r.structured_output = Some(serde_json::json!({ "x_forced_choice": false }));"#
        ));
    }

    /// THE FAILING INPUT THIS GATE EXISTS FOR (ARCH principle 5): a second
    /// register grown beside the first. It is how the claim-extraction
    /// register drifted in 2026-08, and it is what the gate must catch.
    #[test]
    fn a_second_register_grown_beside_the_first_is_a_site() {
        let planted = r#"
async fn my_own_judge(inference: &dyn InferenceProvider, prompt: &str) -> Option<f64> {
    let req = CompletionRequest {
        prompt: prompt.to_string(),
        max_tokens: Some(1),
        structured_output: Some(serde_json::json!({
            "type": "string", "enum": ["A", "B"], "x_forced_choice": true
        })),
        ..Default::default()
    };
    inference.complete(&req).await.ok()
}
"#;
        assert_eq!(hits(planted), [(7, false)]);
    }

    /// Prose about the sentinel is not a body. Four files in this tree name the
    /// key in their module docs, and a census that read them would be one
    /// people silence.
    #[test]
    fn comments_are_not_construction_sites() {
        assert!(hits(r#"//! `{"x_forced_choice": true, "enum": [...]}` opts in."#).is_empty());
        assert!(hits(r#"    /// the `"x_forced_choice": true` sentinel"#).is_empty());
    }

    /// The engine's own tests build the shape they are testing. `gates.rs`,
    /// `grammar.rs` and `rpc_distribution.rs` each do, and all three are inside
    /// `#[cfg(test)] mod tests` — which is why the engine needs no path
    /// exception in this gate.
    #[test]
    fn cfg_test_modules_are_skipped_and_the_scan_resumes_after_them() {
        let text = r#"
fn production() {}

#[cfg(test)]
mod tests {
    fn t() {
        let r = json!({ "x_forced_choice": true });
        if true { let _ = 1; }
    }
}

fn after() {
    let r = json!({ "x_forced_choice": true });
}
"#;
        // Only the site after the module, at its exact line (the raw string
        // opens with a newline, so `after`'s body is line 13). Pinned exactly
        // rather than "some line": a depth tracker that exited the module one
        // brace early would still find one site, at a different line.
        assert_eq!(hits(text), [(13, false)]);
    }

    /// The exception is a marker at the site, and it reads from the comment
    /// block above the body — where the reason belongs. The live case
    /// (`mechanism_fidelity.rs`) has four lines of reason and a multi-line
    /// `json!` between the marker and the sentinel, so a rule that only looked
    /// at the previous line would have reported it as an undeclared second
    /// register.
    #[test]
    fn the_marker_governs_the_block_it_opens_and_the_site_is_still_reported() {
        let text = r#"
fn probe() {
    // judge-funnel: instrument-of-the-mechanism — this harness measures
    // whether a model honours the sentinel, so it must build the wire shape.
    let schema = serde_json::json!({
        "type": "string",
        "enum": candidates,
        "x_forced_choice": true
    });
}

fn other() {
    let schema = serde_json::json!({ "x_forced_choice": true });
}
"#;
        assert_eq!(hits(text), [(8, true), (13, false)]);
    }

    /// A blank line closes the block. Without this the marker would govern the
    /// rest of the file, and one declared instrument would silence every site
    /// written after it.
    #[test]
    fn a_blank_line_ends_the_markers_reach() {
        let text = r#"
// judge-funnel: instrument-of-the-mechanism — declared for the probe below.
let a = json!({ "x_forced_choice": true });

let b = json!({ "x_forced_choice": true });
"#;
        assert_eq!(hits(text), [(3, true), (5, false)]);
    }
}
