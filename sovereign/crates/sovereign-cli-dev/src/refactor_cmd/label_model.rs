// SPDX-License-Identifier: AGPL-3.0-or-later
//! The model-assisted labelling pass, and the harness that decides whether to
//! believe it.
//!
//! # What the model is asked, and what it is never asked
//!
//! The deterministic pass (`label --from-register`) already settles every name
//! the concept register carries. What is left is the genuinely open question,
//! and it is the one this codebase has measured itself getting wrong:
//!
//! > Two crates define a type with the same name. Is that ONE concept that
//! > forked, or two unrelated things that happen to share a word?
//!
//! `quality/CONCEPTS.toml` records the answer's difficulty directly —
//! adjudication of a 55-row sample found "roughly half the two-crate tail is
//! name coincidence, not duplication". A pass that answers `converge` for
//! everything would look productive and would be wrong half the time.
//!
//! The model is NEVER asked to decide whether a site is done, to classify a
//! compiler error, or to guarantee anything code can enforce (ARCH §7.6). It
//! answers one closed-set question with a mandatory `unsure`.
//!
//! # The unit is the GROUP, not the site
//!
//! The name detector emits one site per definition, but "are these the same
//! concept" is a question about all of them at once. Grouping first turns 118
//! sites into 33 questions and — more importantly — is the only framing in
//! which the question is answerable at all.
//!
//! # Grounding: a held-out half, scored once
//!
//! A pass tuned against the same rows it is scored on has measured nothing.
//! The groups are split deterministically into `dev` and `test` by a hash of
//! the name — tune against `dev`, report `test` once. The split is a function
//! of the name, so it cannot drift between runs and cannot be reshuffled until
//! it flatters a result.

use std::collections::BTreeMap;
use std::path::Path;

use super::detector::Site;
use super::labels::{Disposition, Label};

/// Routed to the daemon's short/fast alias — mechanical batch classification is
/// exactly what a phase override exists for, and it keeps the pass on this
/// machine at zero external cost.
pub const MODEL_ALIAS: &str = "fast";

/// Factual classification, not prose.
const TEMPERATURE: f32 = 0.1;
/// One verdict, one canonical path, one short reason.
const MAX_TOKENS: u32 = 220;
/// Lines of source shown per definition. Enough to see the shape; small enough
/// that a nine-definition group still fits a short context.
const SNIPPET_LINES: usize = 14;

/// One duplicated name and every place it is defined.
#[derive(Debug, Clone)]
pub struct NameGroup {
    pub name: String,
    pub sites: Vec<Site>,
}

/// Which half of the grounding split a group falls in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Split {
    /// Tune here.
    Dev,
    /// Score here, once.
    Test,
}

/// Deterministic split, keyed on the name itself.
///
/// Not a counter and not a shuffle: the same name always lands in the same
/// half, on every host, forever. A split that could be re-rolled is a split
/// that will be re-rolled until it agrees with you.
pub fn split_of(name: &str) -> Split {
    let h = kernel_types::ContentHash::of_str(name);
    let short = h.short();
    let first = short.as_bytes().first().copied().unwrap_or(b'0');
    // Hex nibble parity — even is dev, odd is test.
    if (first as char).to_digit(16).unwrap_or(0) % 2 == 0 {
        Split::Dev
    } else {
        Split::Test
    }
}

pub fn group_by_name(sites: &[Site]) -> Vec<NameGroup> {
    let mut by: BTreeMap<String, Vec<Site>> = BTreeMap::new();
    for s in sites {
        by.entry(s.token.clone()).or_default().push(s.clone());
    }
    by.into_iter()
        .map(|(name, sites)| NameGroup { name, sites })
        .collect()
}

/// The model's answer. A closed set with a mandatory `unsure` — see the module
/// doc for why the third option is not optional.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Judgement {
    /// One concept that forked. The twins migrate onto `canonical`.
    OneConcept,
    /// Unrelated things sharing a word. Rename apart, do not converge.
    DifferentConcepts,
    /// A per-crate convention (`Result`, `Error`, `Args`) — not duplication.
    PerCrateIdiom,
    /// Could not tell from what was shown.
    Unsure,
}

