// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::wrapped::Turn;

fn ts(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
}

fn turn(ts_str: &str, is_user: bool, chunk_id: u64, first_line: &str) -> Turn {
    Turn {
        ts: Some(ts(ts_str)),
        is_user,
        words: first_line.split_whitespace().count() as u64,
        chunk_id,
        first_line: first_line.to_string(),
    }
}

/// A fully-parsed conversation: every chunk yielded a turn, so the
/// chunk sequence is exactly the turns' chunks. Use `doc_with_chunks`
/// for the realistic case where most chunks parse to nothing.
fn doc(uuid: &str, turns: Vec<Turn>) -> ConvDoc {
    let mut chunk_ids: Vec<u64> = turns.iter().map(|t| t.chunk_id).collect();
    chunk_ids.dedup();
    doc_with_chunks(uuid, turns, chunk_ids)
}

fn doc_with_chunks(uuid: &str, turns: Vec<Turn>, chunk_ids: Vec<u64>) -> ConvDoc {
    ConvDoc {
        conv_uuid: uuid.to_string(),
        title: Some(format!("title-{uuid}")),
        turns,
        chunk_ids,
    }
}

fn content(pairs: &[(u64, &str)]) -> HashMap<u64, String> {
    pairs.iter().map(|(id, c)| (*id, c.to_string())).collect()
}

fn node(conv: &str, entities: &[&str], chunks: &[u64]) -> RaptorNode {
    RaptorNode {
        conv_uuid: conv.to_string(),
        summary: format!("summary of {conv}"),
        entities: entities.iter().map(|s| s.to_string()).collect(),
        chunk_ids: chunks.to_vec(),
        coherence: 0.8,
    }
}

/// Orthonormal-ish unit vectors so cosines are exactly predictable.
fn vec3(x: f32, y: f32, z: f32) -> Vec<f32> {
    vec![x, y, z]
}

// ─── citation resolution ─────────────────────────────────────────────

#[test]
fn citation_prefers_the_verbatim_form_when_present() {
    let c = content(&[(1, "we talked about Taoism at length")]);
    let cite = resolve_entity_citation("Taoism", &[1], &c).unwrap();
    assert_eq!(cite.text, "Taoism");
    assert_eq!(cite.chunk_id, 1);
    assert_eq!(&c[&1][cite.char_start..cite.char_end], "Taoism");
}

#[test]
fn citation_recovers_llm_normalised_names_and_quotes_the_archive() {
    // The enrichment pass writes "Dodd Frank Act"; the archive says
    // "Dodd-Frank Act". The citation must carry the ARCHIVE's form, or
    // the audit (correctly) refuses it.
    let c = content(&[(7, "the Dodd-Frank Act reshaped the banks")]);
    let cite = resolve_entity_citation("Dodd Frank Act", &[7], &c).unwrap();
    assert_eq!(cite.text, "Dodd-Frank Act");
    assert!(c[&7].contains(&cite.text));
    assert_eq!(&c[&7][cite.char_start..cite.char_end], "Dodd-Frank Act");
}

#[test]
fn citation_is_case_insensitive_but_quotes_original_casing() {
    let c = content(&[(2, "practising wu wei daily")]);
    let cite = resolve_entity_citation("Wu Wei", &[2], &c).unwrap();
    assert_eq!(cite.text, "wu wei");
}

#[test]
fn citation_handles_multibyte_content_without_slicing_mid_char() {
    // Punctuation differs (space vs hyphen) so this takes the normalised
    // path, and both the haystack prefix and the match itself carry
    // multibyte chars — the case where a byte-offset bug slices a char
    // in half and panics.
    let c = content(&[(3, "café culture — the Montréal-Métro specifically")]);
    let cite = resolve_entity_citation("Montréal Métro", &[3], &c).unwrap();
    assert_eq!(cite.text, "Montréal-Métro");
    assert_eq!(&c[&3][cite.char_start..cite.char_end], "Montréal-Métro");
}

#[test]
fn citation_normalisation_does_not_fold_diacritics() {
    // Deliberate: folding "Montreal" onto "Montréal" would let a theme
    // be quoted with a form the archive never used. The theme is dropped
    // instead — losing a card beats inventing a quote.
    let c = content(&[(3, "Montréal in the winter")]);
    assert!(resolve_entity_citation("Montreal", &[3], &c).is_none());
}

