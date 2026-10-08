// SPDX-License-Identifier: AGPL-3.0-or-later
//! Seeding, the windowed token alignment, and the differences it reports.

use std::collections::HashMap;
use std::ops::Range;

use crate::config::AlignConfig;
use crate::norm::{norm_v0, NormText};
use crate::tokens::{parse_quote, tokenize, GapKind, Item, Token};

/// One place a quotation stands in one text.
#[derive(Debug, Clone, PartialEq)]
pub struct QuoteAlignment {
    /// Index of the text in the slice given to [`align`].
    pub text: usize,
    /// The text's code points the quotation covers, half-open.
    pub source: Range<usize>,
    /// Every difference, in quotation order. Empty when the quotation is
    /// verbatim under `norm_v0`.
    pub edits: Vec<QuoteEdit>,
    /// Quotation tokens matched exactly, over quotation tokens.
    pub coverage: f32,
}

/// One difference between a quotation and its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuoteEdit {
    /// What kind.
    pub kind: QuoteEditKind,
    /// Code points of the quotation, half-open; empty (a point) for
    /// `Omitted`.
    pub quote: Range<usize>,
    /// Code points of the text, half-open; empty (a point) for `Added` and
    /// for a gap that stands for nothing.
    pub source: Range<usize>,
}

/// The closed set of differences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuoteEditKind {
    /// The quotation has other words where the source has these.
    Substituted,
    /// The quotation has words the source does not.
    Added,
    /// The source has words the quotation dropped with no ellipsis.
    Omitted,
    /// An ellipsis stands for this stretch of source.
    Elided,
    /// Bracketed editorial text stands for this stretch of source.
    Bracketed,
}

/// What [`align`] found, and what it decided along the way.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Aligned {
    /// Best first: coverage, then fewest edits' cost, then text, then
    /// position. Overlapping alignments in one text are reduced to the best.
    pub alignments: Vec<QuoteAlignment>,
    /// Seed occurrences aligned.
    pub anchors: usize,
    /// Distinct alignments the coverage floor dropped.
    pub below_floor: usize,
}

/// Weighted edit costs. A substitution is half an addition plus an omission,
/// so one changed word is one substitution. A case-only substitution is
/// cheaper still: a quotation that starts mid-sentence in lower case is
/// still the same words, and is reported as a substitution the client may
/// read as case (the platform reports wording and case; ADDRESSED_TEXT
/// §5.3 step 1).
const SUB: u32 = 8;
const SUB_CASE: u32 = 4;
const ADD: u32 = 8;
const OMIT: u32 = 8;
/// A bracket costs this for each of its words the stretch it stands for
/// lacks, and [`BRACKET_TOKEN`] for each token it consumes: the shortest
/// stretch holding the words the source has wins.
const BRACKET_MISSING: u32 = 4;
const BRACKET_TOKEN: u32 = 1;
const INF: u32 = u32::MAX / 4;

/// Find where `quote` stands in `texts`, and how it differs (crate docs).
pub fn align(quote: &str, texts: &[&str], cfg: &AlignConfig) -> Aligned {
    let q = Quote::new(quote);
    if q.tokens == 0 {
        return Aligned::default();
    }
    let sources: Vec<Source> = texts.iter().map(|t| Source::new(t)).collect();
    let anchors = seed(&q, &sources, cfg);
    let mut found: Vec<(QuoteAlignment, u32)> = Vec::new();
    for a in &anchors {
        let src = &sources[a.text];
        if let Some(hit) = align_at(&q, src, a, q.window(a, src, cfg), cfg) {
            found.push(hit);
        }
    }
    found.sort_by(|(a, ac), (b, bc)| {
        b.coverage
            .total_cmp(&a.coverage)
            .then(ac.cmp(bc))
            .then(a.text.cmp(&b.text))
            .then(a.source.start.cmp(&b.source.start))
    });
    let mut kept: Vec<QuoteAlignment> = Vec::new();
    for (hit, _) in found {
        let overlaps = kept.iter().any(|k| {
            k.text == hit.text && k.source.start < hit.source.end && hit.source.start < k.source.end
        });
        if !overlaps {
            kept.push(hit);
        }
    }
    let total = kept.len();
    kept.retain(|h| h.coverage >= cfg.coverage_floor);
    Aligned {
        below_floor: total - kept.len(),
        alignments: kept,
        anchors: anchors.len(),
    }
}

