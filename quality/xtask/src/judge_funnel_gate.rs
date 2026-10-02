// SPDX-License-Identifier: AGPL-3.0-or-later
//! judge-funnel-gate — ONE place builds a forced-choice judge request, and it
//! is the place that hands the request to the census.
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
//!   2. **sites whose file never calls `gate_call(` == 0** — a body built
//!      where the funnel is not is a judge call the census cannot see.
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

/// The funnel every constructed body must be handed to.
const FUNNEL: &str = "gate_call(";

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
    let mut sites: Vec<Site> = Vec::new();
    let mut files_read = 0usize;
    let mut funnel_files: Vec<String> = Vec::new();
    for m in &members {
        let src = root.join(&m.dir).join("src");
        collect(
            &src,
            &root,
            &scope,
            &mut sites,
            &mut files_read,
            &mut funnel_files,
        );
    }

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
    let unfunnelled: Vec<&&Site> = used
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
        let funnelled = if unfunnelled.iter().any(|u| ***u == **s) {
            "NOT handed to the funnel"
        } else {
            "reaches gate_call"
        };
        eprintln!(
            "  builds a forced-choice body  {}:{}  — {funnelled}",
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
        "  construction sites: {} (target 1) · not reaching the funnel: {} (target 0)",
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
            "FAIL: {} judge-side sites construct an `{SENTINEL}` request body. There may be \
             ONE. Call `sovereign_core::runtime::forced_choice_ab` — it takes the system \
             message, the routing (OICP envelope or a pinned slot) and the `JudgeCall` name, \
             which is every difference the three collapsed sites actually had. If the site is \
             measuring the sentinel rather than using it, say so at the site with a \
             `{INSTRUMENT_MARKER}` comment and the reason.",
            used.len()
        );
    }
    for s in &unfunnelled {
        eprintln!(
            "FAIL: {}:{} builds a forced-choice body in a file that never calls `{FUNNEL}` — \
             every call it issues is a judge call no census can see, which is the exact \
             blindness this gate exists to end.",
            s.file, s.line
        );
    }
    // Deliberately NOT `common::fix_footer` — this gate has no baseline to
    // update, and offering one would name a command that refuses.
    eprintln!(
        "Fix: collapse the extra site onto `sovereign_core::runtime::forced_choice_ab`, or \
         declare it at the site with `{INSTRUMENT_MARKER}` and the reason."
    );
    1
}

fn collect(
    dir: &Path,
    root: &Path,
    scope: &common::SourceTree,
    out: &mut Vec<Site>,
    files_read: &mut usize,
    funnel_files: &mut Vec<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let rel = common::rel_path(&path, root);
        if path.is_dir() {
            if !scope.excludes_dir(&rel) && path.file_name().is_some_and(|n| n != "tests") {
                collect(&path, root, scope, out, files_read, funnel_files);
            }
            continue;
        }
        if path.extension().is_some_and(|e| e == "rs") && !rel.ends_with("/tests.rs") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                *files_read += 1;
                if text.contains(FUNNEL) {
                    funnel_files.push(rel.clone());
                }
                scan(&rel, &text, out);
            }
        }
    }
}

/// Scan one file for construction sites, skipping comments and `#[cfg(test)]`
/// modules. The marker may sit on the site's own line or in the comment block
/// directly above it — where a reason belongs.
fn scan(rel: &str, text: &str, out: &mut Vec<Site>) {
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
        let mut out = Vec::new();
        scan("x.rs", text, &mut out);
        out.iter().map(|s| (s.line, s.instrument)).collect()
    }

    /// The body under construction fires; the detector reading the same key
    /// does not. Both shapes are live in this tree — `judge.rs:119` and
    /// `oicp-types/src/completion.rs:464` — so a rule that could not tell them
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