#[test]
fn citation_absent_when_the_theme_never_appears() {
    let c = content(&[(1, "nothing relevant here")]);
    assert!(resolve_entity_citation("Taoism", &[1], &c).is_none());
    // Unknown chunk id must not panic.
    assert!(resolve_entity_citation("Taoism", &[404], &c).is_none());
}

#[test]
fn uncitable_themes_are_dropped_from_the_index_entirely() {
    // This is the enforcement point for commitment 2: a fold cannot emit
    // a claim it has no quote for, because the theme never reaches it.
    let docs = vec![doc("c1", vec![turn("2025-01-01 10:00", true, 1, "hi")])];
    let c = content(&[(1, "we discussed Taoism")]);
    let nodes = vec![node("c1", &["Taoism", "Neverland"], &[1])];
    let idx = ThemeIndex::from_enrichment(&nodes, &docs, &c, &HashMap::new());
    assert!(idx.prior.contains_key("taoism"));
    assert!(!idx.prior.contains_key("neverland"));
    assert!(idx.mentions.iter().all(|m| m.key == "taoism"));
}

// ─── distinctiveness ─────────────────────────────────────────────────

#[test]
fn log_odds_ranks_over_indexing_above_raw_frequency() {
    // "common" dominates BOTH the group and the archive — it is the
    // baseline showing through, and frequency ranking would put it top.
    // "rare" is uncommon overall but concentrated here.
    let prior: HashMap<String, usize> = [("common".into(), 100), ("rare".into(), 6)]
        .into_iter()
        .collect();
    let group: HashMap<String, usize> = [("common".into(), 30), ("rare".into(), 6)]
        .into_iter()
        .collect();
    let rest: HashMap<String, usize> = [("common".into(), 70), ("rare".into(), 0)]
        .into_iter()
        .collect();

    // Frequency would say "common".
    assert!(group["common"] > group["rare"]);
    // Distinctiveness says "rare".
    let z = log_odds(&group, &rest, &prior);
    assert!(
        z["rare"] > z["common"],
        "expected rare to out-rank common: {z:?}"
    );
}

#[test]
fn log_odds_is_symmetric_when_a_theme_is_evenly_spread() {
    let prior: HashMap<String, usize> = [("even".into(), 20), ("other".into(), 20)]
        .into_iter()
        .collect();
    let group: HashMap<String, usize> = [("even".into(), 10), ("other".into(), 10)]
        .into_iter()
        .collect();
    let rest: HashMap<String, usize> = [("even".into(), 10), ("other".into(), 10)]
        .into_iter()
        .collect();
    let z = log_odds(&group, &rest, &prior);
    assert!(z["even"].abs() < 1e-9, "got {}", z["even"]);
}

#[test]
fn log_odds_skips_the_degenerate_single_theme_vocabulary() {
    // One theme that IS the entire vocabulary has no baseline to be
    // unusual against — y + α equals n + α₀ exactly and the log-odds is
    // undefined. It is dropped rather than reported as a bogus score.
    let prior: HashMap<String, usize> = [("only".into(), 20)].into_iter().collect();
    let group: HashMap<String, usize> = [("only".into(), 10)].into_iter().collect();
    let rest: HashMap<String, usize> = [("only".into(), 10)].into_iter().collect();
    assert!(log_odds(&group, &rest, &prior).is_empty());
}

// ─── obsessions ──────────────────────────────────────────────────────

fn quarterly_fixture() -> (Vec<RaptorNode>, Vec<ConvDoc>, HashMap<u64, String>) {
    // "Rust" is the archive's baseline — everywhere, every quarter.
    // "Taoism" is concentrated in Q2 only.
    let mut nodes = Vec::new();
    let mut docs = Vec::new();
    let mut pairs: Vec<(u64, String)> = Vec::new();
    let mut chunk = 1u64;
    for (conv, when) in [
        ("q1a", "2025-01-05 10:00"),
        ("q1b", "2025-02-05 10:00"),
        ("q1c", "2025-03-05 10:00"),
        ("q2a", "2025-04-05 10:00"),
        ("q2b", "2025-05-05 10:00"),
        ("q2c", "2025-06-05 10:00"),
    ] {
        let entities: Vec<&str> = if conv.starts_with("q2") {
            vec!["Rust", "Taoism"]
        } else {
            vec!["Rust"]
        };
        pairs.push((chunk, "we discussed Rust and Taoism".to_string()));
        nodes.push(node(conv, &entities, &[chunk]));
        docs.push(doc(conv, vec![turn(when, true, chunk, "opening line")]));
        chunk += 1;
    }
    let c = pairs.into_iter().collect();
    (nodes, docs, c)
}

