//! `sovereign enrich diff <corpus> <run-a> <run-b>` — side-by-side
//! diff of two phase 1 run output files so the developer can see what
//! changed between exemplar iterations.
//!
//! Landing 4 supports phase 1 (chapter → questions); phase 3+ diffs
//! land in a follow-up.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use corpus_engine::enrichment::pipeline::{ExtractedQuestion, Phase1Output};

use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich diff",
    summary: "Side-by-side compare two phase 1 run output files.",
    sections: &[
        HelpSection::Usage("sovereign enrich diff <corpus-id> <run-a.json> <run-b.json>"),
        HelpSection::Notes(
            "Diff reports per-chapter added / removed / changed questions. Phase 3+ diffs land \
             in a follow-up iteration.",
        ),
    ],
};

pub async fn cmd_diff(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    let (corpus_id, a, b) = match parse_args(args) {
        Ok(x) => x,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };
    let _ = corpus_id; // currently unused — reserved for future cache-aware modes
    let left: Phase1Output = match load_phase1(&a) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("error: loading {}: {e}", a.display());
            return 1;
        }
    };
    let right: Phase1Output = match load_phase1(&b) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("error: loading {}: {e}", b.display());
            return 1;
        }
    };

    let diff = diff_phase1(&left, &right);
    print_report(&diff, &a, &b);
    0
}

fn load_phase1(path: &Path) -> Result<Phase1Output, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str::<Phase1Output>(&raw).map_err(|e| e.to_string())
}

#[derive(Debug, Default)]
struct Phase1Diff {
    only_left: Vec<String>,           // chapter ids present in a, missing in b
    only_right: Vec<String>,          // chapter ids present in b, missing in a
    changed: Vec<ChangedChapter>,     // chapters present in both but diverge
    unchanged: Vec<String>,           // identical
}

#[derive(Debug)]
struct ChangedChapter {
    chapter_id: String,
    added_questions: Vec<String>,
    removed_questions: Vec<String>,
    reveals_changed: bool,
    carriers_changed: bool,
}

fn diff_phase1(a: &Phase1Output, b: &Phase1Output) -> Phase1Diff {
    let a_map: BTreeMap<&str, &ExtractedQuestion> = a
        .questions_by_chapter
        .iter()
        .map(|q| (q.chapter_id.as_str(), q))
        .collect();
    let b_map: BTreeMap<&str, &ExtractedQuestion> = b
        .questions_by_chapter
        .iter()
        .map(|q| (q.chapter_id.as_str(), q))
        .collect();

    let mut out = Phase1Diff::default();
    for (id, ae) in &a_map {
        match b_map.get(id) {
            None => out.only_left.push((*id).to_string()),
            Some(be) => {
                let a_qs: std::collections::BTreeSet<&str> =
                    ae.questions.iter().map(|s| s.as_str()).collect();
                let b_qs: std::collections::BTreeSet<&str> =
                    be.questions.iter().map(|s| s.as_str()).collect();
                let added: Vec<String> =
                    b_qs.difference(&a_qs).map(|s| (*s).to_string()).collect();
                let removed: Vec<String> =
                    a_qs.difference(&b_qs).map(|s| (*s).to_string()).collect();
                let reveals_changed = ae.reveals != be.reveals;
                let carriers_changed = {
                    let a_c: std::collections::BTreeSet<&str> =
                        ae.thematic_carriers.iter().map(|s| s.as_str()).collect();
                    let b_c: std::collections::BTreeSet<&str> =
                        be.thematic_carriers.iter().map(|s| s.as_str()).collect();
                    a_c != b_c
                };
                if added.is_empty()
                    && removed.is_empty()
                    && !reveals_changed
                    && !carriers_changed
                {
                    out.unchanged.push((*id).to_string());
                } else {
                    out.changed.push(ChangedChapter {
                        chapter_id: (*id).to_string(),
                        added_questions: added,
                        removed_questions: removed,
                        reveals_changed,
                        carriers_changed,
                    });
                }
            }
        }
    }
    for id in b_map.keys() {
        if !a_map.contains_key(id) {
            out.only_right.push((*id).to_string());
        }
    }
    out
}