/// The quotation, normalised and parsed.
struct Quote {
    norm: NormText,
    items: Vec<Item>,
    /// How many items are tokens.
    tokens: usize,
}

impl Quote {
    fn new(quote: &str) -> Self {
        let norm = norm_v0(quote);
        let items = parse_quote(&norm);
        let tokens = items.iter().filter(|i| matches!(i, Item::Tok(_))).count();
        Quote {
            norm,
            items,
            tokens,
        }
    }

    fn chars(&self, t: &Token) -> &[char] {
        &self.norm.chars()[t.norm.clone()]
    }

    /// The source tokens an anchor's alignment may reach: the quotation laid
    /// along the seed's diagonal, widened by the band and by what each gap
    /// on either side may stand for.
    fn window(&self, a: &Anchor, src: &Source, cfg: &AlignConfig) -> Range<usize> {
        let reach = |items: &[Item]| -> usize {
            items
                .iter()
                .map(|i| match i {
                    Item::Tok(_) => 1,
                    Item::Gap {
                        kind: GapKind::Elide,
                        ..
                    } => cfg.elide_max_tokens,
                    Item::Gap {
                        kind: GapKind::Bracket { .. },
                        ..
                    } => cfg.bracket_max_tokens,
                })
                .sum()
        };
        let before = reach(&self.items[..a.item]) + cfg.band;
        let after = reach(&self.items[a.item..]) + cfg.band;
        a.token.saturating_sub(before)..(a.token + after).min(src.tokens.len())
    }
}

/// One candidate text, normalised and tokenised.
struct Source {
    norm: NormText,
    tokens: Vec<Token>,
}

impl Source {
    fn new(text: &str) -> Self {
        let norm = norm_v0(text);
        let tokens = tokenize(&norm);
        Source { norm, tokens }
    }

    fn chars(&self, t: &Token) -> &[char] {
        &self.norm.chars()[t.norm.clone()]
    }
}

/// A seed occurrence: quotation item `item` (the n-gram's first word) found
/// at source token `token` of text `text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Anchor {
    item: usize,
    text: usize,
    token: usize,
}

/// Up to `cfg.seeds` of the quotation's rarest word n-grams (n = 3, or the
/// longest gap-free run of words when that is shorter), and every
/// occurrence of each in the texts, rarest first, capped at
/// `cfg.max_anchors`. Rarity is counted in the candidate texts themselves.
fn seed(q: &Quote, sources: &[Source], cfg: &AlignConfig) -> Vec<Anchor> {
    // Words of each gap-free run of the quotation, as (item index, chars).
    let mut runs: Vec<Vec<(usize, &[char])>> = vec![Vec::new()];
    for (i, item) in q.items.iter().enumerate() {
        match item {
            Item::Tok(t) if t.word => {
                if let Some(run) = runs.last_mut() {
                    run.push((i, q.chars(t)));
                }
            }
            Item::Tok(_) => {}
            Item::Gap { .. } => runs.push(Vec::new()),
        }
    }
    let n = runs.iter().map(Vec::len).max().unwrap_or(0).min(3);
    if n == 0 {
        return Vec::new();
    }
    let mut grams: Vec<(Key, usize)> = Vec::new();
    for run in &runs {
        for w in run.windows(n) {
            grams.push((key(w.iter().map(|(_, c)| *c)), w[0].0));
        }
    }
    let mut by_key: HashMap<Key, Vec<usize>> = HashMap::new();
    for (g, (k, _)) in grams.iter().enumerate() {
        by_key.entry(*k).or_default().push(g);
    }
    let mut hits: Vec<Vec<(usize, usize)>> = vec![Vec::new(); grams.len()];
    for (s, src) in sources.iter().enumerate() {
        let words: Vec<usize> = (0..src.tokens.len())
            .filter(|&t| src.tokens[t].word)
            .collect();
        for w in words.windows(n) {
            let k = key(w.iter().map(|&t| src.chars(&src.tokens[t])));
            if let Some(gs) = by_key.get(&k) {
                for &g in gs {
                    hits[g].push((s, w[0]));
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..grams.len()).filter(|&g| !hits[g].is_empty()).collect();
    order.sort_by_key(|&g| (hits[g].len(), grams[g].1));
    order
        .into_iter()
        .take(cfg.seeds)
        .flat_map(|g| {
            let item = grams[g].1;
            hits[g]
                .iter()
                .map(move |&(text, token)| Anchor { item, text, token })
        })
        .take(cfg.max_anchors)
        .collect()
}

/// An n-gram key of up to three words; unused slots are empty.
type Key<'a> = [&'a [char]; 3];

fn key<'a>(words: impl Iterator<Item = &'a [char]>) -> Key<'a> {
    let mut k: Key<'a> = [&[], &[], &[]];
    for (slot, w) in k.iter_mut().zip(words) {
        *slot = w;
    }
    k
}