#[test]
fn obsessions_ranks_the_quarter_specific_theme_above_the_baseline() {
    let (nodes, docs, c) = quarterly_fixture();
    let idx = ThemeIndex::from_enrichment(&nodes, &docs, &c, &HashMap::new());
    let card = fold_obsessions(&idx).unwrap();
    assert_eq!(card.quarters.len(), 2);

    let q2 = card
        .quarters
        .iter()
        .find(|q| q.quarter == "2025-Q2")
        .unwrap();
    // Rust appears in just as many Q2 conversations as Taoism, so a
    // count-based ranking is a coin flip. Distinctiveness is not.
    assert_eq!(q2.topics[0].text, "Taoism");
    assert!(q2.topics[0].distinctiveness > 0.0);
    let rust = q2.topics.iter().find(|t| t.text == "Rust").unwrap();
    assert!(
        q2.topics[0].distinctiveness > rust.distinctiveness,
        "Taoism {} should beat Rust {}",
        q2.topics[0].distinctiveness,
        rust.distinctiveness
    );
    assert!(card.derivation.iter().any(|d| d.contains("log-odds")));
}

#[test]
fn obsessions_quotes_are_verbatim_in_their_cited_chunk() {
    let (nodes, docs, c) = quarterly_fixture();
    let idx = ThemeIndex::from_enrichment(&nodes, &docs, &c, &HashMap::new());
    let card = fold_obsessions(&idx).unwrap();
    for q in &card.quarters {
        for t in &q.topics {
            let chunk = &c[&t.sample.chunk_id];
            assert!(
                chunk.contains(&t.sample.text),
                "{:?} not verbatim in chunk {}",
                t.sample.text,
                t.sample.chunk_id
            );
        }
    }
}

#[test]
fn obsessions_absent_without_themes() {
    let idx = ThemeIndex::from_enrichment(&[], &[], &HashMap::new(), &HashMap::new());
    assert!(fold_obsessions(&idx).is_none());
}

// ─── the night shift ─────────────────────────────────────────────────

#[test]
fn utc_offset_is_inferred_from_the_sleep_trough() {
    // Turns everywhere EXCEPT 08:00–11:00 UTC. The quietest window
    // centres on 09:30 → placing that at 03:00 local gives UTC-7 (the
    // measured answer for the reference archive).
    let mut turns = Vec::new();
    for hour in 0..24 {
        if (8..12).contains(&hour) {
            continue;
        }
        for m in 0..3 {
            turns.push(turn(
                &format!("2025-01-0{} {hour:02}:{:02}", (m % 3) + 1, m * 5),
                true,
                1,
                "line",
            ));
        }
    }
    let docs = vec![doc("c1", turns)];
    let (offset, derivation) = infer_utc_offset(&docs, None);
    assert_eq!(offset, -7);
    assert!(derivation.iter().any(|d| d.contains("quietest")));
}

#[test]
fn utc_offset_prefers_the_host_when_supplied() {
    let (offset, derivation) = infer_utc_offset(&[], Some(5));
    assert_eq!(offset, 5);
    assert!(derivation[0].contains("host OS"));
}

#[test]
fn utc_offset_degrades_to_utc_without_timestamps() {
    let (offset, _) = infer_utc_offset(&[], None);
    assert_eq!(offset, 0);
}

#[test]
fn night_shift_bands_by_local_hour_not_utc() {
    // Every conversation at 02:00 UTC. With offset -7 that is 19:00
    // local — "evening", NOT "late night". This inversion is the bug the
    // naive UTC reading shipped with.
    let mut nodes = Vec::new();
    let mut docs = Vec::new();
    let mut pairs = Vec::new();
    for i in 0..12u64 {
        let chunk = i + 1;
        pairs.push((chunk, "we discussed Taoism and Entropy".to_string()));
        nodes.push(node(
            &format!("c{i}"),
            &["Taoism", "Entropy"],
            &[chunk],
        ));
        docs.push(doc(
            &format!("c{i}"),
            vec![turn("2025-01-05 02:00", true, chunk, "opening")],
        ));
    }
    let c: HashMap<u64, String> = pairs.into_iter().collect();
    let idx = ThemeIndex::from_enrichment(&nodes, &docs, &c, &HashMap::new());

    let utc = fold_night_shift(&idx, &docs, Some(0));
    let shifted = fold_night_shift(&idx, &docs, Some(-7));
    // Single band ⇒ card withheld (nothing to contrast), but the band
    // that WOULD be populated differs, which is the point.
    assert!(utc.is_none() && shifted.is_none());

    let band_of = |offset: i32| {
        let local = (2 + offset).rem_euclid(24) as u32;
        BANDS
            .iter()
            .find(|(_, lo, hi)| local >= *lo && local <= *hi)
            .unwrap()
            .0
    };
    assert_eq!(band_of(0), "late night");
    assert_eq!(band_of(-7), "evening");
}

