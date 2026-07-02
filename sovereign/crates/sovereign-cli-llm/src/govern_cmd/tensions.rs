// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn govern tensions` — the meeting agenda: open tensions,
//! ranked (open-first, confidence-desc by `build_view`), each with both
//! rule texts and a ready-to-run resolve command. Integrity issues are
//! surfaced, never hidden (glass-box).

use super::load_view;

pub fn cmd_tensions(args: &[String]) -> i32 {
    let mut corpus_id = None;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--format" => {
                json = args.get(i + 1).map(|s| s == "json").unwrap_or(false);
                i += 2;
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown flag {other}");
                return 2;
            }
            other => {
                corpus_id = Some(other.to_string());
                i += 1;
            }
        }
    }
    let Some(corpus_id) = corpus_id else {
        eprintln!("error: usage: sovereign govern tensions <corpus-id> [--format json]");
        return 2;
    };
    let view = match load_view(&corpus_id) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    let open: Vec<_> = view.open_tensions().collect();
    // JSON mode: emit the open tensions so a setup script can discover the
    // tension/rule ids (which vary per enrichment) and resolve reproducibly.
    if json {
        match serde_json::to_string_pretty(&open) {
            Ok(s) => {
                println!("{s}");
                return 0;
            }
            Err(e) => {
                eprintln!("error: serializing tensions: {e}");
                return 1;
            }
        }
    }
    println!(
        "=== govern tensions — {corpus_id} ({} open) ===",
        open.len()
    );
    if open.is_empty() {
        if view.rules.is_empty() {
            println!("  no governed rules yet — run `svrn govern seed {corpus_id}` first.");
        } else {
            println!("  no open tensions — current law is internally consistent (as detected).");
        }
    }
    // Glass-box agenda. For each flagged conflict, surface the RAW material a
    // human needs to *overrule the detector* — the verbatim source clause and
    // a traceable citation (the Article / dated Decision it comes from) — set
    // beside the model's paraphrase and reasoning, each labeled as the model's
    // read. The verdict is only ~0.67-precise; the source clause is exact, so
    // a reviewer adjudicates the real rules, and a paraphrase that drifted from
    // its source (e.g. "the house" narrowed to "any bedroom") stays visible
    // rather than hidden behind the verdict.
    let rule_by_id = view
        .rules
        .iter()
        .map(|r| (r.id.as_str(), r))
        .collect::<std::collections::HashMap<_, _>>();
    let titles = corpus_engine::enrichment::governance_view::section_titles(
        crate::enrich_cmd::paths::index_root(&corpus_id),
    );
    // (section title, verbatim source clause) for a rule id, from its citation.
    let cite = |rid: &str| -> (String, Option<String>) {
        match rule_by_id.get(rid).and_then(|r| r.citation.as_ref()) {
            Some(c) => (
                titles
                    .get(&c.chunk_id)
                    .cloned()
                    .unwrap_or_else(|| c.chunk_id.clone()),
                c.passage_preview.clone(),
            ),
            None => ("(unmapped section)".to_string(), None),
        }
    };

    for t in &open {
        println!();
        println!(
            "  conflict {}  (confidence {:.2})",
            t.id.as_str(),
            t.confidence
        );
        for (label, rid, paraphrase) in [
            ("A", t.rule_a.as_str(), &t.text_a),
            ("B", t.rule_b.as_str(), &t.text_b),
        ] {
            let (title, verbatim) = cite(rid);
            println!("    {label} · {title}  [{rid}]");
            if let Some(v) = verbatim {
                println!("        source: \"{}\"", v.trim());
            }
            println!("        model read it as: {paraphrase}");
        }
        if let Some(why) = &t.why {
            println!("    model's reasoning: {why}");
        }
        println!(
            "    → resolve: sovereign govern resolve {corpus_id} {} --keep <{}|{}>",
            t.id.as_str(),
            t.rule_a.as_str(),
            t.rule_b.as_str()
        );
    }

    // Glass-box: data-integrity findings the view surfaced.
    if !view.issues.is_empty() {
        println!();
        println!("  ⚠ {} integrity issue(s):", view.issues.len());
        for issue in &view.issues {
            println!("    {issue:?}");
        }
    }
    0
}
