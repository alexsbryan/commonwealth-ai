// SPDX-License-Identifier: AGPL-3.0-or-later
//! Judge the llg-parity corpus under llama-server's llguidance (1.0.1).
//!
//!     llg-parity [--corpus <corpus.jsonl>] --out <scenario.jsonl>
//!
//! The corpus is written by the engine side
//! (serve/crates/sovereign-inference/src/llg_parity_corpus_tests.rs): per
//! grammar, the text the remote client sends llama-server and the engine's
//! accept/reject verdict for each input. Each server grammar is compiled as
//! llama-server compiles it (`llg_new_matcher(…, "lark", …)`, LS
//! common/llguidance.cpp:25, which is `TopLevelGrammar::from_lark`) and every
//! input is judged whole by the same rule as the engine side
//! (`llg_whole_string.rs`). One verdict per inventory row goes to `--out`:
//! passed when every input agrees, failed naming the divergent inputs,
//! could-not-judge when a grammar does not compile here or is not what
//! llama-server enforces.
//!
//! Exit 0 when every row passed, 3 when one failed, 4 when none could be
//! judged otherwise, 2 on a usage or input error.

use std::collections::BTreeMap;
use std::path::PathBuf;

use llguidance::{api::TopLevelGrammar, toktrie::ApproximateTokEnv, Matcher, ParserFactory};
use serde::Deserialize;
use serde_json::json;

/// The engine side's corpus, from this manifest: it lives in the crate that
/// writes it, which may read nothing outside itself.
const CORPUS: &str =
    "../../../../serve/crates/sovereign-inference/tests/fixtures/llg-parity/corpus.jsonl";

/// How many divergent inputs a failed verdict names.
const NAMED: usize = 3;

#[derive(Deserialize)]
struct Line {
    row: String,
    grammar_id: String,
    server_converter: String,
    server_grammar: String,
    #[serde(default)]
    server_note: Option<String>,
    inputs: Vec<Input>,
}

#[derive(Deserialize)]
struct Input {
    text: String,
    engine_accepts: bool,
}

/// One grammar compiled once, judged per string. The same rule as the
/// engine side's `WholeString::accepts`: every byte allowed in turn, a byte
/// past a stopped grammar refused, then accepting at the end.
struct WholeString {
    factory: ParserFactory,
    grammar: TopLevelGrammar,
}

impl WholeString {
    fn new(lark: &str) -> Result<Self, String> {
        let env = ApproximateTokEnv::single_byte_env();
        let factory = ParserFactory::new_simple(&env).map_err(|e| e.to_string())?;
        let grammar = TopLevelGrammar::from_lark(lark.to_string());
        let m = Matcher::new(factory.create_parser(grammar.clone()));
        if m.is_error() {
            return Err(m.get_error().unwrap_or_else(|| "unknown".into()));
        }
        Ok(Self { factory, grammar })
    }

    fn accepts(&self, text: &[u8]) -> Result<bool, String> {
        let mut m = Matcher::new(self.factory.create_parser(self.grammar.clone()));
        for &b in text {
            if m.is_stopped() {
                return Ok(false);
            }
            let mask = m.compute_mask().map_err(|e| e.to_string())?;
            if !mask.is_allowed(b as u32) {
                return Ok(false);
            }
            m.consume_token(b as u32).map_err(|e| e.to_string())?;
        }
        m.is_accepting().map_err(|e| e.to_string())
    }
}

/// What one grammar came to.
enum Outcome {
    /// Inputs judged, and the ones that disagree: (text, engine, server).
    Judged {
        total: usize,
        divergent: Vec<(String, bool, Result<bool, String>)>,
    },
    /// The server grammar does not compile under 1.0.1.
    NoCompile(String),
}

fn judge_line(line: &Line) -> Outcome {
    let g = match WholeString::new(&line.server_grammar) {
        Ok(g) => g,
        Err(e) => return Outcome::NoCompile(e),
    };
    let divergent = line
        .inputs
        .iter()
        .filter_map(|i| {
            let server = g.accepts(i.text.as_bytes());
            (server.as_ref().ok() != Some(&i.engine_accepts))
                .then(|| (i.text.clone(), i.engine_accepts, server))
        })
        .collect();
    Outcome::Judged {
        total: line.inputs.len(),
        divergent,
    }
}

fn describe(grammar_id: &str, d: &(String, bool, Result<bool, String>)) -> String {
    let server = match &d.2 {
        Ok(b) => b.to_string(),
        Err(e) => format!("error ({e})"),
    };
    format!("{grammar_id} {:?}: engine {}, server {server}", d.0, d.1)
}

