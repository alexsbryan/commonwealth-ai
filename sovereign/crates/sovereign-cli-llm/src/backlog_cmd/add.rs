// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn backlog add` — score one item and file it.

use std::path::PathBuf;

use super::item::{self, Draft, Landed};
use super::ruler::Ruler;
use super::score;

const DEFAULT_PRODUCER: &str = "svrn backlog add";

#[derive(Debug, Default)]
struct Args {
    text: Option<String>,
    objective: Option<String>,
    key: Option<String>,
    producer: Option<String>,
    no_score: bool,
    db: Option<PathBuf>,
    ruler: Option<PathBuf>,
    daemon: Option<String>,
    create: bool,
    json: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let mut take = |name: &str| -> Result<String, String> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a {
            "--objective" => out.objective = Some(take("--objective")?),
            "--key" => out.key = Some(take("--key")?),
            "--producer" => out.producer = Some(take("--producer")?),
            "--db" => out.db = Some(PathBuf::from(take("--db")?)),
            "--ruler" => out.ruler = Some(PathBuf::from(take("--ruler")?)),
            "--daemon" => out.daemon = Some(take("--daemon")?),
            "--no-score" => out.no_score = true,
            "--create" => out.create = true,
            "--json" => out.json = true,
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}`"));
            }
            other => {
                if out.text.is_some() {
                    return Err(format!(
                        "unexpected second item text `{other}` — quote the item as \
                         one argument"
                    ));
                }
                out.text = Some(other.to_string());
            }
        }
        i += 1;
    }
    let text = out.text.as_deref().unwrap_or("").trim();
    if text.is_empty() {
        return Err("an item needs text — `svrn backlog add \"<text>\"`".to_string());
    }
    Ok(out)
}

