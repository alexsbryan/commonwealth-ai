// SPDX-License-Identifier: AGPL-3.0-or-later
//! The llg-parity corpus (engine conformance battery, milestone 5): the
//! engine's accept/reject verdict for every input over the production
//! grammars and the allow-list grammars, beside the grammar text the remote
//! client sends llama-server. `bench/lanes/engine-swap/llg-parity` judges the
//! same inputs under the server's llguidance (1.0.1) and writes the scenario
//! verdicts the conformance judge reads.
//!
//! The corpus is a committed golden file: a change in the engine's verdicts
//! fails this test. `LLG_PARITY_UPDATE=1` rewrites it.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{json, Value};
use sovereign_contracts::decode_allowlist::{llguidance_lark, AllowlistLanguage, EVIDENCE_ID, URL};
use sovereign_contracts::structured_output::default_additional_properties_false;

use crate::decode_allowlist_parity_tests::{generated, hand_picked, test_ids, test_urls};
use crate::evidence_id_constraint::EvidenceIdAllowlistConstraint;
use crate::llg_whole_string::{Lcg, WholeString};
use crate::llguidance_constraint::{engine_schema_text, top_level_grammar};
use crate::url_constraint::UrlAllowlistConstraint;

const FIXTURES: &str = "tests/fixtures/llg-parity";
const UPDATE_ENV: &str = "LLG_PARITY_UPDATE";

/// Why a schema row's server grammar is not the one llama-server enforces.
const GBNF_NOTE: &str = "llama-server's chat path (--jinja, its default) converts a \
     response_format schema to GBNF, not llguidance (LS common/chat.cpp:1323-1331, \
     common/peg-parser.cpp:1734 add_schema); the battery's grammar facet shows it. \
     server_grammar is the llguidance form LS emits off that path \
     (common/json-schema-to-grammar.cpp:1234-1236).";

/// `grammars.json`: the production grammars and schemas, with their sources.
#[derive(Deserialize)]
struct Sources {
    grammars: Vec<Source>,
}

#[derive(Deserialize)]
struct Source {
    row: String,
    grammar_id: String,
    source: String,
    #[serde(default)]
    lark: Option<String>,
    #[serde(default)]
    schema: Option<Value>,
    inputs: Vec<String>,
}

/// The corpus and its sources live in the crate, so the test reads nothing
/// outside it (boundary-gate 3c); llg-parity reads the corpus from here.
fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURES)
}

/// A Lark grammar as the remote client sends it to llama-server
/// (oicp-client chat_wire.rs:126-127, `write_constraints`).
fn wire_lark(lark: &str) -> String {
    if lark.trim_start().starts_with("%llguidance") {
        lark.to_string()
    } else {
        format!("%llguidance {{}}\n{lark}")
    }
}

/// A schema as llama-server built with llguidance compiles it off the chat
/// path: the schema the client sends, closed by the decider the engine uses
/// too (oicp-client `write_structured_output`), with no rewrite of the
/// server's own (LS common/json-schema-to-grammar.cpp:1234-1236).
fn wire_schema(schema: &Value) -> String {
    let mut sent = schema.clone();
    default_additional_properties_false(&mut sent);
    format!("%llguidance {{}}\nstart: %json {sent}")
}

/// Inputs for one grammar: its hand-written cases, then random walks over
/// the engine's mask and one mutation of each accepted walk. Fixed seed.
fn inputs(engine: &WholeString, grammar_id: &str, hand: &[String]) -> Vec<String> {
    let seed = grammar_id.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    let mut rng = Lcg::new(seed);
    let mut out: Vec<String> = hand.to_vec();
    for _ in 0..12 {
        let (text, accepted) = engine.walk(&mut rng, 40);
        if accepted {
            out.push(mutate(&text, &mut rng));
        }
        out.push(text);
    }
    let mut seen = BTreeSet::new();
    out.retain(|t| seen.insert(t.clone()));
    out
}

/// Delete, insert or substitute one byte. Walks are ASCII, so the result is
/// still a string.
fn mutate(text: &str, rng: &mut Lcg) -> String {
    let mut b = text.as_bytes().to_vec();
    let printable = |rng: &mut Lcg| 0x20 + rng.below(0x5f) as u8;
    let at = rng.below(b.len() + 1);
    match rng.below(3) {
        0 if at < b.len() => {
            b.remove(at);
        }
        2 if at < b.len() => b[at] = printable(rng),
        _ => b.insert(at, printable(rng)),
    }
    String::from_utf8(b).expect("ASCII mutation")
}

