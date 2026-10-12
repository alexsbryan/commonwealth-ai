// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn verify <file|-> [--corpus <id>]... [--tier fast|checker] [--json]` —
//! check a piece of writing against your corpora, sentence by sentence.
//!
//! A client of the daemon's published `verify` (OICP v0.6 §2.6), reached the
//! way any OICP client reaches it: the manifest names the endpoint and the
//! tiers, and a host that advertises no `evidence:verify` is said so, never
//! guessed at. Each paragraph of the file is one unit; every claim comes back
//! with its line, its verdict, the rung that decided and, for `unsupported`,
//! what no passage carried. It is the platform's own second user of `verify`
//! (CITE_V1 §1.3), so it reads nothing the wire does not carry.

use std::io::Read;

use sovereign_contracts::oicp::verify::{VerifyRequest, VerifyResponse, VerifyTier, VerifyVerdict};
use sovereign_contracts::oicp::{features, ProviderManifest};

#[derive(clap::Parser, Debug)]
#[command(name = "svrn verify", no_binary_name = true, disable_help_flag = true)]
struct Opts {
    /// The file to check, or `-` for stdin.
    file: String,
    /// A corpus to check against; repeatable. None means every corpus you may read.
    #[arg(long = "corpus")]
    corpora: Vec<String>,
    /// `fast` (no model) or `checker`.
    #[arg(long, default_value = "fast")]
    tier: String,
    /// Print each unit's reply as JSON, one per line.
    #[arg(long)]
    json: bool,
}

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }
    let opts = match sovereign_cli_shared::flag_surface::parse::<Opts>(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("verify: {e}");
            return 2;
        }
    };
    let tier = match opts.tier.as_str() {
        "fast" => VerifyTier::Fast,
        "checker" => VerifyTier::Checker,
        other => {
            eprintln!("verify: --tier is `fast` or `checker`, not `{other}`");
            return 2;
        }
    };
    let text = match read_input(&opts.file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("verify: {e}");
            return 2;
        }
    };
    let Some(base) = sovereign_cli_shared::urls::daemon_base_or_refuse(None) else {
        return 2;
    };
    let http = reqwest::Client::new();
    let endpoint = match verify_endpoint(&http, &base, tier).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("verify: {e}");
            return 1;
        }
    };
    let mut counts = Counts::default();
    for (line, unit) in paragraphs(&text) {
        let req = VerifyRequest {
            unit: unit.to_string(),
            context: None,
            corpora: opts.corpora.clone(),
            tier,
            span_context: None,
        };
        let reply = match post(&http, &format!("{base}{endpoint}"), &req).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("verify: line {line}: {e}");
                return 1;
            }
        };
        if opts.json {
            match serde_json::to_string(&reply) {
                Ok(line) => println!("{line}"),
                Err(e) => {
                    eprintln!("verify: line {line}: the reply does not encode: {e}");
                    return 1;
                }
            }
        } else {
            print_unit(line, unit, &reply);
        }
        counts.add(&reply);
    }
    if !opts.json {
        println!("\n{}", counts.line());
    }
    0
}

fn read_input(file: &str) -> Result<String, String> {
    if file == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| format!("stdin: {e}"))?;
        return Ok(s);
    }
    std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))
}

/// The paragraphs of `text` with the 1-based line each starts on.
fn paragraphs(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start: Option<(usize, usize)> = None; // (line, byte)
    let mut byte = 0;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let blank = line.trim().is_empty();
        match (blank, start) {
            (false, None) => start = Some((i + 1, byte)),
            (true, Some((l, b))) => {
                out.push((l, text[b..byte].trim()));
                start = None;
            }
            _ => {}
        }
        byte += line.len();
    }
    if let Some((l, b)) = start {
        out.push((l, text[b..].trim()));
    }
    out
}

/// The manifest's verify endpoint, if the host offers `tier`.
async fn verify_endpoint(
    http: &reqwest::Client,
    base: &str,
    tier: VerifyTier,
) -> Result<String, String> {
    let m: ProviderManifest = http
        .get(format!("{base}/oicp/v1/capabilities"))
        .send()
        .await
        .map_err(|e| format!("the daemon at {base} did not answer: {e}"))?
        .json()
        .await
        .map_err(|e| format!("the manifest does not decode: {e}"))?;
    let evidence = m.knowledge.as_ref().and_then(|k| k.evidence.as_ref());
    let endpoint = evidence.and_then(|e| e.verify_endpoint.clone());
    match endpoint {
        Some(e) if m.has_feature(features::EVIDENCE_VERIFY) => {
            if evidence.is_some_and(|ev| ev.verify_tiers.contains(&tier)) {
                Ok(e)
            } else {
                Err(format!(
                    "the daemon does not serve the `{}` tier",
                    tier.as_str()
                ))
            }
        }
        _ => Err("the daemon does not advertise `evidence:verify`".into()),
    }
}