pub async fn cmd_add(args: &[String]) -> i32 {
    let args = match parse_args(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let ruler = match Ruler::load(args.ruler.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let db = item::notes_db_path(args.db.as_deref());
    let text = args.text.clone().unwrap_or_default();

    // Say what was resolved BEFORE the slow part, so a run against the
    // wrong store or the wrong ruler is visible while it is happening
    // rather than inferable afterwards (ARCH §9).
    if !args.json {
        eprintln!("  store  {}", db.display());
        eprintln!("  ruler  {} (v{})", ruler.path.display(), ruler.version);
    }

    let scored = if args.no_score {
        if !args.json {
            eprintln!("  score  skipped (--no-score): filing unscored");
        }
        None
    } else {
        if !args.json {
            eprintln!("  score  one call to the resident model…");
        }
        let base = args
            .daemon
            .clone()
            .unwrap_or_else(score::default_daemon_base);
        match score::score_item(&ruler, &base, args.objective.as_deref(), &text).await {
            Ok(s) => Some(s),
            Err(e) => {
                // Refuse, never substitute (ARCH §18.3). An unscored
                // item filed as though it were scored would be worse
                // than no item at all — it would be RANKED.
                eprintln!("error: {e}");
                return 1;
            }
        }
    };

    let draft = Draft {
        text: &text,
        objective: args.objective.as_deref(),
        key: args.key.as_deref(),
        producer: args.producer.as_deref().unwrap_or(DEFAULT_PRODUCER),
        score: scored.as_ref(),
    };

    let landed = match item::land(&db, &draft, &ruler, args.create).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    let (id, replaced) = match &landed {
        Landed::Filed { id } => (id.clone(), None),
        Landed::Updated { id, replaced } => (id.clone(), Some(replaced.clone())),
    };

    if args.json {
        let out = serde_json::json!({
            "id": id,
            "replaced": replaced,
            "store": db.display().to_string(),
            "ruler": ruler.path.display().to_string(),
            "ruler_version": ruler.version,
            "scored_by": scored.as_ref().map(|s| s.scored_by.clone()),
            "value": scored.as_ref().map(|s| s.value),
            "capped_from": scored.as_ref().and_then(|s| s.capped_from),
            "axis": scored.as_ref().map(|s| s.axis.clone()),
            "cost": scored.as_ref().map(|s| s.cost.clone()),
            "vetted": false,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return 0;
    }

    match &landed {
        Landed::Filed { .. } => println!("filed {id}"),
        Landed::Updated { replaced, .. } => {
            println!(
                "updated {id} (supersedes {replaced} — same key `{}`)",
                draft.key.unwrap_or("")
            );
        }
    }
    match &scored {
        Some(s) => {
            println!(
                "  {} · {} ({} session-chunk(s)) · scored by {}",
                s.value_line(&ruler),
                s.cost,
                ruler.cost.chunks.get(&s.cost).copied().unwrap_or_default(),
                s.scored_by
            );
            if let Some(from) = s.capped_from {
                println!(
                    "  capped {from} -> {}: the ruler's top level needs a \
                     measurement and the item text quoted none",
                    s.value
                );
            }
            println!(
                "  UNVETTED, and not pullable: a machine scored it. Review it, \
                 then clear `Scored-by:` — that is the vetting."
            );
        }
        None => println!("  unscored: no Value line, so it sorts last until someone scores it"),
    }
    println!("  see it with: scripts/co-backlog.py --open");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_text_is_one_positional_and_flags_are_named() {
        let a = parse_args(&args(&[
            "decline p50 is 11s",
            "--objective",
            "one sweep",
            "--key",
            "bench-lane:x",
            "--no-score",
        ]))
        .unwrap();
        assert_eq!(a.text.as_deref(), Some("decline p50 is 11s"));
        assert_eq!(a.objective.as_deref(), Some("one sweep"));
        assert_eq!(a.key.as_deref(), Some("bench-lane:x"));
        assert!(a.no_score);
    }

    #[test]
    fn empty_or_missing_text_is_refused() {
        for bad in [vec![], args(&["   "]), args(&["--objective", "o"])] {
            assert!(
                parse_args(&bad).is_err(),
                "{bad:?} must not file an empty item"
            );
        }
    }

    #[test]
    fn an_unquoted_multi_word_item_is_refused_not_silently_truncated() {
        // The failure this catches: `svrn backlog add decline p50 is 11s`
        // filing an item that says only "decline".
        let err = parse_args(&args(&["decline", "p50", "is", "11s"])).unwrap_err();
        assert!(err.contains("quote the item"), "{err}");
    }

    #[test]
    fn an_unknown_flag_is_refused() {
        let err = parse_args(&args(&["t", "--vetted"])).unwrap_err();
        assert!(err.contains("--vetted"), "{err}");
    }

    #[test]
    fn a_flag_with_no_value_is_refused() {
        let err = parse_args(&args(&["t", "--objective"])).unwrap_err();
        assert!(err.contains("--objective"), "{err}");
    }

    // --- the refusal path, watched to fail ------------------------------
    //
    // A gate nobody has watched fail is not a gate (ARCH §18.1). The
    // claim being gated is: with no daemon, `add` files NOTHING — it
    // never quietly lands an unscored item as if it were scored. So the
    // same assertion is run in both directions against the same store
    // shape: dead daemon must leave the backlog empty, and --no-score
    // (which does not need a daemon) must leave exactly one item. If the
    // refusal ever regressed into a silent filing, the first test goes
    // red; if the assertion itself stopped being able to see a filing,
    // the second one does.

    async fn backlog_len(db: &std::path::Path) -> usize {
        corpus_engine_notes::NoteStore::open(db)
            .unwrap()
            .read_notes_by_related_entity("backlog", &["todo"])
            .await
            .unwrap()
            .len()
    }

    /// A port nothing listens on. `probe_daemon` gives up in 500ms.
    const DEAD_DAEMON: &str = "http://127.0.0.1:1";

    #[tokio::test]
    async fn a_dead_daemon_refuses_and_files_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("notes.db");
        corpus_engine_notes::NoteStore::open(&db).unwrap();

        let code = cmd_add(&args(&[
            "the daemon is down and this must not be filed as scored",
            "--db",
            db.to_str().unwrap(),
            "--daemon",
            DEAD_DAEMON,
        ]))
        .await;

        assert_eq!(code, 1, "a refusal must not exit 0");
        assert_eq!(
            backlog_len(&db).await,
            0,
            "the item was FILED despite the daemon being down — an unscored \
             item filed as scored would then be RANKED"
        );
    }

    #[tokio::test]
    async fn no_score_files_one_without_any_daemon() {
        // The other direction: the assertion above can see a filing.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("notes.db");
        corpus_engine_notes::NoteStore::open(&db).unwrap();

        let code = cmd_add(&args(&[
            "filed on purpose without a score",
            "--db",
            db.to_str().unwrap(),
            "--daemon",
            DEAD_DAEMON,
            "--no-score",
        ]))
        .await;

        assert_eq!(code, 0, "--no-score needs no daemon");
        assert_eq!(backlog_len(&db).await, 1);
    }

    #[tokio::test]
    async fn create_is_the_only_way_a_store_comes_into_existence() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("fresh").join("notes.db");

        // Without --create: refused, and nothing on disk.
        let refused = cmd_add(&args(&[
            "a first item",
            "--db",
            db.to_str().unwrap(),
            "--no-score",
        ]))
        .await;
        assert_eq!(refused, 1);
        assert!(!db.exists(), "a refusal must not leave a store behind");

        // With --create: the store exists and holds the item.
        let created = cmd_add(&args(&[
            "a first item",
            "--db",
            db.to_str().unwrap(),
            "--no-score",
            "--create",
        ]))
        .await;
        assert_eq!(created, 0);
        assert_eq!(backlog_len(&db).await, 1);
    }

    #[tokio::test]
    async fn a_missing_ruler_refuses_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("notes.db");
        corpus_engine_notes::NoteStore::open(&db).unwrap();

        let code = cmd_add(&args(&[
            "no ruler means no scale means no score",
            "--db",
            db.to_str().unwrap(),
            "--ruler",
            "/nonexistent/backlog-ruler.toml",
            "--no-score",
        ]))
        .await;

        assert_eq!(code, 2);
        assert_eq!(backlog_len(&db).await, 0, "nothing lands without a ruler");
    }
}