impl Judgement {
    pub fn to_disposition(&self) -> Disposition {
        match self {
            Judgement::OneConcept => Disposition::Converge,
            Judgement::DifferentConcepts => Disposition::Distinct,
            Judgement::PerCrateIdiom => Disposition::Idiom,
            Judgement::Unsure => Disposition::Unsure,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModelAnswer {
    pub judgement: Judgement,
    /// Only meaningful for `one-concept`.
    #[serde(default)]
    pub canonical: String,
    #[serde(default)]
    pub why: String,
}

/// Read the declaration around a site so the model sees shape, not just a name.
fn snippet(root: &Path, site: &Site) -> String {
    let Ok(text) = std::fs::read_to_string(root.join(&site.file)) else {
        return "(source unavailable)".to_string();
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = (site.line as usize).saturating_sub(1);
    lines
        .iter()
        .skip(start)
        .take(SNIPPET_LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

fn crate_of(site: &Site) -> String {
    // `sovereign/crates/<name>/src/...` or `<name>/src/...`
    let parts: Vec<&str> = site.file.split('/').collect();
    if let Some(i) = parts.iter().position(|p| *p == "crates") {
        parts.get(i + 1).unwrap_or(&"?").to_string()
    } else {
        parts.first().unwrap_or(&"?").to_string()
    }
}

/// Compose the question.
///
/// Succinct and non-contradictory on purpose: this runs on a small open-weight
/// model, and a prompt that hedges in one clause and demands certainty in the
/// next produces confident noise. One task, one output shape, one escape hatch.
pub fn compose_prompt(root: &Path, group: &NameGroup) -> String {
    let mut p = String::new();
    p.push_str(&format!(
        "The type name `{}` is defined in {} different Rust crates in one workspace.\n\n",
        group.name,
        group.sites.len()
    ));
    for s in &group.sites {
        p.push_str(&format!("--- crate `{}` ({})\n", crate_of(s), s.file));
        p.push_str(&snippet(root, s));
        p.push_str("\n\n");
    }
    p.push_str(
        "Decide which ONE of these describes them:\n\
         \n\
         one-concept        the same idea, forked into copies; they should become one type\n\
         different-concepts unrelated ideas that happen to share a word\n\
         per-crate-idiom    a convention every crate repeats (Result, Error, Args, Config)\n\
         unsure             you cannot tell from what is shown\n\
         \n\
         Answer `unsure` rather than guessing. It is a useful answer.\n\
         \n\
         Reply with one JSON object and nothing else:\n\
         {\"judgement\":\"one-concept\",\"canonical\":\"crate_name::path::Type\",\"why\":\"one short sentence\"}\n\
         \n\
         `canonical` is the crate that should own the survivor; use \"\" unless the judgement is one-concept.\n",
    );
    p
}

pub fn strip_fences(s: &str) -> &str {
    let t = s.trim();
    let t = t.strip_prefix("```json").unwrap_or(t);
    let t = t.strip_prefix("```").unwrap_or(t);
    t.strip_suffix("```").unwrap_or(t).trim()
}

pub fn parse_answer(raw: &str) -> Result<ModelAnswer, String> {
    let stripped = strip_fences(raw);
    // Models sometimes emit a sentence then the object. Take the first object.
    let candidate = match (stripped.find('{'), stripped.rfind('}')) {
        (Some(a), Some(b)) if b > a => &stripped[a..=b],
        _ => stripped,
    };
    serde_json::from_str::<ModelAnswer>(candidate).map_err(|e| {
        format!(
            "{e}; raw head: {:?}",
            raw.chars().take(160).collect::<String>()
        )
    })
}

/// Ask the daemon one question.
///
/// Local by construction: this is mechanical batch classification, which is
/// exactly the work that should run on the stack we are building rather than
/// on someone else's tokens.
pub async fn ask(
    client: &reqwest::Client,
    daemon_url: &str,
    model: &str,
    prompt: &str,
) -> Result<ModelAnswer, String> {
    let url = format!("{}/v1/chat/completions", daemon_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "temperature": TEMPERATURE,
        "top_p": 1.0,
        "max_tokens": MAX_TOKENS,
        "messages": [
            {"role": "system", "content": "You output exactly one JSON object matching the requested schema. No prose. No markdown fences."},
            {"role": "user", "content": prompt},
        ],
    });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon {status}: {}",
            text.chars().take(300).collect::<String>()
        ));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse daemon response: {e}"))?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if content.is_empty() {
        return Err("empty content from daemon".into());
    }
    parse_answer(content)
}

/// Run the pass over a set of groups, one question each.
pub async fn run_groups(
    root: &Path,
    daemon_url: &str,
    model: &str,
    groups: &[NameGroup],
    progress: bool,
) -> BTreeMap<String, Result<ModelAnswer, String>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .unwrap_or_default();
    let mut out = BTreeMap::new();
    for (i, g) in groups.iter().enumerate() {
        let prompt = compose_prompt(root, g);
        let answer = ask(&client, daemon_url, model, &prompt).await;
        if progress {
            let label = match &answer {
                Ok(a) => a.judgement.to_disposition().as_str().to_string(),
                Err(e) => format!("FAILED ({})", e.chars().take(60).collect::<String>()),
            };
            eprintln!("  [{}/{}] {:<24} {}", i + 1, groups.len(), g.name, label);
        }
        out.insert(g.name.clone(), answer);
    }
    out
}

pub fn render_score(split: Split, s: &Score) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let name = match split {
        Split::Dev => "DEV (tune here)",
        Split::Test => "TEST (report once)",
    };
    let _ = writeln!(o, "  {name}");
    let _ = writeln!(o, "    groups         {}", s.n);
    let _ = writeln!(o, "    agreed         {}", s.agree);
    let _ = writeln!(o, "    abstained      {}", s.abstained);
    let _ = writeln!(o, "    failed         {}", s.failed);
    let _ = writeln!(
        o,
        "    FALSE CONVERGE {}  (the costly error)",
        s.false_converge
    );
    match s.precision() {
        Some(p) => {
            let _ = writeln!(o, "    precision      {p:.3}  (of groups it committed on)");
        }
        // Never print 0.0 for "nothing to judge" — that reads as total failure.
        None => {
            let _ = writeln!(
                o,
                "    precision      COULD-NOT-JUDGE (it committed on nothing)"
            );
        }
    }
    if let Some(a) = s.abstention_rate() {
        let _ = writeln!(o, "    abstention     {a:.3}");
    }
    for d in s.disagreements.iter().take(12) {
        let _ = writeln!(o, "      {d}");
    }
    o
}

