// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn proxy …` — query a Proxy Voting Corpus (SEC DEF 14A).
//!
//! The legibility surface: for one issuer's installed `proxy-cik…` corpus,
//! answer "what is on the ballot and what are the sides?" under the
//! cite-or-abstain discipline. A turn sealed to the corpus (retrieval
//! restricted via `enabled_corpora`) runs the real chat path; because the
//! sealed corpus is in the `proxy-cik` family, the runtime selects
//! `GateSurface::ProxyArgument` (its own calibrated bank — RL-1: no
//! confabulated opposition for a management item; RL-2: both sides cited
//! for a shareholder proposal).

pub mod ask;

const USAGE: &str = "\
usage: sovereign proxy <subcommand>

subcommands:
  ask <corpus-id> \"<question>\"   Answer a question about a company's ballot,
                                cite-or-abstain over the filing's verbatim text.";

pub async fn run_proxy(args: &[String]) -> i32 {
    let mut it = args.iter();
    let Some(sub) = it.next() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let rest: Vec<String> = it.cloned().collect();
    match sub.as_str() {
        "ask" => ask::cmd_ask(&rest).await,
        other => {
            eprintln!("error: unknown `proxy` subcommand `{other}`\n\n{USAGE}");
            2
        }
    }
}

pub(crate) use sovereign_core::time::unix_now as now_unix;