#[test]
fn night_shift_contrasts_two_populated_bands() {
    let mut nodes = Vec::new();
    let mut docs = Vec::new();
    let mut pairs = Vec::new();
    let mut chunk = 1u64;
    // 24 morning conversations about Plyometrics, 24 late-night ones
    // about Jung — one theme-mention each, so both bands clear
    // BAND_MIN_MENTIONS with margin.
    for (hour, theme, tag) in [("09:00", "Plyometrics", "m"), ("02:00", "Jung", "n")] {
        for i in 0..24u64 {
            let conv = format!("{tag}{i}");
            pairs.push((chunk, format!("we discussed {theme} at length")));
            nodes.push(node(&conv, &[theme], &[chunk]));
            docs.push(doc(
                &conv,
                vec![turn(&format!("2025-01-05 {hour}"), true, chunk, "opening")],
            ));
            chunk += 1;
        }
    }
    let c: HashMap<u64, String> = pairs.into_iter().collect();
    let idx = ThemeIndex::from_enrichment(&nodes, &docs, &c, &HashMap::new());
    let card = fold_night_shift(&idx, &docs, Some(0)).unwrap();
    assert_eq!(card.utc_offset_hours, 0);
    assert_eq!(card.bands.len(), 2);

    let night = card.bands.iter().find(|b| b.name == "late night").unwrap();
    let morning = card.bands.iter().find(|b| b.name == "morning").unwrap();
    assert_eq!(night.topics[0].text, "Jung");
    assert_eq!(morning.topics[0].text, "Plyometrics");
    for b in &card.bands {
        for t in &b.topics {
            assert!(c[&t.sample.chunk_id].contains(&t.sample.text));
        }
    }
}

// ─── the turn ────────────────────────────────────────────────────────

/// Eight chunks: 1–5 about one thing, 6–8 about an orthogonal thing.
/// The seam is between chunk 5 and chunk 6.
fn pivot_fixture() -> (Vec<ConvDoc>, HashMap<u64, Vec<f32>>, HashMap<u64, String>) {
    let mut turns = Vec::new();
    let mut emb = HashMap::new();
    let mut pairs = Vec::new();
    for chunk in 1..=8u64 {
        let before_seam = chunk <= 5;
        let line = if before_seam {
            format!("question {chunk} about interest rates")
        } else {
            format!("question {chunk} about human consciousness")
        };
        turns.push(turn(
            &format!("2025-01-05 {:02}:00", 9 + chunk),
            true,
            chunk,
            &line,
        ));
        pairs.push((chunk, format!("### [2025-01-05] user\n\n{line}")));
        // Tiny jitter keeps adjacent cosines just under 1.0 so the
        // conversation has a real median to fall below.
        let j = chunk as f32 * 0.01;
        emb.insert(
            chunk,
            if before_seam {
                vec3(1.0, j, 0.0)
            } else {
                vec3(j, 0.0, 1.0)
            },
        );
    }
    let docs = vec![doc("c1", turns)];
    (docs, emb, pairs.into_iter().collect())
}

#[test]
fn turn_finds_the_seam_and_quotes_both_sides() {
    let (docs, emb, c) = pivot_fixture();
    let card = fold_turn(&docs, &emb, &c).unwrap();
    assert_eq!(card.pivots.len(), 1);
    let p = &card.pivots[0];
    assert_eq!(p.seam_index, 5, "seam sits between chunk 5 and 6");
    assert_eq!(p.chunk_count, 8);
    assert!(p.drop >= TURN_MIN_DROP);
    assert!(p.cosine < p.conv_median);
    assert!(p.before.as_ref().unwrap().text.contains("interest rates"));
    assert!(p
        .after
        .as_ref()
        .unwrap()
        .text
        .contains("human consciousness"));
    // Both quotes verbatim in their cited chunks.
    for e in [p.before.as_ref().unwrap(), p.after.as_ref().unwrap()] {
        assert!(c[&e.chunk_id].contains(&e.text));
    }
    assert!(!card.derivation.is_empty());
}