/// A row's verdict from its grammars: failed if any input differs on a
/// grammar llama-server enforces through llguidance; could-not-judge if a
/// grammar did not compile or is not enforced through llguidance; passed
/// otherwise.
fn row_verdict(lines: &[(&Line, Outcome)]) -> (&'static str, String) {
    let mut failed = Vec::new();
    let mut unjudged = Vec::new();
    let (mut agree, mut total) = (0, 0);
    for (line, outcome) in lines {
        match outcome {
            Outcome::NoCompile(e) => unjudged.push(format!(
                "{}: the server grammar does not compile under llguidance 1.0.1: {e}",
                line.grammar_id
            )),
            Outcome::Judged {
                total: n,
                divergent,
            } if line.server_converter != "llguidance" => {
                let first = divergent
                    .first()
                    .map(|d| format!(", first {}", describe(&line.grammar_id, d)))
                    .unwrap_or_default();
                unjudged.push(format!(
                    "{}: {} of {n} agree under llguidance 1.0.1's %json form{first}",
                    line.grammar_id,
                    n - divergent.len()
                ));
            }
            Outcome::Judged {
                total: n,
                divergent,
            } => {
                total += n;
                agree += n - divergent.len();
                failed.extend(divergent.iter().map(|d| describe(&line.grammar_id, d)));
            }
        }
    }
    let grammars = lines.len();
    if !failed.is_empty() {
        let named: Vec<&str> = failed.iter().take(NAMED).map(String::as_str).collect();
        return (
            "failed",
            format!(
                "{} of {total} inputs over {grammars} grammars differ; {}",
                failed.len(),
                named.join("; ")
            ),
        );
    }
    if !unjudged.is_empty() {
        let note = lines
            .iter()
            .find_map(|(l, _)| l.server_note.as_deref())
            .map(|n| format!("{n} "))
            .unwrap_or_default();
        return ("could-not-judge", format!("{note}{}", unjudged.join("; ")));
    }
    (
        "passed",
        format!("{agree} of {total} inputs over {grammars} grammars agree"),
    )
}

fn run(corpus: PathBuf, out: PathBuf) -> Result<i32, String> {
    let text = std::fs::read_to_string(&corpus)
        .map_err(|e| format!("reading {}: {e}", corpus.display()))?;
    let lines: Vec<Line> = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(n, l)| {
            serde_json::from_str(l).map_err(|e| format!("{}:{}: {e}", corpus.display(), n + 1))
        })
        .collect::<Result<_, _>>()?;
    let mut by_row: BTreeMap<&str, Vec<(&Line, Outcome)>> = BTreeMap::new();
    for line in &lines {
        let outcome = judge_line(line);
        let summary = match &outcome {
            Outcome::NoCompile(e) => format!("does not compile: {e}"),
            Outcome::Judged { total, divergent } => {
                format!("{} of {total} agree", total - divergent.len())
            }
        };
        println!(
            "  {:<30} {:<28} {:<10} {summary}",
            line.row, line.grammar_id, line.server_converter
        );
        by_row.entry(&line.row).or_default().push((line, outcome));
    }
    println!();
    let mut body = String::new();
    let mut verdicts = Vec::new();
    for (row, lines) in &by_row {
        let (verdict, detail) = row_verdict(lines);
        println!("{verdict:<16} {row:<30} {detail}");
        body.push_str(&json!({"row": row, "verdict": verdict, "detail": detail}).to_string());
        body.push('\n');
        verdicts.push(verdict);
    }
    std::fs::write(&out, body).map_err(|e| format!("writing {}: {e}", out.display()))?;
    println!("\nwrote {}", out.display());
    Ok(if verdicts.contains(&"failed") {
        3
    } else if verdicts.contains(&"passed") {
        0
    } else {
        4
    })
}

fn main() {
    let mut corpus = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(CORPUS);
    let mut out = None;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match (arg.as_str(), it.next()) {
            ("--corpus", Some(v)) => corpus = v.into(),
            ("--out", Some(v)) => out = Some(PathBuf::from(v)),
            _ => {
                eprintln!("usage: llg-parity [--corpus <corpus.jsonl>] --out <scenario.jsonl>");
                std::process::exit(2);
            }
        }
    }
    let Some(out) = out else {
        eprintln!("usage: llg-parity [--corpus <corpus.jsonl>] --out <scenario.jsonl>");
        std::process::exit(2);
    };
    match run(corpus, out) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    }
}