/// One step of an alignment. `q` indexes quotation items and `s` source
/// tokens, both absolute.
#[derive(Debug, Clone, PartialEq)]
enum Op {
    Match { q: usize, s: usize },
    Sub { q: usize, s: usize },
    Add { q: usize },
    Omit { s: usize },
    Gap { q: usize, s: Range<usize> },
}

/// Align the quotation through one anchor (seed and extend): the items from
/// the anchor on are extended forward from its token, and the items before
/// it backward from the token before it, each inside the window. The two
/// halves meet at the anchor, so a window that holds two occurrences of the
/// quotation still reports the one this anchor seeded.
fn align_at(
    q: &Quote,
    src: &Source,
    a: &Anchor,
    window: Range<usize>,
    cfg: &AlignConfig,
) -> Option<(QuoteAlignment, u32)> {
    let ahead_items: Vec<usize> = (a.item..q.items.len()).collect();
    let ahead_toks: Vec<usize> = (a.token..window.end).collect();
    let behind_items: Vec<usize> = (0..a.item).rev().collect();
    let behind_toks: Vec<usize> = (window.start..a.token).rev().collect();
    let (ahead, ahead_cost) = extend(q, src, &ahead_items, &ahead_toks, cfg)?;
    let (mut behind, behind_cost) = extend(q, src, &behind_items, &behind_toks, cfg)?;
    behind.reverse();
    let ops: Vec<Op> = behind.into_iter().chain(ahead).collect();
    let matched = ops.iter().filter(|o| matches!(o, Op::Match { .. })).count();
    let coverage = matched as f32 / q.tokens as f32;
    let hit = Render { q, src }.alignment(a.text, &ops, coverage)?;
    Some((hit, ahead_cost + behind_cost))
}