#[test]
fn turn_ignores_conversations_that_never_change_subject() {
    let (docs, mut emb, c) = pivot_fixture();
    // Flatten the geometry: every chunk on the same topic.
    for chunk in 1..=8u64 {
        emb.insert(chunk, vec3(1.0, chunk as f32 * 0.01, 0.0));
    }
    assert!(fold_turn(&docs, &emb, &c).is_none());
}

#[test]
fn turn_ignores_conversations_below_the_chunk_floor() {
    let (docs, emb, c) = pivot_fixture();
    let short = vec![doc(
        "c1",
        docs[0].turns.iter().take(4).cloned().collect::<Vec<_>>(),
    )];
    assert!(fold_turn(&short, &emb, &c).is_none());
}

#[test]
fn turn_absent_without_embeddings() {
    let (docs, _, c) = pivot_fixture();
    assert!(fold_turn(&docs, &HashMap::new(), &c).is_none());
}

/// Twelve chunks, seam between 6 and 7, but only two of the twelve
/// carry a parseable `### [ts] role` header. This is the SHAPE OF THE
/// REAL ARCHIVE: measured 2026-07-26, 13,373 of 16,404 chunks parse to
/// zero turns, which left the turn-derived sequence below the chunk
/// floor for 290 of 425 eligible conversations.
fn sparse_turn_fixture(
    turn_chunks: &[u64],
) -> (Vec<ConvDoc>, HashMap<u64, Vec<f32>>, HashMap<u64, String>) {
    let mut emb = HashMap::new();
    let mut pairs = Vec::new();
    let mut turns = Vec::new();
    for chunk in 1..=12u64 {
        let before_seam = chunk <= 6;
        let line = if before_seam {
            format!("question {chunk} about interest rates")
        } else {
            format!("question {chunk} about human consciousness")
        };
        // Every chunk has text and an embedding...
        pairs.push((chunk, line.clone()));
        let j = chunk as f32 * 0.01;
        emb.insert(
            chunk,
            if before_seam {
                vec3(1.0, j, 0.0)
            } else {
                vec3(j, 0.0, 1.0)
            },
        );
        // ...but only a few parsed into turns.
        if turn_chunks.contains(&chunk) {
            turns.push(turn(
                &format!("2025-03-0{} 10:00", 1 + chunk / 6),
                true,
                chunk,
                &line,
            ));
        }
    }
    let docs = vec![doc_with_chunks("c1", turns, (1..=12).collect())];
    (docs, emb, pairs.into_iter().collect())
}

#[test]
fn turn_reads_geometry_from_chunks_not_from_parsed_turns() {
    let (docs, emb, c) = sparse_turn_fixture(&[6, 7]);
    // Precondition: the turn-derived sequence this fold used to build
    // would have been 2 chunks long — far below the floor.
    assert_eq!(docs[0].turns.len(), 2);
    assert_eq!(docs[0].chunk_ids.len(), 12);

    let card = fold_turn(&docs, &emb, &c).expect("seam is visible from the chunk sequence");
    let p = &card.pivots[0];
    assert_eq!(p.seam_index, 6, "seam sits between chunk 6 and 7");
    assert_eq!(p.chunk_count, 12, "the whole conversation, not the parsed 2");
    assert!(p.before.as_ref().unwrap().text.contains("interest rates"));
    assert!(p
        .after
        .as_ref()
        .unwrap()
        .text
        .contains("human consciousness"));
}

#[test]
fn turn_quotes_a_turn_near_the_seam_when_the_seam_chunk_itself_is_unparsed() {
    // Nothing at 6 or 7; the nearest parsed turns are one chunk out on
    // each side, inside the quote window.
    let (docs, emb, c) = sparse_turn_fixture(&[5, 8]);
    let card = fold_turn(&docs, &emb, &c).expect("quotes recovered from within the window");
    let p = &card.pivots[0];
    assert_eq!(p.seam_index, 6);
    assert_eq!(p.before.as_ref().unwrap().chunk_id, 5);
    assert_eq!(p.after.as_ref().unwrap().chunk_id, 8);
}

#[test]
fn turn_reaches_across_an_unparsed_run_because_it_can_hold_no_turn() {
    // The only parsed turns sit at the conversation's ends, five chunks
    // either side of the seam. Quoting them is still exactly right: a
    // chunk parses to no turn precisely when it carries no `### [ts]
    // role` header — it is a mid-answer continuation fragment — so no
    // user turn can be hiding in the run between. These ARE the last
    // thing said before and the first said after.
    let (docs, emb, c) = sparse_turn_fixture(&[1, 12]);
    let card = fold_turn(&docs, &emb, &c).expect("distance is not doubt");
    let p = &card.pivots[0];
    assert_eq!(p.before.as_ref().unwrap().chunk_id, 1);
    assert_eq!(p.after.as_ref().unwrap().chunk_id, 12);
}