/// One scored comparison against the gold set.
#[derive(Debug, Default, Clone)]
pub struct Score {
    pub n: usize,
    pub agree: usize,
    /// Model said converge, gold said otherwise. The costly error: a worker
    /// burns sites that should have been left alone.
    pub false_converge: usize,
    /// Model said unsure. Not an error — an honest abstention, and a cost.
    pub abstained: usize,
    /// Model could not be parsed or the call failed.
    pub failed: usize,
    pub disagreements: Vec<String>,
}

impl Score {
    /// Of the groups the model committed on (not unsure), how many matched
    /// gold. Abstentions are excluded from the numerator AND denominator —
    /// counting them as wrong would punish the honest answer.
    pub fn precision(&self) -> Option<f64> {
        let committed = self.n.saturating_sub(self.abstained + self.failed);
        if committed == 0 {
            return None;
        }
        Some(self.agree as f64 / committed as f64)
    }

    pub fn abstention_rate(&self) -> Option<f64> {
        if self.n == 0 {
            return None;
        }
        Some(self.abstained as f64 / self.n as f64)
    }
}

/// Score model answers against the gold labels.
pub fn score(
    answers: &BTreeMap<String, Result<ModelAnswer, String>>,
    gold: &BTreeMap<String, Disposition>,
) -> Score {
    let mut s = Score::default();
    for (name, gold_disp) in gold {
        let Some(answer) = answers.get(name) else {
            continue;
        };
        s.n += 1;
        match answer {
            Err(e) => {
                s.failed += 1;
                s.disagreements.push(format!("{name}: FAILED — {e}"));
            }
            Ok(a) => {
                let got = a.judgement.to_disposition();
                if got == Disposition::Unsure {
                    s.abstained += 1;
                } else if got == *gold_disp {
                    s.agree += 1;
                } else {
                    if got == Disposition::Converge {
                        s.false_converge += 1;
                    }
                    s.disagreements.push(format!(
                        "{name}: model {} / gold {}",
                        got.as_str(),
                        gold_disp.as_str()
                    ));
                }
            }
        }
    }
    s
}