fn print_report(d: &Phase1Diff, a: &Path, b: &Path) {
    println!("Comparing:");
    println!("  A: {}", a.display());
    println!("  B: {}", b.display());
    println!();
    println!(
        "  {} unchanged, {} changed, {} only in A, {} only in B",
        d.unchanged.len(),
        d.changed.len(),
        d.only_left.len(),
        d.only_right.len()
    );
    if !d.only_left.is_empty() {
        println!();
        println!("  Only in A:");
        for id in &d.only_left {
            println!("    - {id}");
        }
    }
    if !d.only_right.is_empty() {
        println!();
        println!("  Only in B:");
        for id in &d.only_right {
            println!("    + {id}");
        }
    }
    if !d.changed.is_empty() {
        println!();
        println!("  Changed:");
        for c in &d.changed {
            println!("    ~ {}", c.chapter_id);
            for q in &c.removed_questions {
                println!("        - {q}");
            }
            for q in &c.added_questions {
                println!("        + {q}");
            }
            if c.reveals_changed {
                println!("        ~ reveals changed");
            }
            if c.carriers_changed {
                println!("        ~ thematic_carriers changed");
            }
        }
    }
}

fn parse_args(args: &[String]) -> Result<(String, PathBuf, PathBuf), String> {
    if args.is_empty() {
        return Err("missing <corpus-id> and two run file paths".into());
    }
    let mut positional: Vec<String> = Vec::new();
    for a in args {
        if a.starts_with("--") {
            return Err(format!("unknown flag: {a}"));
        }
        positional.push(a.clone());
    }
    if positional.len() != 3 {
        return Err(format!(
            "expected <corpus-id> <run-a.json> <run-b.json> (got {} positional arg(s))",
            positional.len()
        ));
    }
    let corpus_id = positional[0].clone();
    let a = PathBuf::from(&positional[1]);
    let b = PathBuf::from(&positional[2]);
    Ok((corpus_id, a, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::enrichment::pipeline::{ExtractedQuestion, Phase1Output};

    fn sample_phase1(pairs: &[(&str, &[&str], Option<&str>)]) -> Phase1Output {
        Phase1Output {
            schema_version: Phase1Output::SCHEMA_VERSION,
            pipeline_id: "literary".into(),
            questions_by_chapter: pairs
                .iter()
                .map(|(id, qs, reveals)| ExtractedQuestion {
                    chapter_id: (*id).to_string(),
                    questions: qs.iter().map(|q| (*q).to_string()).collect(),
                    reveals: reveals.map(|s| s.to_string()),
                    thematic_carriers: Vec::new(),
                    setting: None,
                    plot: None,
                    section_extraction: None,
                })
                .collect(),
            failures: Vec::new(),
            written_at: "t".into(),
        }
    }

    #[test]
    fn diff_detects_added_removed_changed_unchanged() {
        let a = sample_phase1(&[
            ("ch1", &["q1", "q2"], Some("r1")),
            ("ch2", &["alpha"], None),
            ("ch3", &["keep"], None),
        ]);
        let b = sample_phase1(&[
            ("ch1", &["q1", "q3"], Some("r1-updated")),
            ("ch3", &["keep"], None),
            ("ch4", &["newchap"], None),
        ]);
        let d = diff_phase1(&a, &b);
        assert_eq!(d.only_left, vec!["ch2"]);
        assert_eq!(d.only_right, vec!["ch4"]);
        assert_eq!(d.unchanged, vec!["ch3"]);
        assert_eq!(d.changed.len(), 1);
        let c = &d.changed[0];
        assert_eq!(c.chapter_id, "ch1");
        assert_eq!(c.added_questions, vec!["q3"]);
        assert_eq!(c.removed_questions, vec!["q2"]);
        assert!(c.reveals_changed);
    }

    #[test]
    fn parse_diff_args_requires_three_positional() {
        let err = parse_args(&["ak".into()]).unwrap_err();
        assert!(err.contains("positional"));
        let err = parse_args(&["ak".into(), "--foo".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }
}