#[test]
fn turn_quotes_the_nearest_turn_on_each_side_not_merely_any() {
    // Three candidates before the seam and two after: the card must pick
    // the LAST before and the FIRST after, not the outermost.
    let (docs, emb, c) = sparse_turn_fixture(&[1, 3, 5, 9, 12]);
    let p = &fold_turn(&docs, &emb, &c).unwrap().pivots[0];
    assert_eq!(p.before.as_ref().unwrap().chunk_id, 5, "last before the seam");
    assert_eq!(p.after.as_ref().unwrap().chunk_id, 9, "first after the seam");
}

#[test]
fn turn_skips_unembedded_chunks_instead_of_dropping_the_conversation() {
    let (docs, mut emb, c) = sparse_turn_fixture(&[6, 7]);
    // One chunk never got an embedding. Old behaviour: the whole
    // conversation is disqualified. New: that chunk drops out and the
    // remaining 11 still carry the seam.
    emb.remove(&2);
    let card = fold_turn(&docs, &emb, &c).expect("one hole does not disqualify a conversation");
    assert_eq!(card.pivots[0].chunk_count, 11);
    assert_eq!(card.pivots[0].seam_index, 5, "seam index shifts with the hole");
}

// ─── the question you keep asking ────────────────────────────────────

#[test]
fn recurring_ranks_the_long_running_thread_above_the_burst() {
    let mut docs = Vec::new();
    let mut emb = HashMap::new();
    let mut pairs = Vec::new();
    let mut chunk = 1u64;

    // Thread A: 3 askings spread over ~10 months.
    for (i, when) in ["2025-01-05 10:00", "2025-06-05 10:00", "2025-11-05 10:00"]
        .iter()
        .enumerate()
    {
        let line = format!("what are the best cities for live music, take {i}");
        pairs.push((chunk, format!("### [x] user\n\n{line}")));
        docs.push(doc(&format!("a{i}"), vec![turn(when, true, chunk, &line)]));
        emb.insert(chunk, vec3(1.0, i as f32 * 0.02, 0.0));
        chunk += 1;
    }
    // Thread B: 3 askings inside one week — same count, no distance.
    for (i, when) in ["2025-03-01 10:00", "2025-03-03 10:00", "2025-03-05 10:00"]
        .iter()
        .enumerate()
    {
        let line = format!("how do I deploy this service, take {i}");
        pairs.push((chunk, format!("### [x] user\n\n{line}")));
        docs.push(doc(&format!("b{i}"), vec![turn(when, true, chunk, &line)]));
        emb.insert(chunk, vec3(0.0, i as f32 * 0.02, 1.0));
        chunk += 1;
    }
    let c: HashMap<u64, String> = pairs.into_iter().collect();

    let card = fold_recurring(&docs, &emb, &c).unwrap();
    assert_eq!(card.threads.len(), 2);
    let top = &card.threads[0];
    assert_eq!(top.conversations, 3);
    assert!(
        top.span_days > 300,
        "the returned-to thread must outrank the burst, got {} days",
        top.span_days
    );
    assert!(top.askings[0].excerpt.text.contains("live music"));
    // Askings are oldest-first and verbatim.
    let dates: Vec<&str> = top.askings.iter().map(|a| a.date.as_str()).collect();
    let mut sorted = dates.clone();
    sorted.sort_unstable();
    assert_eq!(dates, sorted);
    for a in &top.askings {
        assert!(c[&a.excerpt.chunk_id].contains(&a.excerpt.text));
    }
}

#[test]
fn recurring_needs_more_than_two_askings() {
    let mut docs = Vec::new();
    let mut emb = HashMap::new();
    let mut pairs = Vec::new();
    for (i, when) in ["2025-01-05 10:00", "2025-06-05 10:00"].iter().enumerate() {
        let chunk = i as u64 + 1;
        let line = format!("asked twice only, take {i}");
        pairs.push((chunk, format!("### [x] user\n\n{line}")));
        docs.push(doc(&format!("a{i}"), vec![turn(when, true, chunk, &line)]));
        emb.insert(chunk, vec3(1.0, 0.0, 0.0));
    }
    let c: HashMap<u64, String> = pairs.into_iter().collect();
    assert!(fold_recurring(&docs, &emb, &c).is_none());
}

