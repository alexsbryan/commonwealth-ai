// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench er-score <predicted.json> <gold.json>` — score one clustering
//! against another with `sovereign_eval::entity_resolution_score` (B³,
//! pairwise, CEAF-e, LEA) and print the report as JSON. Each file is a JSON
//! object mapping a mention id to its cluster id. One scorer for the atlas
//! benches and the research composers alike, so no script keeps its own copy.

use std::path::Path;

use sovereign_eval::entity_resolution_score::{score, Clustering};

use sovereign_cli_base::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn bench er-score",
    summary: "Score a clustering against a gold clustering (B³, pairwise, CEAF-e, LEA) and print the report as JSON.",
    sections: &[
        HelpSection::Usage("svrn bench er-score <predicted.json> <gold.json>"),
        HelpSection::Examples(&[(
            "svrn bench er-score composed.json gold.json",
            "Each file maps mention id -> cluster id; only mentions both hold are scored, the rest are listed.",
        )]),
        HelpSection::Notes(
            "Exit 2 on a usage error, 4 when a file cannot be read as a clustering (nothing was judged).",
        ),
    ],
};

fn load(path: &str) -> Result<Clustering, String> {
    let text = std::fs::read_to_string(Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    serde_json::from_str(&text)
        .map_err(|e| format!("{path}: not a {{mention: cluster}} object: {e}"))
}

pub fn cmd_er_score(args: &[String]) -> i32 {
    if args
        .first()
        .is_some_and(|a| matches!(a.as_str(), "--help" | "-h" | "help"))
    {
        help::print(&HELP);
        return 0;
    }
    let [predicted, gold] = args else {
        help::print(&HELP);
        return 2;
    };
    match (load(predicted), load(gold)) {
        (Ok(p), Ok(g)) => {
            let report = score(&p, &g);
            println!(
                "{}",
                serde_json::to_string(&report).expect("the report serialises")
            );
            0
        }
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("error: {e}");
            4
        }
    }
}
