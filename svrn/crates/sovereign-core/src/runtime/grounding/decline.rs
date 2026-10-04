// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decline detection — the predicates that recognise a released text
//! that asserts nothing: the honest abstention ("the sources don't
//! cover it") and the pure provenance-flagged NO_CLAIM decline. Split
//! out of `mod.rs` (file ceiling); the historical `grounding::` paths
//! hold via the re-exports there.

use super::strip_gk_caveat;

/// The honest-abstention phrasings [`answer_declines`] recognises.
const DECLINES: &[&str] = &[
    "i don't have reliable information",
    "i do not have reliable information",
    "i am not certain",
    "i'm not certain",
    "i do not have information",
    "i don't have information",
    "couldn't confirm an answer",  // grounded_abstention prose (current)
    "could not confirm an answer", // grounded_abstention prose (current)
    "none of them actually cover it", // grounded_abstention prose (legacy, still in-the-wild)
    "i'd rather not guess",        // grounded_abstention prose (legacy)
    "do not contain",
    "does not contain",
    "not recorded there",
    "the sources do not",
    "the sources don't",
    "sources do not contain",
    "no passage",
    "not in your sources",
];

/// True when a released short answer is itself an honest abstention / decline
/// ("the sources don't cover it", "I'm not certain", the `grounded_abstention`
/// prose). Such an answer asserts no verifiable value, so the specifics scan has
/// nothing to fabricate-check — running it only surfaces kind-(3) noise (the
/// scan second-guessing a correct "not in sources" as a false claim ABOUT the
/// evidence). Skipping is a latency optimisation and errs fail-open: a false
/// skip just preserves prior behaviour. Measured 2026-07-01: 6/7 short-band scan
/// flags on GOOD answers were exactly these honest abstentions.
pub fn answer_declines(text: &str) -> bool {
    let h = text.trim_start().to_lowercase();
    DECLINES.iter().any(|d| h.contains(d))
}

/// True when a NO_CLAIM release is a pure provenance-flagged decline — the
/// model saying "I don't have reliable information in my knowledge base"
/// over retrieved-but-useless evidence. Such a turn asserts nothing, so
/// releasing it as an answer mis-states the turn's epistemic standing: the
/// ledger derives `Unverified` (evidence present, nothing audited), the
/// coverage probe never runs (`gap_turn=false`), and a genuine knowledge
/// gap defaults to `ClaimUncovered` (bench/gap_check/DECISION.md, bug 2).
/// A 0-holding decline IS an abstention — reclassify the ACTION, keep the
/// model's own (honest, already provenance-flagged) prose.
///
/// Deliberately narrower than [`answer_declines`]: a caveated parametric
/// answer ("Not in your sources — from general knowledge: Canberra…")
/// declines-then-ANSWERS, and must keep releasing — so the caveat is
/// stripped first and any remaining "from general knowledge" pivot vetoes
/// the reclassification.
/// Did a claim-free release actually abstain?
///
/// ONE decider, on both arms of the native-grounding flag: the incumbent
/// 17-phrase zoo, which recovers the decision the system made but never
/// carried.
///
/// **Why the typed verdict is not consulted here.** It used to be: when
/// H1 had run, its `decision` supplied the action directly and the zoo was
/// skipped. P1 retired that (`NATIVE_GROUNDING_PARITY_PLAN.md` §4.1 —
/// admission is telemetry, "decisions traced, never enforced"), because
/// letting it stand made the flag change a turn's *action* in both
/// directions: a prose decline under a typed `Answer` stayed `released`
/// on the flag-on arm while flag-off reclassified it, and the epistemic
/// ledger, the collaboration surface and the honesty scorer all read that
/// string. That divergence is exactly what A1's arm-identity check
/// forbids, and A1 is the plan's pre-registered kill for the whole phase.
/// The typed path returns at P3c, when a verdict is enforced again by a
/// signal that earned it.
///
/// Returns the legacy action string to reclassify to, or `None` to leave
/// the action alone. Pure — no model, no env, no clock.
pub(crate) fn abstention_action(text: &str) -> Option<&'static str> {
    released_pure_decline(text).then_some("abstained_decline")
}

pub fn released_pure_decline(text: &str) -> bool {
    let stripped = strip_gk_caveat(text);
    if stripped.to_lowercase().contains("from general knowledge") {
        return false;
    }
    answer_declines(&stripped)
}

/// An absence statement is a negator followed, within [`ABSENCE_WINDOW`]
/// words of the same clause, by what a source tells: "does not mention",
/// "no hourly rate specified", "no mention is made", "never appears",
/// "contain no information". Read only by [`declines_asked_fact`], where
/// verified holdings bound it.
const NEGATORS: &[&str] = &["not", "no", "never"];
/// Stems, matched as word prefixes ("specif" is specify / specified).
/// "answer" is left out on purpose: the citation multiquote renders "The
/// passages do not answer: <part>" only beside a part it grounded
/// (`citation.rs`), and that part is often the asked fact.
const TELLING: &[&str] = &[
    "mention",
    "specif",
    "state",
    "say",
    "said",
    "set",
    "give",
    "given",
    "includ",
    "contain",
    "appear",
    "list",
    "provid",
    "establish",
    "information",
    "figure",
    "record",
    "name",
    "detail",
    "data",
];
/// "no distinct fee schedule was established": the negator may sit four
/// words ahead of what it negates.
const ABSENCE_WINDOW: usize = 5;

/// A contrast, or a general-knowledge signpost, may open an answer ("the
/// sources don't give the date, but the war ended in 1945") or continue the
/// decline ("but her given name never appears"). [`pivot_answers`] reads
/// which.
const ANSWER_PIVOTS: &[&str] = &[" but ", "however", "though", "instead", "nevertheless"];
const GK_SIGNPOST: &str = "from general knowledge";