/// Load a gold file — the same jsonl shape a label uses, so the seat's
/// adjudication IS a label file and nothing needs a second format.
pub fn load_gold(path: &Path) -> Result<BTreeMap<String, Disposition>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let l: Label =
            serde_json::from_str(line).map_err(|e| format!("{}:{}: {e}", path.display(), i + 1))?;
        // Gold is keyed by NAME (the group), not by site key.
        let name = l.key.rsplit('/').next().unwrap_or(&l.key).to_string();
        out.insert(name, l.disp);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(file: &str, token: &str) -> Site {
        Site {
            detector: super::super::detector::DetectorId::Name,
            file: file.to_string(),
            line: 1,
            locus: file.to_string(),
            token: token.to_string(),
            note: String::new(),
        }
    }

    #[test]
    fn the_split_is_a_function_of_the_name_and_never_moves() {
        let a = split_of("Verdict");
        for _ in 0..5 {
            assert_eq!(split_of("Verdict"), a);
        }
        // And it actually divides — not everything in one half.
        let names = [
            "Verdict", "Result", "Error", "Plan", "Role", "Gap", "Registry", "Source", "Draft",
            "Citation", "Step", "Artifact",
        ];
        let dev = names.iter().filter(|n| split_of(n) == Split::Dev).count();
        assert!(
            dev > 0 && dev < names.len(),
            "split put everything in one half"
        );
    }

    #[test]
    fn groups_collapse_sites_into_one_question_per_name() {
        let sites = vec![
            site("a/src/x.rs", "Verdict"),
            site("b/src/y.rs", "Verdict"),
            site("c/src/z.rs", "Plan"),
        ];
        let g = group_by_name(&sites);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].name, "Plan");
        assert_eq!(g[1].sites.len(), 2);
    }

    #[test]
    fn the_prompt_offers_exactly_the_four_answers_and_names_the_escape_hatch() {
        let d = tempfile::tempdir().unwrap();
        let g = NameGroup {
            name: "Verdict".into(),
            sites: vec![site("a/src/x.rs", "Verdict")],
        };
        let p = compose_prompt(d.path(), &g);
        for opt in [
            "one-concept",
            "different-concepts",
            "per-crate-idiom",
            "unsure",
        ] {
            assert!(p.contains(opt), "prompt omits {opt}");
        }
        assert!(p.contains("rather than guessing"));
    }

    #[test]
    fn a_fenced_or_prefixed_answer_still_parses() {
        let a =
            parse_answer("```json\n{\"judgement\":\"one-concept\",\"canonical\":\"k::V\"}\n```")
                .unwrap();
        assert_eq!(a.judgement, Judgement::OneConcept);
        let b =
            parse_answer("Here is my answer: {\"judgement\":\"unsure\"} hope that helps").unwrap();
        assert_eq!(b.judgement, Judgement::Unsure);
    }

    #[test]
    fn an_unparseable_answer_is_an_error_not_a_default() {
        assert!(parse_answer("I think they are the same").is_err());
    }

    /// Abstention is honest, so it must not be scored as a wrong answer — but
    /// it is also not free, so it is reported as its own rate.
    #[test]
    fn abstentions_leave_precision_alone_and_surface_as_their_own_rate() {
        let mut answers = BTreeMap::new();
        answers.insert(
            "A".to_string(),
            Ok(ModelAnswer {
                judgement: Judgement::OneConcept,
                canonical: "k::A".into(),
                why: String::new(),
            }),
        );
        answers.insert(
            "B".to_string(),
            Ok(ModelAnswer {
                judgement: Judgement::Unsure,
                canonical: String::new(),
                why: String::new(),
            }),
        );
        let mut gold = BTreeMap::new();
        gold.insert("A".to_string(), Disposition::Converge);
        gold.insert("B".to_string(), Disposition::Distinct);
        let s = score(&answers, &gold);
        assert_eq!(s.n, 2);
        assert_eq!(s.agree, 1);
        assert_eq!(s.abstained, 1);
        assert_eq!(
            s.precision(),
            Some(1.0),
            "abstention must not count as wrong"
        );
        assert_eq!(s.abstention_rate(), Some(0.5));
    }

    /// The costly error gets its own counter: a false `converge` sends a worker
    /// to change code that should have been left alone.
    #[test]
    fn a_false_converge_is_counted_separately_from_any_other_miss() {
        let mut answers = BTreeMap::new();
        answers.insert(
            "A".to_string(),
            Ok(ModelAnswer {
                judgement: Judgement::OneConcept,
                canonical: String::new(),
                why: String::new(),
            }),
        );
        answers.insert(
            "B".to_string(),
            Ok(ModelAnswer {
                judgement: Judgement::DifferentConcepts,
                canonical: String::new(),
                why: String::new(),
            }),
        );
        let mut gold = BTreeMap::new();
        gold.insert("A".to_string(), Disposition::Distinct);
        gold.insert("B".to_string(), Disposition::Converge);
        let s = score(&answers, &gold);
        assert_eq!(s.false_converge, 1, "only A is a false converge");
        assert_eq!(s.agree, 0);
    }

    #[test]
    fn precision_over_an_all_abstained_run_is_absent_not_zero() {
        let mut answers = BTreeMap::new();
        answers.insert(
            "A".to_string(),
            Ok(ModelAnswer {
                judgement: Judgement::Unsure,
                canonical: String::new(),
                why: String::new(),
            }),
        );
        let mut gold = BTreeMap::new();
        gold.insert("A".to_string(), Disposition::Converge);
        let s = score(&answers, &gold);
        // Absence reported, never defaulted (ARCH §18.3). Zero would read as
        // "the model was wrong about everything".
        assert_eq!(s.precision(), None);
    }
}
