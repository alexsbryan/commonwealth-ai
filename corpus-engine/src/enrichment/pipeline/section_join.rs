// SPDX-License-Identifier: AGPL-3.0-or-later
//! The chunk → section join: which detected section does each stored chunk
//! belong to?
//!
//! # Why this exists
//!
//! [`super::ChapterManifest`]'s `chunk_ids` is the bridge between what
//! retrieval carries (LanceDB row ids on `ScoredChunk`) and what the rest of
//! the system cites (section ids like `sec_0001`, and through
//! `governance_view::section_titles`, human headings like `CHAPTER VII`).
//! Two production readers already depend on it —
//! `governance_view::chunk_to_section_map` and the retrieval pipeline's
//! governance active-set step.
//!
//! It was never filled. `ChapterManifest::from_detected_sections` set
//! `chunk_ids: Vec::new()` and the comment at the enrich call site said the
//! rest would arrive "from a future LanceDB ingest". Measured 2026-08-05:
//! 9 of 1788 local corpora had a populated join — all of them built by the
//! `--from-corpus` path, where chapters ARE chunks. Every file-backed corpus
//! (both chaos benches, ~1700 SEP articles) had an empty one, so every reader
//! silently got back an empty map and behaved as though the corpus had no
//! structure at all.
//!
//! # How the join is computed
//!
//! Sections carry BYTE ranges into the source document. Stored chunks carry
//! text but no offset, and — this is the whole difficulty — their text is
//! rarely a verbatim slice of the source. Ingest prepends titles and re-flows
//! whitespace. So a chunk is LOCATED (a byte offset in the source) and then
//! assigned to the section whose body range contains that offset.
//!
//! Locating tries four things, cheapest and most exact first:
//!
//! 1. The whole body, verbatim.
//! 2. The body minus its first line — the shape ingest creates when it
//!    prepends a document title (`abduction\n\n<text>`).
//! 3. Either of those against a WHITESPACE-NORMALISED projection of the
//!    source, mapped back to real byte offsets.
//! 4. Failing all that, a window walked forward from the head until one is
//!    uniquely findable — which skips exactly the interpolated prefix.
//!
//! Each step is evidence-driven, and the evidence is three corpora that each
//! broke a simpler version:
//!
//! | corpus | shape | what broke |
//! |---|---|---|
//! | `chaos-saltgrass` | title prefixed with spaces | naive `source.find(chunk)` finds nothing |
//! | `sep-*` | short boilerplate + title LINE | window is below the distinctive minimum |
//! | `chaos-secret-agent` | hard-wrapped Gutenberg, re-paragraphed | no window matches at all — 0 of 316 |
//!
//! Ambiguity is never resolved by taking the first hit. A body or window that
//! matches twice is rejected and the search moves on, because two plausible
//! homes is not a location and guessing here mis-attributes a citation.
//!
//! # Absence is reported, never defaulted
//!
//! A chunk whose probes are all ambiguous, or whose offset falls in no
//! section body, lands in [`SectionJoin::unmapped`]. It is NEVER dropped and
//! never guessed at. A caller that writes a manifest from this must surface a
//! non-empty `unmapped`, because a partially-joined manifest and a fully
//! joined one are different claims and the readers downstream cannot tell
//! them apart on their own (ARCH_PRINCIPLES §18.3).

use std::collections::BTreeMap;

use crate::chunkers::sectioned::DetectedSection;

/// Probe length in CHARACTERS. Long enough that a repeated phrase is
/// vanishingly unlikely, short enough to sit inside a small chunk.
const PROBE_CHARS: usize = 120;

/// How far into a chunk the probe scan will walk, in characters, and in what
/// steps.
///
/// The scan exists for ONE reason: ingest may prepend text that is not in the
/// source (chaos-saltgrass puts `saltgrass-ledger  ` on every chunk), so the
/// chunk's head does not match and the whole body is not findable. Walking
/// forward a few characters at a time finds the first position where the
/// remaining text IS source text — and that position is the chunk's true
/// start, because everything dropped was the interpolation.
///
/// The step bounds the error: a located offset is at most `PROBE_STEP_CHARS`
/// past the true start, which only misattributes a chunk that begins within
/// that distance of a section boundary. Fractional sampling (probe at 10%,
/// 25%, 50% …) was tried first and is WRONG for this: it locates a probe
/// rather than the chunk, so on chaos-saltgrass it put chunk 21 — which
/// starts in chapter X and runs into XI — in chapter XI.
const PROBE_MAX_DROP_CHARS: usize = 512;
const PROBE_STEP_CHARS: usize = 4;