// ─── the cast ────────────────────────────────────────────────────────

#[test]
fn cast_keeps_only_better_than_chance_links() {
    // Alice and Acme genuinely travel together (3 shared conversations
    // out of 3 each). Ubiquitous appears in EVERY conversation, so its
    // overlap with anything is exactly what chance predicts — PMI ≈ 0
    // and the edge is dropped. Under the old rule (≥2 shared) it would
    // have linked to everything, which is how the graph went dense.
    let mut nodes = Vec::new();
    let mut docs = Vec::new();
    let mut pairs = Vec::new();
    for i in 0..6u64 {
        let conv = format!("c{i}");
        let chunk = i + 1;
        pairs.push((chunk, "Alice of Acme, and Ubiquitous too".to_string()));
        let entities: Vec<&str> = if i < 3 {
            vec!["Alice", "Acme", "Ubiquitous"]
        } else {
            vec!["Ubiquitous"]
        };
        nodes.push(node(&conv, &entities, &[chunk]));
        docs.push(doc(
            &conv,
            vec![turn("2025-01-05 10:00", true, chunk, "opening")],
        ));
    }
    let c: HashMap<u64, String> = pairs.into_iter().collect();
    let idx = ThemeIndex::from_enrichment(&nodes, &docs, &c, &HashMap::new());
    let card = fold_cast(&idx).unwrap();

    assert_eq!(card.nodes.len(), 3);
    assert_eq!(card.edges.len(), 1, "only Alice–Acme beats chance");
    let e = &card.edges[0];
    assert_eq!(e.co_conversations, 3);
    assert!(e.pmi > 0.0);
    assert_eq!(e.first_date, "2025-01-05");

    let ubiquitous = card
        .nodes
        .iter()
        .find(|n| n.canonical_name == "Ubiquitous")
        .unwrap();
    assert_eq!(
        ubiquitous.conversations, 6,
        "most frequent theme in the archive…"
    );
    assert_eq!(ubiquitous.degree, 0, "…and yet connects nothing");
    assert!(card.derivation.iter().any(|d| d.contains("betweenness")));
}

#[test]
fn cast_bridging_beats_frequency_for_the_connector() {
    // Two clusters joined only through Bridge. Bridge appears in fewer
    // conversations than the cluster members but carries every path
    // between them — the node the card should make big.
    const TEXT: &str = "Ledger Loom Reef Relay Bridge together";
    let mut nodes = Vec::new();
    let mut docs = Vec::new();
    let mut pairs = Vec::new();
    let mut chunk = 1u64;
    let add = |conv: String, members: Vec<&str>, chunk: u64| {
        (
            node(&conv, &members, &[chunk]),
            doc(&conv, vec![turn("2025-01-05 10:00", true, chunk, "opening")]),
        )
    };
    // Five conversations per cluster…
    for i in 0..5 {
        for (group, members) in [
            ("L", vec!["Ledger", "Loom"]),
            ("R", vec!["Reef", "Relay"]),
        ] {
            pairs.push((chunk, TEXT.to_string()));
            let (n, d) = add(format!("{group}{i}"), members, chunk);
            nodes.push(n);
            docs.push(d);
            chunk += 1;
        }
    }
    // …and only THREE where Bridge sits between them. Fewer
    // conversations than Loom, and the only crossing anywhere.
    for i in 0..3 {
        pairs.push((chunk, TEXT.to_string()));
        let (n, d) = add(format!("X{i}"), vec!["Ledger", "Reef", "Bridge"], chunk);
        nodes.push(n);
        docs.push(d);
        chunk += 1;
    }
    let c: HashMap<u64, String> = pairs.into_iter().collect();
    let idx = ThemeIndex::from_enrichment(&nodes, &docs, &c, &HashMap::new());
    let card = fold_cast(&idx).unwrap();

    let named = |name: &str| {
        card.nodes
            .iter()
            .find(|n| n.canonical_name == name)
            .unwrap_or_else(|| panic!("{name} missing from the cast"))
    };
    let bridge = named("Bridge");
    let loom = named("Loom");
    assert!(
        bridge.conversations < loom.conversations,
        "fixture is wrong: Bridge ({}) must be RARER than Loom ({}) for this test to prove anything",
        bridge.conversations,
        loom.conversations
    );
    // The two clusters are not directly linked — every path between them
    // runs through Bridge, which is what betweenness is measuring.
    assert!(
        !card
            .edges
            .iter()
            .any(|e| (e.source == "ledger" && e.target == "reef")
                || (e.source == "reef" && e.target == "ledger")),
        "clusters must only meet through Bridge"
    );
    assert!(
        bridge.bridging > loom.bridging,
        "Bridge {} should out-bridge Loom {}",
        bridge.bridging,
        loom.bridging
    );
}

