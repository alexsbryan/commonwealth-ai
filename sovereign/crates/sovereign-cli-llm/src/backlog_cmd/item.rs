// SPDX-License-Identifier: AGPL-3.0-or-later
//! The item as it lands: the header block, the store path, and identity.
//!
//! A backlog item IS a notes-store todo carrying `related_entity=backlog`
//! and a header block. There is no backlog store — `scripts/co-backlog.py`
//! reads that same store and ranks it, and the ordering is derived at
//! every read, so an insert here is an O(1) append with nothing to
//! re-index (the priority-queue contract, order backlog-insert-system).

use std::path::{Path, PathBuf};

use corpus_engine_notes::{NoteRow, NoteScope, NoteSource, NoteStore};

use super::ruler::Ruler;
use super::score::Score;

/// The test override, spelled exactly as `scripts/co-backlog.py` spells
/// it (`notes_db_path()`). One name for one path, across two languages.
pub const DB_ENV: &str = "CO_BACKLOG_NOTES_DB";
/// The registered per-user data root (`quality/env-flags.toml`).
pub const DATA_DIR_ENV: &str = "SOVEREIGN_DATA_DIR";

/// The store, NEVER discovered from cwd (invariant 0f8abed1).
///
/// A cwd-sensitive resolver answers confidently from the wrong store:
/// measured on this host, the same query from the repo root and from
/// `$HOME` hit different databases — 68 notes versus 6811 — and exited 0
/// both times. So the path is named, in the same precedence
/// `co-backlog.py` uses, and the verb prints what it resolved.
pub fn notes_db_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Ok(v) = std::env::var(DB_ENV) {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    if let Ok(v) = std::env::var(DATA_DIR_ENV) {
        if !v.is_empty() {
            return PathBuf::from(v).join("notes.db");
        }
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".sovereign").join("notes.db")
}

/// Everything a producer supplies. Filling this in is the whole job of
/// the verb; the ruler decides the numbers.
pub struct Draft<'a> {
    pub text: &'a str,
    pub objective: Option<&'a str>,
    /// Producer identity, from essence (ARCH §7.5) — a lane name, not a
    /// counter or a row id. A repeat filing under the same key UPDATES
    /// the item it already filed.
    pub key: Option<&'a str>,
    pub producer: &'a str,
    pub score: Option<&'a Score>,
}

/// The header block, rendered in the ruler's declared key order.
///
/// Key ORDER comes from `quality/backlog-ruler.toml`'s `header_keys`, so
/// a key added to the format appears in one file and both languages
/// follow — the Rust writer here and the Python parser that reads it.
pub fn render_header(draft: &Draft<'_>, ruler: &Ruler) -> String {
    let mut fields: Vec<(&str, String)> = Vec::new();
    fields.push((
        "Objective",
        draft
            .objective
            .unwrap_or("unstated — filed without an anchor")
            .to_string(),
    ));
    match draft.score {
        Some(s) => {
            fields.push(("Value", s.value_line(ruler)));
            fields.push(("Cost", format!("{} (session-chunks)", s.cost)));
            fields.push(("Approach", s.approach.trim().to_string()));
        }
        None => {
            // --no-score. The absence is written down, not left blank:
            // an item with no Value renders unscored and sorts last,
            // which is the honest place for it (ARCH §18.3).
            fields.push(("Approach", "unknown — needs a design pass".to_string()));
        }
    }
    if let Some(k) = draft.key {
        fields.push(("Key", k.to_string()));
    }
    fields.push(("Producer", draft.producer.to_string()));
    if let Some(s) = draft.score {
        // The stamp that keeps this unvetted. co-backlog.py's vet()
        // treats its PRESENCE as disqualifying, so a machine-drafted
        // item cannot become pullable by looking complete — a person
        // clears the stamp, and that clearing IS the review.
        fields.push(("Scored-by", s.scored_by.clone()));
    }

    let order: Vec<&str> = ruler
        .format
        .header_keys
        .iter()
        .map(|s| s.as_str())
        .collect();
    let mut lines = Vec::new();
    for key in &order {
        if let Some((_, v)) = fields.iter().find(|(k, _)| k == key) {
            lines.push(format!("{key}: {v}"));
        }
    }
    // Anything the ruler does not declare would be parsed as malformed
    // by the renderer, so it is a bug here, not something to emit and
    // hope about.
    debug_assert!(
        fields.iter().all(|(k, _)| order.contains(k)),
        "emitting a header key the ruler does not declare"
    );
    lines.join("\n")
}

