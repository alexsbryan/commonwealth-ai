// SPDX-License-Identifier: AGPL-3.0-or-later
//! Build the Wrapped artifact against a real corpus and print the deck.
//!
//! The unit tests prove each fold's contract on fixtures; this proves the
//! deck is worth showing on a real archive, which fixtures cannot. Run it
//! after changing a fold or a threshold — a card that passes its tests and
//! still reads as a word-frequency list is a regression the tests will not
//! catch.
//!
//! ```text
//! cargo run -p sovereign-meshapp --example wrapped_dump -- \
//!     ~/.svrnmesh/indexes/conversations-anthropic ~/.svrnmesh/sovereign.db
//! ```
//!
//! Pass `--json` to dump the artifact instead of the human rendering.

use sovereign_meshapp::wrapped::{build_wrapped_artifact, WrappedCard};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if positional.is_empty() {
        eprintln!("usage: wrapped_dump <index-dir> [state-db] [--json]");
        std::process::exit(2);
    }
    let index = std::path::PathBuf::from(positional[0]);
    let db = positional.get(1).map(std::path::PathBuf::from);

    let artifact = match build_wrapped_artifact(&index, db.as_deref()).await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("build failed: {e}");
            std::process::exit(1);
        }
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&artifact).unwrap());
        return;
    }

    println!(
        "\n=== {} · schema v{} · {} cards ===",
        artifact.corpus_id,
        artifact.schema_version,
        artifact.cards.len()
    );
    for card in &artifact.cards {
        match card {
            WrappedCard::Scale(c) => {
                println!("\n── SCALE ──");
                println!(
                    "  {} conversations · {} months · {} words ({}–{})",
                    c.conversations, c.months_active, c.words_total, c.first_date, c.last_date
                );
            }
            WrappedCard::Rhythm(c) => {
                println!("\n── RHYTHM ──");
                println!("  {} turns", c.total_turns);
                if let Some(s) = &c.longest_session {
                    println!(
                        "  longest rabbit hole: {} min, {} turns on {}",
                        s.duration_minutes, s.turns, s.date
                    );
                }
            }
            WrappedCard::Recurring(c) => {
                println!("\n── THE QUESTION YOU KEEP ASKING ──");
                for t in &c.threads {
                    println!(
                        "\n  {} conversations over {} days",
                        t.conversations, t.span_days
                    );
                    for a in &t.askings {
                        println!("    {}  {}", a.date, truncate(&a.excerpt.text, 120));
                    }
                }
                trace(&c.derivation);
            }
            WrappedCard::Turn(c) => {
                println!("\n── THE TURN ──");
                for p in &c.pivots {
                    println!(
                        "\n  drop {:.3} (cos {:.3} vs median {:.3}) · {} · seam {}/{} · {}",
                        p.drop,
                        p.cosine,
                        p.conv_median,
                        p.date,
                        p.seam_index,
                        p.chunk_count,
                        p.title.as_deref().unwrap_or("(untitled)")
                    );
                    if let Some(b) = &p.before {
                        println!("    BEFORE  {}", truncate(&b.text, 130));
                    }
                    if let Some(a) = &p.after {
                        println!("    AFTER   {}", truncate(&a.text, 130));
                    }
                }
                trace(&c.derivation);
            }
            WrappedCard::Obsessions(c) => {
                println!("\n── OBSESSIONS ──");
                for q in &c.quarters {
                    let line: Vec<String> = q
                        .topics
                        .iter()
                        .map(|t| {
                            format!(
                                "{} ({}, z={:.1})",
                                t.text, t.conversations, t.distinctiveness
                            )
                        })
                        .collect();
                    println!("  {}  {}", q.quarter, line.join(" · "));
                }
                trace(&c.derivation);
            }
            WrappedCard::NightShift(c) => {
                println!(
                    "\n── THE NIGHT SHIFT ── (local = UTC{:+})",
                    c.utc_offset_hours
                );
                for b in &c.bands {
                    let line: Vec<String> = b
                        .topics
                        .iter()
                        .map(|t| format!("{} ({})", t.text, t.conversations))
                        .collect();
                    println!(
                        "  {:<12} {:02}-{:02}  [{} mentions]  {}",
                        b.name,
                        b.start_hour,
                        b.end_hour,
                        b.mentions,
                        line.join(" · ")
                    );
                }
                trace(&c.derivation);
            }
            WrappedCard::Cast(c) => {
                println!(
                    "\n── THE CAST ── ({} nodes, {} links)",
                    c.nodes.len(),
                    c.edges.len()
                );
                let mut nodes = c.nodes.clone();
                nodes.sort_by(|a, b| b.bridging.partial_cmp(&a.bridging).unwrap());
                for n in nodes.iter().take(8) {
                    println!(
                        "  {:<28} bridging {:.3} · {} convs · {} links · {}→{}",
                        n.canonical_name,
                        n.bridging,
                        n.conversations,
                        n.degree,
                        n.first_date,
                        n.last_date
                    );
                }
                println!("  strongest links:");
                for e in c.edges.iter().take(6) {
                    println!(
                        "    {} — {}   pmi {:.2}, {} shared",
                        e.source, e.target, e.pmi, e.co_conversations
                    );
                }
                trace(&c.derivation);
            }
            WrappedCard::Door(_) => println!("\n── DOOR ──"),
        }
    }
    println!();
}

fn trace(derivation: &[String]) {
    for line in derivation {
        println!("    · {line}");
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}