fn line(
    row: &str,
    grammar_id: &str,
    source: &str,
    server: (&str, String),
    verdicts: Vec<(String, bool)>,
) -> Value {
    let accepted = verdicts.iter().filter(|(_, a)| *a).count();
    assert!(
        accepted > 0 && accepted < verdicts.len(),
        "{grammar_id}: {accepted}/{} accepted; both verdicts must occur or agreement says nothing",
        verdicts.len()
    );
    let mut v = json!({
        "row": row,
        "grammar_id": grammar_id,
        "source": source,
        "server_converter": server.0,
        "server_grammar": server.1,
        "inputs": verdicts
            .into_iter()
            .map(|(text, ok)| json!({"text": text, "engine_accepts": ok}))
            .collect::<Vec<_>>(),
    });
    if server.0 == "gbnf" {
        v["server_note"] = json!(GBNF_NOTE);
    }
    v
}

fn grammar_line(src: &Source) -> Value {
    let (engine_text, server) = match (&src.lark, &src.schema) {
        (Some(lark), None) => (lark.clone(), ("llguidance", wire_lark(lark))),
        (None, Some(schema)) => (
            engine_schema_text(schema).expect("schema serialises"),
            ("gbnf", wire_schema(schema)),
        ),
        _ => panic!("{}: exactly one of lark and schema", src.grammar_id),
    };
    let engine = WholeString::new(top_level_grammar(&engine_text).expect("engine grammar"))
        .unwrap_or_else(|e| panic!("{}: engine grammar must compile: {e}", src.grammar_id));
    let verdicts = inputs(&engine, &src.grammar_id, &src.inputs)
        .into_iter()
        .map(|t| {
            let ok = engine
                .accepts(t.as_bytes())
                .unwrap_or_else(|e| panic!("{}: matcher error on {t:?}: {e}", src.grammar_id));
            (t, ok)
        })
        .collect();
    line(&src.row, &src.grammar_id, &src.source, server, verdicts)
}

/// An allow-list row: the engine decides with its trie; the server gets
/// `llguidance_lark` for the one list (chat_wire.rs:84-93).
fn allowlist_line(
    row: &str,
    lang: &AllowlistLanguage,
    ids: &[String],
    trie: impl Fn(&[u8]) -> bool,
) -> Value {
    let server = llguidance_lark(&[(lang, ids)]).expect("non-empty list");
    let mut seen = BTreeSet::new();
    let verdicts = hand_picked()
        .into_iter()
        .chain(generated(40))
        .filter(|t| seen.insert(t.clone()))
        .map(|t| {
            let ok = trie(t.as_bytes());
            (t, ok)
        })
        .collect();
    let source = format!(
        "sovereign_contracts::decode_allowlist::llguidance_lark over {ids:?}; engine verdict from the trie"
    );
    line(row, row, &source, ("llguidance", server), verdicts)
}

fn corpus() -> String {
    let dir = fixture_dir();
    let text = std::fs::read_to_string(dir.join("grammars.json")).expect("grammars.json");
    let sources: Sources = serde_json::from_str(&text).expect("grammars.json parses");
    let urls = test_urls();
    let ids = test_ids();
    sources
        .grammars
        .iter()
        .map(grammar_line)
        .chain([
            allowlist_line("decode.url-allowlist", &URL, &urls, |t| {
                UrlAllowlistConstraint::accepts_whole(&urls, t)
            }),
            allowlist_line("decode.evidence-id-allowlist", &EVIDENCE_ID, &ids, |t| {
                EvidenceIdAllowlistConstraint::accepts_whole(&ids, t)
            }),
        ])
        .map(|v| v.to_string() + "\n")
        .collect()
}

#[test]
fn the_llg_parity_corpus_holds_the_engines_verdicts() {
    let want = corpus();
    let path = fixture_dir().join("corpus.jsonl");
    if std::env::var(UPDATE_ENV).as_deref() == Ok("1") {
        std::fs::write(&path, &want).expect("write corpus.jsonl");
        return;
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    let first = have
        .lines()
        .zip(want.lines())
        .position(|(a, b)| a != b)
        .unwrap_or(have.lines().count().min(want.lines().count()));
    assert!(
        have == want,
        "{} differs from the engine's verdicts at line {} ({} lines committed, {} generated). \
         If the change is intended, regenerate with {UPDATE_ENV}=1 and re-run llg-parity.",
        path.display(),
        first + 1,
        have.lines().count(),
        want.lines().count()
    );
}