/// The full note body: header block, blank line, then the producer's own
/// words, verbatim. The item's text is never summarized — the score is a
/// draft over it, not a replacement for it.
pub fn render_body(draft: &Draft<'_>, ruler: &Ruler) -> String {
    let mut body = render_header(draft, ruler);
    body.push_str("\n\n");
    if let Some(s) = draft.score {
        body.push_str(&format!(
            "Scored against value ruler v{} ({}) by {}.",
            ruler.version,
            ruler.path.display(),
            s.scored_by
        ));
        if let Some(from) = s.capped_from {
            body.push_str(&format!(
                " The model scored {from}; the ruler's top level requires a \
                 measurement attached and the item text quoted none, so it was \
                 capped to {}.",
                s.value
            ));
        } else if !s.measurement.trim().is_empty() {
            body.push_str(&format!(
                " Measurement quoted from the item text: {}.",
                s.measurement.trim()
            ));
        }
        body.push_str(
            " This is a DRAFT: it renders greyed and cannot be pulled until a \
             person reviews it and clears `Scored-by:`.\n\n",
        );
    } else {
        body.push_str(
            "Filed unscored (--no-score). It has no Value line, so it sorts \
             last and cannot be pulled until someone scores it.\n\n",
        );
    }
    body.push_str(draft.text.trim());
    body.push('\n');
    body
}

/// The `Key:` line of an item body, if it has one. Reads the header
/// block only — the leading run of lines before the first blank — for
/// the same reason the renderer's parser does: prose that happens to
/// contain "Key:" is prose.
pub fn key_of(body: &str) -> Option<String> {
    body.split("\n\n")
        .next()?
        .lines()
        .find_map(|l| l.trim().strip_prefix("Key:").map(|v| v.trim().to_string()))
        .filter(|v| !v.is_empty())
}

/// Where an insert landed, so the verb can say which one happened.
#[derive(Debug)]
pub enum Landed {
    Filed { id: String },
    Updated { id: String, replaced: String },
}