/// One extension away from an anchor: quotation items `items` against
/// source tokens `toks`, both listed in the order of travel. A weighted
/// edit alignment anchored at the start (source tokens skipped there are
/// omissions) and free at the far end. A gap item consumes a stretch: an
/// ellipsis any stretch up to its reach, free; a bracket the shortest
/// stretch holding the most of its own words ([`BRACKET_MISSING`]).
///
/// Ties in the traceback prefer a match, then an omission, then a
/// substitution, then an addition, and at the far end the longer span: a
/// word changed at either edge of the quotation is a substitution, not an
/// addition that drops the source's word from the span. Returns the ops in
/// travel order, with the cost.
fn extend(
    q: &Quote,
    src: &Source,
    items: &[usize],
    toks: &[usize],
    cfg: &AlignConfig,
) -> Option<(Vec<Op>, u32)> {
    let (n, m) = (items.len(), toks.len());
    if n == 0 {
        return Some((Vec::new(), 0));
    }
    let width = m + 1;
    let mut d = vec![INF; (n + 1) * width];
    for (j, cell) in d[..width].iter_mut().enumerate() {
        *cell = j as u32 * OMIT;
    }
    // The cost of aligning item `i` (a token) with token `j`: 0 for the
    // same characters, SUB_CASE when only case differs, else SUB.
    let cost = |i: usize, j: usize| match &q.items[items[i]] {
        Item::Tok(t) => token_cost(q.chars(t), src.chars(&src.tokens[toks[j]])),
        Item::Gap { .. } => INF,
    };
    // What a bracket costs standing for travel positions `[k, j)`.
    let bracket_cost = |words: &[Vec<char>], k: usize, j: usize| {
        let run = &toks[k..j];
        let missing = words
            .iter()
            .filter(|w| {
                !run.iter().any(|&t| {
                    let c = src.chars(&src.tokens[t]);
                    c.len() == w.len()
                        && c.iter()
                            .zip(w.iter())
                            .all(|(a, b)| a.to_lowercase().eq(std::iter::once(*b)))
                })
            })
            .count() as u32;
        missing * BRACKET_MISSING + (j - k) as u32 * BRACKET_TOKEN
    };
    for i in 1..=n {
        let (prev, row) = d.split_at_mut(i * width);
        let prev = &prev[(i - 1) * width..];
        let row = &mut row[..width];
        match &q.items[items[i - 1]] {
            Item::Tok(_) => {
                row[0] = prev[0] + ADD;
                for j in 1..=m {
                    let diag = prev[j - 1] + cost(i - 1, j - 1);
                    row[j] = diag.min(prev[j] + ADD).min(row[j - 1] + OMIT);
                }
            }
            Item::Gap {
                kind: GapKind::Elide,
                ..
            } => {
                let mut best = INF;
                for j in 0..=m {
                    best = best.min(prev[j]);
                    row[j] = best;
                }
            }
            Item::Gap {
                kind: GapKind::Bracket { words },
                ..
            } => {
                for (j, cell) in row.iter_mut().enumerate() {
                    let lo = j.saturating_sub(cfg.bracket_max_tokens);
                    *cell = (lo..=j)
                        .map(|k| prev[k] + bracket_cost(words, k, j))
                        .min()
                        .unwrap_or(INF);
                }
            }
        }
    }
    let at = |i: usize, j: usize| d[i * width + j];
    let best = (0..=m).map(|j| at(n, j)).min()?;
    let mut j = (0..=m).rev().find(|&j| at(n, j) == best)?;
    let mut i = n;
    // The source tokens at travel positions `[k, j)`, as an absolute range.
    let stretch = |k: usize, j: usize| {
        if k < j {
            let (x, y) = (toks[k], toks[j - 1]);
            x.min(y)..x.max(y) + 1
        } else {
            0..0
        }
    };
    let mut ops = Vec::new();
    while i > 0 || j > 0 {
        if i == 0 {
            ops.push(Op::Omit { s: toks[j - 1] });
            j -= 1;
            continue;
        }
        let here = at(i, j);
        let item = items[i - 1];
        match &q.items[item] {
            Item::Tok(_) => {
                let diag = (j > 0).then(|| (cost(i - 1, j - 1), at(i - 1, j - 1)));
                let by_diag = diag.is_some_and(|(c, before)| here == before + c);
                if by_diag && diag.is_some_and(|(c, _)| c == 0) {
                    ops.push(Op::Match {
                        q: item,
                        s: toks[j - 1],
                    });
                    j -= 1;
                } else if j > 0 && here == at(i, j - 1) + OMIT {
                    ops.push(Op::Omit { s: toks[j - 1] });
                    j -= 1;
                    continue;
                } else if by_diag {
                    ops.push(Op::Sub {
                        q: item,
                        s: toks[j - 1],
                    });
                    j -= 1;
                } else {
                    ops.push(Op::Add { q: item });
                }
                i -= 1;
            }
            Item::Gap {
                kind: GapKind::Elide,
                ..
            } => {
                let k = (0..=j).rev().find(|&k| at(i - 1, k) == here)?;
                ops.push(Op::Gap {
                    q: item,
                    s: stretch(k, j),
                });
                i -= 1;
                j = k;
            }
            Item::Gap {
                kind: GapKind::Bracket { words },
                ..
            } => {
                let lo = j.saturating_sub(cfg.bracket_max_tokens);
                let k = (lo..=j)
                    .rev()
                    .find(|&k| at(i - 1, k) + bracket_cost(words, k, j) == here)?;
                ops.push(Op::Gap {
                    q: item,
                    s: stretch(k, j),
                });
                i -= 1;
                j = k;
            }
        }
    }
    ops.reverse();
    Some((ops, best))
}

/// Turns ops into ranges in the callers' code points.
struct Render<'a> {
    q: &'a Quote,
    src: &'a Source,
}