/// The decline has to open the answer: it begins within the first two
/// sentences. A caveat at the end of a full answer ("…under load are not
/// specified in these passages") declines a side detail, not the asked fact.
const LEAD_SENTENCES: usize = 2;

/// Does a released text decline the asked fact, whatever adjacent facts it
/// restates? The ledger's partial-decline test (pc-partial-decline-verdict):
/// [`released_pure_decline`] answers "did it assert nothing", which a text
/// citing the partner and associate rates while declining the paralegal one
/// fails although it answered nothing that was asked.
///
/// Three shapes decide it, none of them a phrase list: the decline (a zoo
/// phrase or an absence statement) opens the text; and no contrast after
/// it, nor any general-knowledge signpost, opens a clause that asserts a
/// value the text had not already named and that does not state an absence
/// itself. A text that declines and then answers ("…but I think it is
/// $150", "from general knowledge: Canberra") is not a decline. Pure — no
/// model, no env.
pub(crate) fn declines_asked_fact(text: &str) -> bool {
    let text = strip_gk_caveat(text);
    // ASCII lowering keeps byte offsets, so `low` and `text` index alike.
    let low = text.to_ascii_lowercase();
    let Some(at) = decline_at(&low) else {
        return false;
    };
    let sentences = sentences_through(&low[..at]);
    if sentences > LEAD_SENTENCES {
        tracing::debug!(
            target: "epistemic.ledger",
            decline_at = at,
            sentences,
            "decline does not open the text: not a decline of the asked fact"
        );
        return false;
    }
    let contrast = ANSWER_PIVOTS.iter().flat_map(|p| {
        low[at..]
            .match_indices(p)
            .map(move |(i, m)| at + i + m.len())
    });
    let signpost = low.match_indices(GK_SIGNPOST).map(|(i, m)| i + m.len());
    let answered_at = contrast
        .chain(signpost)
        .find(|&start| pivot_answers(&text, &low, start));
    if let Some(pivot_end) = answered_at {
        tracing::debug!(
            target: "epistemic.ledger",
            decline_at = at,
            pivot_end,
            "a pivot after the decline asserts a new value: declines then answers"
        );
    }
    answered_at.is_none()
}

/// Byte offset of the first decline: a zoo phrase or an absence statement.
fn decline_at(low: &str) -> Option<usize> {
    DECLINES
        .iter()
        .filter_map(|p| low.find(p))
        .chain(absence_at(low))
        .min()
}

/// Byte offset of the first absence statement's negator. Clauses end at
/// sentence punctuation, a colon or a line break; markdown emphasis
/// ("do **not** contain") is not a word.
fn absence_at(low: &str) -> Option<usize> {
    runs(low, |c| !matches!(c, '\n' | '.' | ';' | ':' | '!' | '?')).find_map(|(start, clause)| {
        let words: Vec<(usize, &str)> = runs(clause, |c| {
            c.is_alphanumeric() || c == '\'' || c == '\u{2019}'
        })
        .collect();
        words.iter().enumerate().find_map(|(i, &(pos, w))| {
            let negates = NEGATORS.contains(&w) || w.ends_with("n't") || w.ends_with("n\u{2019}t");
            let tells = words[i + 1..]
                .iter()
                .take(ABSENCE_WINDOW)
                .any(|(_, t)| TELLING.iter().any(|s| t.starts_with(s)));
            (negates && tells).then_some(start + pos)
        })
    })
}

/// Sentences begun in `prefix`, the one the decline sits in included.
fn sentences_through(prefix: &str) -> usize {
    prefix
        .split(|c| c == '\n' || c == '?' || c == '!')
        .flat_map(|s| s.split(". "))
        .filter(|s| !s.trim().is_empty())
        .count()
        .max(1)
}

/// Does the clause a pivot opens at `start` answer? It does when it asserts
/// a value — a number, or a capitalised word mid-sentence — that the text
/// had not named before the pivot, and does not itself state an absence.
fn pivot_answers(text: &str, low: &str, start: usize) -> bool {
    let end = low[start..]
        .find(['\n', ';', '?', '!'])
        .into_iter()
        .chain(low[start..].find(". "))
        .min()
        .map_or(low.len(), |i| start + i);
    if decline_at(&low[start..end]).is_some() {
        return false;
    }
    let named: std::collections::HashSet<&str> = words(&low[..start]).map(|(_, w)| w).collect();
    words(&text[start..end]).any(|(i, w)| {
        let lw = &low[start + i..start + i + w.len()];
        let fresh = !named.contains(lw);
        let number = w.chars().any(|c| c.is_ascii_digit());
        let mid_sentence = text[..start + i]
            .trim_end()
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == ',');
        let name = w.starts_with(|c: char| c.is_ascii_uppercase()) && w != "I" && mid_sentence;
        fresh && (number || name)
    })
}

/// ASCII alphanumeric runs with their byte offsets.
fn words(s: &str) -> impl Iterator<Item = (usize, &str)> {
    runs(s, |c| c.is_ascii_alphanumeric())
}

/// Maximal runs of chars `keep` accepts, each with its byte offset in `s`.
fn runs<'a>(
    s: &'a str,
    keep: impl Fn(char) -> bool + 'a,
) -> impl Iterator<Item = (usize, &'a str)> + 'a {
    let mut begun: Option<usize> = None;
    s.char_indices()
        .map(Some)
        .chain([None])
        .filter_map(move |ci| match ci {
            Some((i, c)) if keep(c) => {
                begun.get_or_insert(i);
                None
            }
            Some((i, _)) => begun.take().map(|b| (b, &s[b..i])),
            None => begun.take().map(|b| (b, &s[b..])),
        })
}