/// The computed join, plus what could not be joined.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SectionJoin {
    /// Section id → the chunk ids whose start offset falls in its body,
    /// ascending. Sections with no chunks are absent rather than empty.
    pub by_section: BTreeMap<String, Vec<u64>>,
    /// Chunks that could not be located, ascending. Reported, never dropped.
    pub unmapped: Vec<u64>,
}

impl SectionJoin {
    pub fn mapped_chunks(&self) -> usize {
        self.by_section.values().map(Vec::len).sum()
    }

    /// True when every chunk found a home. The one-line check a caller
    /// should gate its "join complete" claim on.
    pub fn is_complete(&self) -> bool {
        self.unmapped.is_empty()
    }
}

/// Assign each `(chunk id, chunk text)` to the detected section containing it.
///
/// A chunk that spans a section boundary is assigned by where it STARTS —
/// one chunk, one section, so the join stays a function. Sections are matched
/// on their body range (`start_byte..end_byte`), which excludes the heading
/// itself; a chunk consisting only of a heading therefore lands in `unmapped`
/// rather than being attributed to the section it announces.
pub fn assign_chunks_to_sections(
    source: &str,
    sections: &[DetectedSection],
    chunks: &[(u64, &str)],
) -> SectionJoin {
    // Built ONCE for the whole batch, not per chunk: a sweep runs this over
    // ~1800 corpora and re-normalising a 500KB novel per chunk would dominate
    // the run.
    let norm = Normalized::of(source);
    let mut join = SectionJoin::default();
    for (id, text) in chunks {
        let placed = locate(&norm, text).and_then(|start| {
            let extent = start..start.saturating_add(text.trim().len());
            section_for(sections, &extent)
        });
        match placed {
            Some(section_id) => join
                .by_section
                .entry(section_id.to_string())
                .or_default()
                .push(*id),
            None => join.unmapped.push(*id),
        }
    }
    for ids in join.by_section.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }
    join.unmapped.sort_unstable();
    join.unmapped.dedup();
    join
}

/// The byte offset in `source` where this chunk's SOURCE-DERIVED text begins,
/// or `None` when it cannot be located unambiguously.
///
/// Two cases:
///
/// - **The whole body is a unique substring** (the common case): that offset
///   is the chunk's true start, exactly.
/// - **It is not** — because ingest interpolated text the source does not
///   contain. Then walk forward from the head in [`PROBE_STEP_CHARS`] steps
///   until a [`PROBE_CHARS`]-wide window IS uniquely findable. The first such
///   window begins at the first character that came from the source, so its
///   offset is the true start up to the step size. Everything skipped was the
///   interpolation.
///
/// Both cases return a START, which is what makes "a chunk spanning a
/// boundary belongs to the section it begins in" a statement this function
/// can actually honour.
///
/// A window matching more than once is REJECTED rather than resolved by
/// taking the first hit: two plausible homes is not a location, and guessing
/// here would silently mis-attribute a citation. The scan simply steps on —
/// a later window in the same chunk is usually unambiguous.
fn locate(norm: &Normalized<'_>, chunk: &str) -> Option<usize> {
    // The known interpolation shape, tried FIRST because it restores an exact
    // body: ingest prepends the document title as its own line
    // (`abduction\n\n<text>`). It has to happen on the raw chunk — after
    // whitespace normalisation there are no lines left to drop. It also
    // rescues chunks the window scan cannot touch at all: SEP's page
    // boilerplate is ~40 characters, below the minimum distinctive window.
    let candidates = [
        Some(chunk.trim()),
        chunk.trim().split_once('\n').and_then(|(first, rest)| {
            let rest = rest.trim_start();
            (!first.trim().is_empty() && !rest.is_empty()).then_some(rest)
        }),
    ];
    for body in candidates.into_iter().flatten() {
        // Exact match on the RAW text first. Normalising raises recall on
        // re-wrapped corpora but it also merges passages that differ only in
        // whitespace, turning some previously-unique bodies ambiguous — the
        // fleet sweep lost ~400 SEP chunks to exactly that while gaining 316
        // on chaos-secret-agent. An exact raw hit cannot be ambiguous in that
        // way, so trying it first keeps both.
        if let Some(off) = unique_find_raw(norm.raw, body.trim()) {
            return Some(off);
        }
        let body = normalize(body);
        if body.is_empty() {
            continue;
        }
        if let Some(off) = norm.unique_find(&body) {
            return Some(off);
        }
        let chars: Vec<(usize, char)> = body.char_indices().collect();
        let max_drop = PROBE_MAX_DROP_CHARS.min(chars.len());
        let mut drop = 0usize;
        while drop < max_drop {
            let end_idx = (drop + PROBE_CHARS).min(chars.len());
            if end_idx.saturating_sub(drop) < PROBE_CHARS / 2 {
                // What is left is too short to be distinctive — a
                // 20-character window finds itself in half the document.
                break;
            }
            let (s, e) = (chars[drop].0, chars.get(end_idx).map_or(body.len(), |c| c.0));
            // A window must not START on whitespace. Its offset maps back to
            // the first character of the matched run, and the run before a
            // heading is the newline that ENDS the previous section — so a
            // space-led window lands one byte inside the wrong chapter. Found
            // by `a_title_prefixed_chunk_is_still_located` regressing when
            // matching moved onto the normalised projection.
            let window = body[s..e].trim_start();
            if !window.is_empty() {
                if let Some(off) = norm.unique_find(window) {
                    return Some(off);
                }
            }
            drop += PROBE_STEP_CHARS;
        }
    }
    None
}