impl Render<'_> {
    /// The input code points of quotation item `q`.
    fn quote(&self, q: usize) -> Range<usize> {
        let norm = match &self.q.items[q] {
            Item::Tok(t) => t.norm.clone(),
            Item::Gap { norm, .. } => norm.clone(),
        };
        self.q.norm.origin(norm)
    }

    /// The text code points of source tokens `[from, to]`, inclusive.
    fn source(&self, from: usize, to: usize) -> Range<usize> {
        let a = &self.src.tokens[from];
        let b = &self.src.tokens[to];
        self.src.norm.origin(a.norm.start..b.norm.end)
    }

    fn alignment(&self, text: usize, ops: &[Op], coverage: f32) -> Option<QuoteAlignment> {
        let consumed = ops.iter().flat_map(|o| match o {
            Op::Match { s, .. } | Op::Sub { s, .. } | Op::Omit { s } => *s..*s + 1,
            Op::Gap { s, .. } => s.clone(),
            Op::Add { .. } => 0..0,
        });
        let (first, last) = consumed.fold(None, |acc: Option<(usize, usize)>, s| match acc {
            None => Some((s, s)),
            Some((a, b)) => Some((a.min(s), b.max(s))),
        })?;
        let span = self.source(first, last);
        let mut edits = Vec::new();
        let mut run = Run::default();
        let mut q_point = self.quote(0).start;
        let mut s_point = span.start;
        for op in ops {
            match op {
                Op::Match { q, s } => {
                    self.flush(&mut run, &mut edits, &mut q_point, &mut s_point);
                    q_point = self.quote(*q).end;
                    s_point = self.source(*s, *s).end;
                }
                Op::Sub { q, s } => {
                    run.q(*q);
                    run.s(*s);
                }
                Op::Add { q } => run.q(*q),
                Op::Omit { s } => run.s(*s),
                Op::Gap { q, s } => {
                    self.flush(&mut run, &mut edits, &mut q_point, &mut s_point);
                    let kind = match &self.q.items[*q] {
                        Item::Gap {
                            kind: GapKind::Elide,
                            ..
                        } => QuoteEditKind::Elided,
                        _ => QuoteEditKind::Bracketed,
                    };
                    let source = if s.is_empty() {
                        s_point..s_point
                    } else {
                        self.source(s.start, s.end - 1)
                    };
                    s_point = source.end;
                    q_point = self.quote(*q).end;
                    edits.push(QuoteEdit {
                        kind,
                        quote: self.quote(*q),
                        source,
                    });
                }
            }
        }
        self.flush(&mut run, &mut edits, &mut q_point, &mut s_point);
        Some(QuoteAlignment {
            text,
            source: span,
            edits,
            coverage,
        })
    }

    /// Close a run of unmatched tokens as one edit: both sides →
    /// substituted, quotation only → added, source only → omitted.
    fn flush(
        &self,
        run: &mut Run,
        edits: &mut Vec<QuoteEdit>,
        q_point: &mut usize,
        s_point: &mut usize,
    ) {
        let taken = std::mem::take(run);
        let quote = taken.q.map(|(a, b)| self.quote(a).start..self.quote(b).end);
        let source = taken.s.map(|(a, b)| self.source(a, b));
        let (kind, quote, source) = match (quote, source) {
            (Some(q), Some(s)) => (QuoteEditKind::Substituted, q, s),
            (Some(q), None) => (QuoteEditKind::Added, q, *s_point..*s_point),
            (None, Some(s)) => (QuoteEditKind::Omitted, *q_point..*q_point, s),
            (None, None) => return,
        };
        *q_point = quote.end;
        *s_point = source.end;
        edits.push(QuoteEdit {
            kind,
            quote,
            source,
        });
    }
}

/// What aligning two tokens costs: nothing when they are the same
/// characters, [`SUB_CASE`] when they differ only in case, else [`SUB`].
fn token_cost(a: &[char], b: &[char]) -> u32 {
    if a == b {
        0
    } else if a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.to_lowercase().eq(y.to_lowercase()))
    {
        SUB_CASE
    } else {
        SUB
    }
}

/// The unmatched quotation items and source tokens since the last match.
#[derive(Default)]
struct Run {
    q: Option<(usize, usize)>,
    s: Option<(usize, usize)>,
}

impl Run {
    fn q(&mut self, i: usize) {
        self.q = Some(self.q.map_or((i, i), |(a, _)| (a, i)));
    }
    fn s(&mut self, j: usize) {
        self.s = Some(self.s.map_or((j, j), |(a, _)| (a, j)));
    }
}

#[cfg(test)]
#[path = "align_tests.rs"]
mod tests;