async fn post(
    http: &reqwest::Client,
    url: &str,
    req: &VerifyRequest,
) -> Result<VerifyResponse, String> {
    let resp = http
        .post(url)
        .json(req)
        .send()
        .await
        .map_err(|e| format!("no answer: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("{status}: {body}"));
    }
    resp.json()
        .await
        .map_err(|e| format!("the reply does not decode: {e}"))
}

fn verdict_word(v: &VerifyVerdict) -> String {
    match v {
        VerifyVerdict::Supported => "supported".into(),
        VerifyVerdict::Contradicted => "contradicted".into(),
        VerifyVerdict::Unsupported => "unsupported".into(),
        VerifyVerdict::Abstained => "abstained".into(),
        VerifyVerdict::CouldNotJudge { reason } => format!("could not judge ({reason})"),
    }
}

fn print_unit(line: usize, unit: &str, reply: &VerifyResponse) {
    let lines_before = |cp: u64| {
        unit.chars()
            .take(cp as usize)
            .filter(|c| *c == '\n')
            .count()
    };
    for c in &reply.claims {
        let at = line + lines_before(c.start);
        let mut head: String = c.exact.chars().take(72).collect();
        if c.exact.chars().count() > 72 {
            head.push('…');
        }
        println!("{at:>5}  {:<28} {head}", verdict_word(&c.verdict));
        if !c.specifics_absent.is_empty() {
            println!("       in no passage: {}", c.specifics_absent.join(", "));
        }
        if let Some(s) = c
            .evidence
            .iter()
            .find(|p| matches!(p.verdict, VerifyVerdict::Supported))
            .and_then(|p| p.span.as_ref())
        {
            println!(
                "       backed by {} [{}, {})",
                s.document.source.id, s.start, s.end
            );
        }
    }
    for u in &reply.corpora_unavailable {
        println!("       corpus {} not read: {}", u.corpus_id, u.reason);
    }
}

#[derive(Default)]
struct Counts {
    claims: usize,
    supported: usize,
    unsupported: usize,
    contradicted: usize,
    abstained: usize,
    could_not: usize,
    verifier: Option<String>,
}

impl Counts {
    fn add(&mut self, r: &VerifyResponse) {
        self.verifier.get_or_insert_with(|| r.verifier.clone());
        for c in &r.claims {
            self.claims += 1;
            match c.verdict {
                VerifyVerdict::Supported => self.supported += 1,
                VerifyVerdict::Unsupported => self.unsupported += 1,
                VerifyVerdict::Contradicted => self.contradicted += 1,
                VerifyVerdict::Abstained => self.abstained += 1,
                VerifyVerdict::CouldNotJudge { .. } => self.could_not += 1,
            }
        }
    }

    fn line(&self) -> String {
        format!(
            "{} claim(s): {} supported, {} unsupported, {} contradicted, {} abstained, {} could not judge — {} (uncalibrated)",
            self.claims,
            self.supported,
            self.unsupported,
            self.contradicted,
            self.abstained,
            self.could_not,
            self.verifier.as_deref().unwrap_or("no verifier answered"),
        )
    }
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn verify",
    summary: "Check writing against your corpora, sentence by sentence (OICP verify).",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "svrn verify <file|->                       Every corpus you may read, fast tier\n\
             svrn verify <file> --corpus <id> ...       Only these corpora\n\
             svrn verify <file> --tier checker          A model judges each claim\n\
             svrn verify <file> --json                  One reply per paragraph, as JSON",
        ),
        crate::util::help::HelpSection::Notes(
            "Each paragraph is sent as one unit. `fast` uses no model: a claim is \
             `unsupported` when a number, date or name in it appears in no passage, and \
             `abstained` otherwise. `checker` asks the daemon's judge. Every verdict is \
             uncalibrated: read `unsupported` as \"nothing backs this\", not \"this is false\".",
        ),
    ],
};

#[cfg(test)]
mod tests {
    use super::paragraphs;

    #[test]
    fn paragraphs_carry_the_line_they_start_on() {
        let t = "One.\nStill one.\n\n\nTwo.\n  \nThree.";
        assert_eq!(
            paragraphs(t),
            vec![(1, "One.\nStill one."), (5, "Two."), (7, "Three.")]
        );
    }
}