#[test]
fn themes_shorter_than_three_characters_are_dropped() {
    // Initialisms the enrichment pass emits as two characters ("AI",
    // "US") collide across unrelated subjects, so they are excluded —
    // the same floor the NER path applies.
    let docs = vec![doc("c1", vec![turn("2025-01-01 10:00", true, 1, "hi")])];
    let c = content(&[(1, "AI and Taoism in the US")]);
    let nodes = vec![node("c1", &["AI", "US", "Taoism"], &[1])];
    let idx = ThemeIndex::from_enrichment(&nodes, &docs, &c, &HashMap::new());
    assert_eq!(idx.prior.keys().collect::<Vec<_>>(), vec!["taoism"]);
}

#[test]
fn cast_absent_without_themes() {
    let idx = ThemeIndex::from_enrichment(&[], &[], &HashMap::new(), &HashMap::new());
    assert!(fold_cast(&idx).is_none());
}

// ─── read_raptor_nodes ───────────────────────────────────────────────

#[test]
fn raptor_nodes_missing_db_degrades_to_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let rows = read_raptor_nodes(&tmp.path().join("nope.db"), "x").unwrap();
    assert!(rows.is_empty());
}

#[test]
fn raptor_nodes_missing_table_degrades_to_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("state.db");
    rusqlite::Connection::open(&db).unwrap();
    let rows = read_raptor_nodes(&db, "x").unwrap();
    assert!(rows.is_empty());
}

#[test]
fn raptor_nodes_read_level_zero_and_tolerate_bad_json() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("state.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE conv_raptor_nodes (
            node_id TEXT PRIMARY KEY, corpus_id TEXT, conv_uuid TEXT, level INTEGER,
            summary TEXT, summary_embedding BLOB, centroid_embedding BLOB,
            children_node_ids TEXT, direct_member_chunk_ids TEXT, evidence_chunk_ids TEXT,
            quote_spans TEXT, primary_entities TEXT, cluster_coherence REAL, created_at INTEGER);",
    )
    .unwrap();
    let insert = "INSERT INTO conv_raptor_nodes VALUES (?1,'k',?2,?3,'s',x'',x'','[]',?4,'[]','[]',?5,0.8,0)";
    conn.execute(insert, rusqlite::params!["n1", "c1", 0, "[1,2]", r#"["Taoism"]"#])
        .unwrap();
    // level 1 — the summary-of-summaries tier, not a leaf: excluded.
    conn.execute(insert, rusqlite::params!["n2", "c1", 1, "[3]", r#"["Ignored"]"#])
        .unwrap();
    // malformed JSON must degrade, not fail the build.
    conn.execute(insert, rusqlite::params!["n3", "c2", 0, "not json", "also not json"])
        .unwrap();
    drop(conn);

    let rows = read_raptor_nodes(&db, "k").unwrap();
    assert_eq!(rows.len(), 2);
    let n1 = rows.iter().find(|r| r.conv_uuid == "c1").unwrap();
    assert_eq!(n1.entities, vec!["Taoism".to_string()]);
    assert_eq!(n1.chunk_ids, vec![1, 2]);
    let n3 = rows.iter().find(|r| r.conv_uuid == "c2").unwrap();
    assert!(n3.entities.is_empty() && n3.chunk_ids.is_empty());
}

// ─── determinism ─────────────────────────────────────────────────────

#[test]
fn folds_are_byte_stable_across_rebuilds() {
    // A deck that reshuffles between builds is a deck the reader cannot
    // trust; the ordering tie-breaks exist for this.
    let (nodes, docs, c) = quarterly_fixture();
    let a = ThemeIndex::from_enrichment(&nodes, &docs, &c, &HashMap::new());
    let b = ThemeIndex::from_enrichment(&nodes, &docs, &c, &HashMap::new());
    let json = |idx: &ThemeIndex| {
        serde_json::to_string(&(
            fold_obsessions(idx).unwrap(),
            fold_cast(idx),
        ))
        .unwrap()
    };
    assert_eq!(json(&a), json(&b));
}