/// The source with every run of whitespace collapsed to a single space, plus
/// the map back to original byte offsets.
///
/// # Why matching cannot be done on the raw text
///
/// Ingest re-flows what it stores. `chaos-secret-agent` is a Gutenberg novel
/// hard-wrapped at ~70 columns, and its stored chunks carry `rammed\n\ndown`
/// where the source has `rammed\ndown` — every single newline inside a
/// paragraph became a blank line. A 120-character window spans several source
/// lines, so on that corpus NO window matched and all 316 chunks were
/// unmapped (measured 2026-08-05) even though every one of them is present in
/// the source word for word.
///
/// Whitespace is the transformation ingest is most likely to apply and the
/// one that carries no meaning, so normalising it away is what makes this
/// join a property of the text rather than of a particular chunker's
/// formatting.
struct Normalized<'a> {
    /// The untouched source, so an exact match can be tried before falling
    /// back to the lossy projection.
    raw: &'a str,
    text: String,
    /// `origin[i]` = byte offset in the ORIGINAL source of the character
    /// whose normalised form starts at byte `i`. One entry per byte of
    /// `text`, so a match offset maps straight back.
    origin: Vec<usize>,
}

impl<'a> Normalized<'a> {
    fn of(source: &'a str) -> Self {
        let mut text = String::with_capacity(source.len());
        let mut origin = Vec::with_capacity(source.len());
        let mut in_ws = false;
        for (i, ch) in source.char_indices() {
            if ch.is_whitespace() {
                if !in_ws {
                    text.push(' ');
                    origin.push(i);
                    in_ws = true;
                }
            } else {
                in_ws = false;
                text.push(ch);
                for _ in 0..ch.len_utf8() {
                    origin.push(i);
                }
            }
        }
        Self { raw: source, text, origin }
    }

    /// Original-source byte offset of `needle`, iff it occurs EXACTLY once in
    /// the normalised text. Ambiguity is rejected rather than resolved by
    /// taking the first hit: two plausible homes is not a location, and
    /// guessing would silently mis-attribute a citation.
    fn unique_find(&self, needle: &str) -> Option<usize> {
        let first = self.text.find(needle)?;
        if self.text[first + needle.len()..].find(needle).is_some() {
            return None;
        }
        self.origin.get(first).copied()
    }
}

/// `Some(offset)` iff `needle` occurs EXACTLY once in `haystack`, verbatim.
fn unique_find_raw(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let first = haystack.find(needle)?;
    match haystack[first + needle.len()..].find(needle) {
        Some(_) => None,
        None => Some(first),
    }
}