/// File the item, or update the one this key already filed.
///
/// Identity is the producer's `Key` and nothing else (ARCH §7.5): a
/// repeat filing supersedes its predecessor rather than adding a
/// duplicate, so a CI lane that fails nightly leaves one item, not
/// thirty. With no key, every call files a new item — the caller is a
/// person, and people do not want their second thought to eat their
/// first.
pub async fn land(db: &Path, draft: &Draft<'_>, ruler: &Ruler) -> Result<Landed, String> {
    if !db.exists() {
        // Opening would CREATE an empty store. A fresh store at a wrong
        // path is the silent failure this whole path exists to avoid, so
        // absence is refused rather than papered over (ARCH §18.3).
        return Err(format!(
            "no notes store at {} — refusing to create one, because a fresh \
             store at the wrong path looks exactly like a working one. Name \
             the right store with --db, or set {DB_ENV}.",
            db.display()
        ));
    }
    let store = NoteStore::open(db).map_err(|e| format!("cannot open {}: {e}", db.display()))?;
    let body = render_body(draft, ruler);
    // A fixed session id, not a new env knob: the producer is already
    // recorded on the item's `Producer:` line, and an unregistered env
    // read would have to earn a row in quality/env-flags.toml to say
    // less than the header already does.
    let session_id = "backlog-add";

    let existing: Option<NoteRow> = match draft.key {
        None => None,
        Some(k) => store
            .read_notes_by_related_entity("backlog", &["todo"])
            .await
            .map_err(|e| format!("cannot read the backlog from {}: {e}", db.display()))?
            .into_iter()
            .filter(|r| key_of(&r.content).as_deref() == Some(k))
            // Newest wins if a key somehow has more than one live item;
            // the older ones are left alone rather than mass-retired.
            .max_by(|a, b| a.created_at.cmp(&b.created_at)),
    };

    let id = store
        .write_note_with_source(
            "todo",
            &body,
            Vec::new(),
            Vec::new(),
            session_id,
            NoteScope::Global,
            None,
            Some("backlog"),
            NoteSource::Agent,
            existing.as_ref().map(|r| r.id.as_str()),
        )
        .await
        .map_err(|e| format!("the write failed: {e}"))?;

    match existing {
        None => Ok(Landed::Filed { id }),
        Some(prev) => {
            store
                .retire_by_id(
                    &prev.id,
                    &format!("superseded by {id}: same producer key, re-filed"),
                )
                .await
                .map_err(|e| {
                    format!(
                        "filed {id}, but could not retire the item it replaces ({}): {e}. \
                         The backlog now shows BOTH — retire {} by hand.",
                        prev.id, prev.id
                    )
                })?;
            Ok(Landed::Updated {
                id,
                replaced: prev.id,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog_cmd::score::parse;

    fn ruler() -> Ruler {
        Ruler::load(None).expect("the repo's own ruler must load")
    }

    fn a_score(ruler: &Ruler) -> Score {
        let mut s = parse(
            r#"{"value":4,"axis":"A","rationale":"A Grounded: cuts wrong-accepts",
                "approach":"extend the existing holdings gate","cost":"S",
                "measurement":""}"#,
            ruler,
        )
        .unwrap();
        s.scored_by = "commonwealth/primary".to_string();
        s
    }

    #[test]
    fn every_emitted_key_is_one_the_ruler_declares() {
        // The cross-language contract: this Rust writes the header and
        // scripts/co-backlog.py parses it, treating an unrecognized key
        // as MALFORMED. So every key emitted here must be in the ruler's
        // declared list — which is also the list the parser reads.
        let r = ruler();
        let s = a_score(&r);
        for draft in [
            Draft {
                text: "t",
                objective: Some("o"),
                key: Some("bench-lane:retrieval-prod"),
                producer: "svrn backlog add",
                score: Some(&s),
            },
            Draft {
                text: "t",
                objective: None,
                key: None,
                producer: "svrn backlog add",
                score: None,
            },
        ] {
            let header = render_header(&draft, &r);
            for line in header.lines() {
                let key = line.split(':').next().unwrap();
                assert!(
                    r.format.header_keys.iter().any(|k| k == key),
                    "emitted key {key:?} is not declared in {}",
                    r.path.display()
                );
            }
        }
    }

    #[test]
    fn a_scored_item_carries_the_stamp_that_keeps_it_unvetted() {
        let r = ruler();
        let s = a_score(&r);
        let body = render_body(
            &Draft {
                text: "the raw item text",
                objective: Some("native grounding H0"),
                key: None,
                producer: "svrn backlog add",
                score: Some(&s),
            },
            &r,
        );
        assert!(body.contains("Scored-by: commonwealth/primary"), "{body}");
        assert!(
            body.contains("Value: 4 — A Grounded: cuts wrong-accepts"),
            "{body}"
        );
        assert!(
            body.contains("the raw item text"),
            "the item's own words survive"
        );
    }

    #[test]
    fn an_unscored_item_carries_no_stamp_and_no_value() {
        let r = ruler();
        let body = render_body(
            &Draft {
                text: "the raw item text",
                objective: None,
                key: None,
                producer: "svrn backlog add",
                score: None,
            },
            &r,
        );
        assert!(!body.contains("Scored-by:"), "{body}");
        assert!(
            !body.contains("Value:"),
            "an unscored item must not claim a value"
        );
    }

    #[test]
    fn the_key_is_read_from_the_header_not_from_the_prose() {
        assert_eq!(
            key_of("Objective: o\nKey: bench-lane:foo\n\nprose\nKey: not-this\n"),
            Some("bench-lane:foo".to_string())
        );
        assert_eq!(key_of("Objective: o\n\nprose\nKey: not-this\n"), None);
        assert_eq!(key_of("Objective: o\nKey:   \n\nprose"), None);
    }

    #[tokio::test]
    async fn an_absent_store_is_refused_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope").join("notes.db");
        let r = ruler();
        let err = land(
            &missing,
            &Draft {
                text: "t",
                objective: None,
                key: None,
                producer: "test",
                score: None,
            },
            &r,
        )
        .await
        .expect_err("an absent store must refuse");
        assert!(err.contains("refusing to create"), "{err}");
        assert!(!missing.exists(), "refusing must not leave a store behind");
    }

    #[tokio::test]
    async fn a_repeat_key_updates_instead_of_duplicating() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("notes.db");
        NoteStore::open(&db).unwrap(); // the store exists; land() refuses otherwise
        let r = ruler();
        let draft = |text: &'static str| Draft {
            text,
            objective: Some("o"),
            key: Some("bench-lane:retrieval-prod"),
            producer: "sovereign-ci-bench.sh",
            score: None,
        };

        let first = land(&db, &draft("failed once"), &r).await.unwrap();
        let first_id = match first {
            Landed::Filed { id } => id,
            Landed::Updated { .. } => panic!("the first filing cannot be an update"),
        };

        let second = land(&db, &draft("failed again"), &r).await.unwrap();
        match second {
            Landed::Updated { replaced, .. } => assert_eq!(replaced, first_id),
            Landed::Filed { .. } => panic!("a repeat key must update, not duplicate"),
        }

        // And the backlog holds ONE live item, carrying the newer text.
        let store = NoteStore::open(&db).unwrap();
        let live = store
            .read_notes_by_related_entity("backlog", &["todo"])
            .await
            .unwrap();
        assert_eq!(live.len(), 1, "a repeat filing must not leave two items");
        assert!(live[0].content.contains("failed again"));
    }

    #[tokio::test]
    async fn a_different_key_files_a_second_item() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("notes.db");
        NoteStore::open(&db).unwrap();
        let r = ruler();
        for key in ["bench-lane:a", "bench-lane:b"] {
            land(
                &db,
                &Draft {
                    text: "t",
                    objective: Some("o"),
                    key: Some(key),
                    producer: "test",
                    score: None,
                },
                &r,
            )
            .await
            .unwrap();
        }
        let store = NoteStore::open(&db).unwrap();
        let live = store
            .read_notes_by_related_entity("backlog", &["todo"])
            .await
            .unwrap();
        assert_eq!(live.len(), 2, "two lanes are two items");
    }

    #[tokio::test]
    async fn no_key_means_every_filing_is_its_own_item() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("notes.db");
        NoteStore::open(&db).unwrap();
        let r = ruler();
        for _ in 0..2 {
            land(
                &db,
                &Draft {
                    text: "a person filing twice",
                    objective: Some("o"),
                    key: None,
                    producer: "test",
                    score: None,
                },
                &r,
            )
            .await
            .unwrap();
        }
        let store = NoteStore::open(&db).unwrap();
        let live = store
            .read_notes_by_related_entity("backlog", &["todo"])
            .await
            .unwrap();
        assert_eq!(live.len(), 2, "without a key, nothing is superseded");
    }
}