/// A chunk body under the same whitespace rule, so the two sides compare.
fn normalize(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut in_ws = false;
    for ch in body.chars() {
        if ch.is_whitespace() {
            if !in_ws && !out.is_empty() {
                out.push(' ');
                in_ws = true;
            }
        } else {
            in_ws = false;
            out.push(ch);
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// The section a chunk occupying `extent` belongs to.
///
/// Two rules, in order:
///
/// 1. **The section whose body contains the chunk's START.** This is what
///    makes a chunk spanning a boundary belong to the section it begins in.
/// 2. **Otherwise, the first section its extent OVERLAPS.** A chunk can begin
///    outside every body and still be mostly body text — the first chunk of a
///    document typically opens with front matter (a title page, a byline) and
///    runs on into chapter one. Measured on chaos-saltgrass: chunk 1 is
///    exactly this, and rule 1 alone left it unmapped, costing the corpus
///    1/30 of its citable surface for no reason. The heading a chunk runs
///    INTO is the honest answer for it.
///
/// A chunk that overlaps nothing — front matter in a document whose first
/// heading is far away — is still `None`, and the caller reports it. Sections
/// are not assumed sorted or disjoint; document order decides ties.
fn section_for<'a>(
    sections: &'a [DetectedSection],
    extent: &std::ops::Range<usize>,
) -> Option<&'a str> {
    sections
        .iter()
        .find(|s| extent.start >= s.start_byte && extent.start < s.end_byte)
        .or_else(|| {
            sections
                .iter()
                .find(|s| extent.start < s.end_byte && extent.end > s.start_byte)
        })
        .map(|s| s.id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn section(id: &str, start: usize, end: usize) -> DetectedSection {
        DetectedSection {
            id: id.into(),
            title: id.to_uppercase(),
            start_byte: start,
            end_byte: end,
            metadata: HashMap::new(),
        }
    }

    /// Two chapters with clearly distinct bodies.
    fn doc() -> (String, Vec<DetectedSection>) {
        let a = "The lock basin held its water behind heavy gates, and grudgingly, \
                 while the gulls argued over the fish-gutting boards on the quay. ";
        let b = "Glasswater Stave is not on the railway, and the coast road reaches \
                 it as if by afterthought, dropping off the saltgrass downs. ";
        let source = format!("CHAPTER I\n{a}\nCHAPTER II\n{b}");
        let a_start = source.find(a).unwrap();
        let b_start = source.find(b).unwrap();
        let sections = vec![
            section("sec_0001", a_start, b_start),
            section("sec_0002", b_start, source.len()),
        ];
        (source, sections)
    }

    #[test]
    fn verbatim_chunks_land_in_their_own_section() {
        let (source, sections) = doc();
        let c1 = "The lock basin held its water behind heavy gates";
        let c2 = "dropping off the saltgrass downs.";
        let join = assign_chunks_to_sections(&source, &sections, &[(1, c1), (2, c2)]);
        assert_eq!(join.by_section["sec_0001"], vec![1]);
        assert_eq!(join.by_section["sec_0002"], vec![2]);
        assert!(join.is_complete());
    }

    /// The case that forces the probe: ingest prepends a corpus title, so the
    /// chunk's full text is NOT a substring of the source. Measured on
    /// chaos-saltgrass, where every stored chunk begins `saltgrass-ledger  `.
    #[test]
    fn a_title_prefixed_chunk_is_still_located() {
        let (source, sections) = doc();
        let prefixed = format!(
            "saltgrass-ledger  {}",
            "Glasswater Stave is not on the railway, and the coast road reaches \
             it as if by afterthought, dropping off the saltgrass downs."
        );
        let join = assign_chunks_to_sections(&source, &sections, &[(7, prefixed.as_str())]);
        assert_eq!(join.by_section["sec_0002"], vec![7], "unmapped: {:?}", join.unmapped);
    }

    #[test]
    fn an_ambiguous_chunk_is_unmapped_not_guessed() {
        // Text that appears in BOTH chapters has no unique location. Picking
        // the first hit would silently mis-attribute a citation.
        let repeated = "the saltgrass tide runs twenty feet at the springs. ";
        let source = format!("CHAPTER I\n{repeated}\nCHAPTER II\n{repeated}");
        let mid = source.len() / 2;
        let sections = vec![section("sec_0001", 10, mid), section("sec_0002", mid, source.len())];
        let join = assign_chunks_to_sections(&source, &sections, &[(3, repeated.trim())]);
        assert_eq!(join.unmapped, vec![3]);
        assert!(join.by_section.is_empty());
        assert!(!join.is_complete());
    }

    #[test]
    fn a_chunk_absent_from_the_source_is_unmapped() {
        let (source, sections) = doc();
        let join = assign_chunks_to_sections(
            &source,
            &sections,
            &[(9, "a sentence that appears nowhere in this document at all")],
        );
        assert_eq!(join.unmapped, vec![9]);
    }

    /// A bare heading is not locatable at all here — "CHAPTER I" is itself a
    /// substring of "CHAPTER II", so it has two occurrences and no unique
    /// position — and unlocatable means unmapped, never guessed.
    #[test]
    fn an_unlocatable_bare_heading_is_unmapped() {
        let (source, sections) = doc();
        let join = assign_chunks_to_sections(&source, &sections, &[(4, "CHAPTER I")]);
        assert_eq!(join.unmapped, vec![4]);
    }

    /// REGRESSION, from chaos-saltgrass chunk 1 (2026-08-05). The first chunk
    /// of a document opens with front matter — a title page, a byline — that
    /// sits before every section body, then runs on into chapter one.
    /// Assigning strictly by start offset left it unmapped and cost the
    /// corpus 1/30 of its citable surface; the section it runs INTO is the
    /// honest answer.
    #[test]
    fn front_matter_running_into_the_first_section_is_assigned_to_it() {
        let (source, sections) = doc();
        // Starts at byte 0 (the title line, outside every body) and reaches
        // into chapter I.
        let head_end = source.find("grudgingly").unwrap();
        let front = &source[..head_end];
        let join = assign_chunks_to_sections(&source, &sections, &[(1, front)]);
        assert_eq!(
            join.by_section.get("sec_0001"),
            Some(&vec![1]),
            "unmapped={:?} by_section={:?}",
            join.unmapped,
            join.by_section
        );
    }

    /// Text that overlaps NO section keeps failing closed. The overlap rule
    /// widens attribution, it does not make everything attributable.
    #[test]
    fn text_overlapping_no_section_at_all_is_unmapped() {
        let a = "Front matter that belongs to nothing and runs for a while here. ";
        let b = "The body of the one and only chapter in this little document. ";
        let source = format!("{a}CHAPTER I\n{b}");
        let b_start = source.find(b).unwrap();
        let sections = vec![section("sec_0001", b_start, source.len())];
        let join = assign_chunks_to_sections(&source, &sections, &[(2, a.trim())]);
        assert_eq!(join.unmapped, vec![2], "by_section={:?}", join.by_section);
    }

    #[test]
    fn an_empty_chunk_is_unmapped_rather_than_matching_everything() {
        let (source, sections) = doc();
        let join = assign_chunks_to_sections(&source, &sections, &[(5, "   ")]);
        assert_eq!(join.unmapped, vec![5]);
    }

    #[test]
    fn a_chunk_spanning_a_boundary_is_assigned_by_where_it_starts() {
        let (source, sections) = doc();
        // Ends in chapter I, runs into chapter II.
        let spanning = {
            let a_tail = "fish-gutting boards on the quay.";
            let start = source.find(a_tail).unwrap();
            let end = source.find("as if by afterthought").unwrap();
            &source[start..end]
        };
        let join = assign_chunks_to_sections(&source, &sections, &[(6, spanning)]);
        assert_eq!(join.by_section["sec_0001"], vec![6], "one chunk, one section");
        assert!(!join.by_section.contains_key("sec_0002"));
    }

    /// REGRESSION, from chaos-saltgrass chunk 21 (2026-08-05). A chunk that
    /// is BOTH title-prefixed AND spans a section boundary is the case where
    /// locating a probe instead of the chunk goes wrong: the fractional-probe
    /// strategy sampled at 10% of the body, which is already past the
    /// boundary, and filed a chapter-X chunk under chapter XI. A wrong
    /// locator is worse than no locator — it points a reader at the wrong
    /// place with full confidence.
    #[test]
    fn a_prefixed_chunk_spanning_a_boundary_is_still_assigned_by_its_start() {
        let (source, sections) = doc();
        let tail = "fish-gutting boards on the quay.";
        let start = source.find(tail).unwrap();
        let end = source.find("as if by afterthought").unwrap();
        let prefixed = format!("saltgrass-ledger  {}", &source[start..end]);
        let join = assign_chunks_to_sections(&source, &sections, &[(21, prefixed.as_str())]);
        assert_eq!(
            join.by_section.get("sec_0001"),
            Some(&vec![21]),
            "must land in the section it STARTS in; unmapped={:?} by_section={:?}",
            join.unmapped,
            join.by_section
        );
    }

    /// REGRESSION, from SEP `abduction` chunks 68/71/73 (2026-08-05). Ingest
    /// prepends the document title as its own line, and SEP's page boilerplate
    /// is ~40 characters — shorter than the minimum distinctive window. Before
    /// the title-line strip, the window scan bailed on "too short" and those
    /// chunks were unmapped for a reason that had nothing to do with where
    /// they are.
    #[test]
    fn a_short_title_prefixed_chunk_is_located_by_dropping_the_title_line() {
        let (source, sections) = doc();
        let short = "dropping off the saltgrass downs.";
        assert!(short.len() < PROBE_CHARS / 2, "the point is that it is too short to probe");
        let prefixed = format!("saltgrass-ledger\n\n{short}");
        let join = assign_chunks_to_sections(&source, &sections, &[(68, prefixed.as_str())]);
        assert_eq!(
            join.by_section.get("sec_0002"),
            Some(&vec![68]),
            "unmapped={:?} by_section={:?}",
            join.unmapped,
            join.by_section
        );
    }

    /// The strip must not turn an unlocatable chunk into a located one by
    /// accident — dropping a line is a repair for a known interpolation, not
    /// a licence to match on whatever is left.
    #[test]
    fn dropping_a_title_line_does_not_rescue_text_absent_from_the_source() {
        let (source, sections) = doc();
        let join = assign_chunks_to_sections(
            &source,
            &sections,
            &[(9, "saltgrass-ledger\n\nthis sentence is nowhere in the document")],
        );
        assert_eq!(join.unmapped, vec![9]);
    }

    /// REGRESSION, from chaos-secret-agent (2026-08-05): a Gutenberg novel
    /// hard-wrapped at ~70 columns whose stored chunks turned every in-
    /// paragraph newline into a blank line. Raw matching found NOTHING —
    /// 0 of 316 chunks mapped — although every chunk is in the source word
    /// for word. Whitespace is the transformation ingest is most likely to
    /// apply and the one that carries no meaning.
    #[test]
    fn rewrapped_whitespace_does_not_prevent_a_match() {
        let a = "The lock basin held its water\nbehind heavy gates, and grudgingly,\nwhile \
                 the gulls argued over the boards. ";
        let b = "Glasswater Stave is not on the railway,\nand the coast road reaches it as \
                 if by afterthought. ";
        let source = format!("CHAPTER I\n{a}\nCHAPTER II\n{b}");
        let sections = vec![
            section("sec_0001", source.find(a).unwrap(), source.find(b).unwrap()),
            section("sec_0002", source.find(b).unwrap(), source.len()),
        ];
        // Same words, re-wrapped: single newlines became blank lines.
        let rechunked = "behind heavy gates, and grudgingly,\n\nwhile the gulls argued over \
                         the boards.";
        let join = assign_chunks_to_sections(&source, &sections, &[(1, rechunked)]);
        assert_eq!(
            join.by_section.get("sec_0001"),
            Some(&vec![1]),
            "unmapped={:?} by_section={:?}",
            join.unmapped,
            join.by_section
        );
    }

    /// Normalising whitespace must not normalise away the WORDS — text that
    /// merely looks similar still has to be absent.
    #[test]
    fn normalisation_does_not_make_absent_text_matchable() {
        let (source, sections) = doc();
        let join = assign_chunks_to_sections(
            &source,
            &sections,
            &[(9, "the   lock\n\nbasin  held  its  fire  behind  heavy  gates")],
        );
        assert_eq!(join.unmapped, vec![9]);
    }

    #[test]
    fn duplicate_chunk_ids_collapse_and_counts_stay_honest() {
        let (source, sections) = doc();
        let c = "The lock basin held its water behind heavy gates";
        let join = assign_chunks_to_sections(&source, &sections, &[(1, c), (1, c)]);
        assert_eq!(join.by_section["sec_0001"], vec![1]);
        assert_eq!(join.mapped_chunks(), 1);
    }
}
