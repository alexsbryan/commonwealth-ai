// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rule-lane next-edit prediction (`sovereign/docs/NEXT_EDIT.md`
//! §2–§4) — the deterministic half of the two-lane design. Pure: no
//! inference, no state, no editor knowledge. The daemon owns ALL
//! policy (context expansion, guards, induction, firing threshold)
//! so every IDE client stays a thin capture-and-render shell and the
//! JetBrains port inherits the behavior for free.
//!
//! The client streams coalesced edit units (`{before, after}` plus a
//! snippet of untouched context each side, captured at unit close);
//! we induce a literal rewrite rule, count support, threshold on
//! structural confidence, and return the remaining sites as a
//! tab-through queue. A wrong edit proposal is the expensive failure
//! (§1), so every gate here prefers silence.

/// One coalesced edit unit as the client captured it. `left`/`right`
/// are the UNTOUCHED context around the edit site at close time —
/// they make the unit self-contained, so induction never needs the
/// historical document states the edits happened in.
#[derive(Debug, Clone)]
pub struct HistoryUnit {
    pub before: String,
    pub after: String,
    pub left: String,
    pub right: String,
}

/// A literal rewrite rule with per-end identifier guards. A guarded
/// end refuses matches abutting a word character — which both keeps
/// matches out of longer identifiers and keeps a rule from
/// re-matching its own output (`word` → `wordNext`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardedRule {
    pub find: String,
    pub replace: String,
    pub guard_left: bool,
    pub guard_right: bool,
}

impl GuardedRule {
    /// Stable identity for client-side session suppression (Esc).
    pub fn key(&self) -> String {
        serde_json::to_string(&[&self.find, &self.replace]).unwrap_or_default()
    }
}

/// One proposed replacement. Offsets are BYTE offsets into the text
/// `predict` was given; the route layer converts to UTF-16 for the
/// wire (`sovereign/docs/NEXT_EDIT.md` §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub new_text: String,
}

/// Outcome of one prediction pass. Silence is a first-class result:
/// `reason_silent` says which gate held (glassbox §9), and `rule` /
/// `support` are still reported when a rule existed but didn't fire.
#[derive(Debug)]
pub struct Prediction {
    pub edits: Vec<Edit>,
    pub rule: Option<GuardedRule>,
    pub support: usize,
    pub sites: usize,
    pub edits_capped: bool,
    pub reason_silent: Option<&'static str>,
}

/// Induction looks at this many most-recent units.
pub const HISTORY_WINDOW: usize = 8;
/// Context absorbed into a rule per side, in chars.
const MAX_CTX: usize = 40;
/// Queue cap — beyond this the debug block reports truncation.
const MAX_EDITS: usize = 256;

fn is_ctx_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.')
}

pub(crate) fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Induce a rule from one unit, absorbing the untouched identifier
/// run (plus a trailing call-paren) on each side: editing
/// `log`→`debug` inside `console.log(` induces `console.log(` →
/// `console.debug(`, not the noisy bare `log`. Returns None for
/// units the rule lane doesn't reason about: no-ops and multi-line
/// edits.
pub fn expand_rule(unit: &HistoryUnit) -> Option<GuardedRule> {
    if unit.before == unit.after {
        return None;
    }
    if unit.before.contains('\n') || unit.after.contains('\n') {
        return None;
    }

    let left_start = unit
        .left
        .char_indices()
        .rev()
        .take(MAX_CTX)
        .take_while(|(_, c)| is_ctx_char(*c))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(unit.left.len());
    let left = &unit.left[left_start..];

    let mut right_end = 0;
    for (i, c) in unit.right.char_indices().take(MAX_CTX) {
        if is_ctx_char(c) {
            right_end = i + c.len_utf8();
        } else {
            break;
        }
    }
    // A trailing call-paren is cheap specificity: it is what
    // separates `console.log(` from the prefix of `console.logger`.
    if unit.right[right_end..].starts_with('(') {
        right_end += 1;
    }
    let right = &unit.right[..right_end];

    let find = format!("{left}{}{right}", unit.before);
    let replace = format!("{left}{}{right}", unit.after);
    if find == replace || find.trim().is_empty() {
        return None;
    }
    let guard_left = find.chars().next().is_some_and(is_word_char);
    let guard_right = find.chars().next_back().is_some_and(is_word_char);
    Some(GuardedRule {
        find,
        replace,
        guard_left,
        guard_right,
    })
}

/// Induce from recent unit rules (oldest first; None = uninducible
/// unit). The rule is always the MOST RECENT unit's — we only ever
/// propose continuing what the user just did — and support counts
/// window agreement rather than strict consecutiveness: a mid-burst
/// settle splits one logical edit into partial-progress units, and
/// strictness would let that noise erase real support.
pub fn induce(rules: &[Option<GuardedRule>]) -> Option<(GuardedRule, usize)> {
    let window_start = rules.len().saturating_sub(HISTORY_WINDOW);
    let recent: Vec<&GuardedRule> = rules[window_start..].iter().flatten().collect();
    let last = (*recent.last()?).clone();
    let support = recent.iter().filter(|r| **r == &last).count();
    Some((last, support))
}

/// Minimum `find` length, in trimmed chars, for a rule to fire. See
/// [`should_fire`] for the measured frontier that chose 5.
///
/// This is not only a filter — it is a ROUTER, and that is the
/// non-obvious thing to know before changing it. Declining a rule here
/// does not end the request: `predict` falls through to the pair kinds
/// ([`induce_insertion`], [`induce_deletion`]), which induce from the
/// SAME history and yield a longer, line-anchored rule. So raising this
/// bar can *increase* useful edits by handing the case to a more
/// specific lane. See [`should_fire`] for the measured example.
const MIN_RULE_CHARS: usize = 5;

/// The firing threshold — the whole "when may the system interrupt"
/// policy in one place (§4): never without a remaining site, never on
/// one supporting edit, never on a rule too short to be specific.
///
/// **The support tier is gone, and that is a result rather than a
/// simplification for its own sake.** The policy used to read "2
/// supports fire only a specific rule (≥4 chars); 3+ lower the bar (≥2)".
/// Once the curve below chose 2 for the support-2 case, the two arms
/// held the same condition and the distinction stopped distinguishing
/// anything — so it is one condition now.
///
/// [`MIN_RULE_CHARS`] went 4 → 2 → 5 in one day, and the reversal is
/// not a flip-flop: **the objective changed.** The 4→2 move maximised
/// `useful-fire`, which the scorer defines as `useful + partial`
/// (`score_golden.py:297`) — so it rewards a rule that fires wide and
/// happens to be right somewhere. Told that a user simply does not
/// accept a wrong fire, the right question stopped being "what
/// maximises the headline" and became "what is the most useful we can
/// be at each level of wrong". On that question the value 2 is
/// dominated outright.
///
/// Swept at 3 rule kinds over the whole golden set
/// (`gym/next-edit/golden/`, 1,098 cases, model lane off), reporting
/// STRICT useful — every proposed hunk one the author actually made —
/// beside the wrong count, rather than the headline:
///
/// | min | strict useful | wrong (of which negatives) |
/// |-----|---------------|----------------------------|
/// |  2  | 138           | 52 (32)                    |  <- dominated
/// |  4  | 139           | 48 (28)                    |
/// |  5  | 141           | 39 (27)                    |  <- chosen
/// |  6  | 143           | 38 (27)                    |
/// |  8  | 133           | 35 (24)                    |
/// | 16  | 116           | 17 ( 7)                    |
///
/// 5 rather than 6 because 141-vs-143 is inside sampling noise
/// (Wilson on 138/711 is ±2.9pts ≈ ±21 cases) while the wrong-fire
/// plateau starts at 5 — the value is chosen on the plateau's edge,
/// not on an argmax over 168 swept cells, which one bank cannot
/// support. Note `c97bf8cd` carries the frontier and the port.
///
/// WHY A STRICTER BAR YIELDS MORE USEFUL, which is the counter-
/// intuitive part: declining the short rule ROUTES the case to the
/// pair kinds, which re-induce from the same history and anchor on a
/// whole line. `fetch` → `-c core.fsmonitor=false fetch` matched 62
/// sites and scored `partial`; the anchored rule the fallback induces
/// instead, `` `git `` → `` `git -c core.fsmonitor=false ``, matches 8
/// and scores `useful`. Paired ledger for this move: 31 partial→missed,
/// 9 partial→useful, 7 useful→missed, and 14 wrong fires removed.
pub fn should_fire(rule: &GuardedRule, support: usize, remaining_sites: usize) -> bool {
    if remaining_sites < 1 {
        return false;
    }
    support >= 2 && rule.find.trim().chars().count() >= MIN_RULE_CHARS
}

/// Ceiling on an induced rule's `find`/`replace`. Rules come from
/// coalesced keystroke bursts plus a bounded context expansion, so a
/// real one is tens of bytes; the wire's 2 KiB-per-field allowance is
/// slack for the *unit*, not a licence for the rule derived from it.
const MAX_RULE_BYTES: usize = 512;
/// A replacement that contains its own target more than this many
/// times is not an edit pattern, it is a pathological input: each
/// occurrence is an alignment that must be probed at every site.
const MAX_ALIGNMENTS: usize = 8;
/// Ceiling on matches examined while scanning for sites. The queue
/// itself is capped at [`MAX_EDITS`] (256), so a file with more
/// occurrences than this is already past the point where more
/// scanning changes what the user sees.
const MAX_SITE_SCAN: usize = 4096;

/// Offsets at which `find` sits inside `replace` — the alignments an
/// occurrence of `find` could occupy within an already-applied
/// `replace`. `None` means the rule is degenerate (too many
/// alignments to probe); callers decline it rather than pay
/// occurrences × sites of comparison.
fn alignments(find: &str, replace: &str) -> Option<Vec<usize>> {
    if find == replace || !replace.contains(find) {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(f) = replace[start..].find(find).map(|i| start + i) {
        if out.len() == MAX_ALIGNMENTS {
            return None;
        }
        out.push(f);
        start = f + 1;
    }
    Some(out)
}

/// True when the occurrence of `find` at `o` sits inside an existing
/// instance of `replace`, at one of the precomputed `aligns` — i.e.
/// the user already made this edit here. Only possible for
/// insertion-shaped rules (`replace` contains `find`): those leave
/// `find` matching at every already-edited site, and re-proposing one
/// would stack the insertion (`await fetch(` → `await await fetch(`).
///
/// The alignments are computed once per rule, never once per site:
/// doing it per site made this quadratic in the rule's self-similarity
/// and turned a single request into 23 s of blocking CPU on a crafted
/// input (measured 2026-07-30 at 512 KiB of text).
fn already_applied(text: &str, o: usize, aligns: &[usize], replace: &str) -> bool {
    aligns.iter().any(|&f| {
        o.checked_sub(f)
            .is_some_and(|b| text.get(b..b + replace.len()) == Some(replace))
    })
}

/// Non-overlapping byte offsets of the rule's `find` in `text`,
/// ordered as a tab-through queue: document order from `from`,
/// wrapping to the sites before it. Sites the rule was already
/// applied to are excluded (see [`already_applied`]).
pub fn find_guarded_sites(text: &str, rule: &GuardedRule, from: usize) -> Vec<usize> {
    let find = rule.find.as_str();
    if find.is_empty() || find.len() > MAX_RULE_BYTES || rule.replace.len() > MAX_RULE_BYTES {
        return Vec::new();
    }
    let Some(aligns) = alignments(find, &rule.replace) else {
        return Vec::new();
    };
    let mut all = Vec::new();
    let mut at = 0;
    let mut examined = 0usize;
    while let Some(rel) = text[at..].find(find) {
        examined += 1;
        if examined > MAX_SITE_SCAN {
            break;
        }
        let o = at + rel;
        let left_ok = !rule.guard_left || !text[..o].chars().next_back().is_some_and(is_word_char);
        let right_ok = !rule.guard_right
            || !text[o + find.len()..]
                .chars()
                .next()
                .is_some_and(is_word_char);
        if left_ok && right_ok && !already_applied(text, o, &aligns, &rule.replace) {
            all.push(o);
        }
        at = o + find.len();
    }
    let split = all.partition_point(|&o| o < from);
    let mut out = all.split_off(split);
    out.extend(all);
    out
}

/// Induce an ANCHORED REPEAT-INSERTION from a pair of insertions —
/// the second rule kind, added 2026-08-06.
///
/// [`expand_rule`] induces from ONE unit and always yields a rewrite:
/// some `find` that occurs in the document is replaced. An insertion
/// has no `find` — the developer typed a block that was not there
/// before — so the single-unit induction cannot express it at all, and
/// returns `None`. That hole is the largest single gap on the golden
/// set: pure-INSERT truths scored 14.0% against 44.6% for replacements,
/// and forcing a model at them moved nothing (`gym/next-edit/golden/`,
/// note `53abe423`).
///
/// What a PAIR buys is the anchor. Two insertions of the same block
/// tell us both *what* to insert (the shared payload) and *where* (the
/// longest common line-aligned tail of their left contexts). Rendered
/// as `find = anchor`, `replace = anchor + payload`, the result is an
/// ordinary insertion-shaped [`GuardedRule`], so site finding, the
/// already-applied exclusion, the firing threshold and the edit queue
/// all apply unchanged — this adds a way to *induce*, not a second
/// pipeline to keep in step.
///
/// Measured as a fallback behind the literal lane: **+21 useful edits
/// for 2 wrong fires, nothing regressed, and 0 wrong fires across the
/// 387 negatives** (15 missed→useful, 6 missed→partial, 2 missed→wrong).
///
/// Word guards are off by construction: the anchor is line-aligned, so
/// its boundaries are newlines rather than identifier edges, and a word
/// guard there would reject every site.
pub fn induce_insertion(history: &[HistoryUnit]) -> Option<GuardedRule> {
    let [a, b] = match history {
        [.., a, b] => [a, b],
        _ => return None,
    };
    // Both units must be pure insertions carrying the SAME payload.
    // A per-site-varying payload is a different shape (`param_insert`)
    // and guessing which variant belongs at an unseen site is exactly
    // the inference this lane refuses to do.
    if !a.before.is_empty() || !b.before.is_empty() {
        return None;
    }
    if a.after != b.after || a.after.trim().is_empty() {
        return None;
    }
    let payload = &a.after;
    if payload.len() > MAX_RULE_BYTES {
        return None;
    }

    // The anchor is the longest common TAIL of the two left contexts,
    // trimmed forward to a line boundary. Cutting to a whole line is
    // what makes the anchor mean "after this line" rather than "after
    // this arbitrary suffix of a line", which would match mid-token.
    let (la, lb) = (a.left.as_bytes(), b.left.as_bytes());
    let mut n = 0;
    while n < la.len().min(lb.len()) && la[la.len() - 1 - n] == lb[lb.len() - 1 - n] {
        n += 1;
    }
    let mut anchor = match a.left.get(a.left.len() - n..) {
        Some(s) => s,
        // The common tail landed inside a multi-byte char; back off to
        // the nearest boundary rather than slicing a char in half.
        None => return None,
    };
    if let Some(i) = anchor.find('\n') {
        anchor = &anchor[i + 1..];
    }
    if anchor.trim().chars().count() < MIN_ANCHOR_CHARS || anchor.len() > MAX_RULE_BYTES {
        return None;
    }

    Some(GuardedRule {
        find: anchor.to_string(),
        replace: format!("{anchor}{payload}"),
        guard_left: false,
        guard_right: false,
    })
}

/// Induce a REPEAT BLOCK DELETION from a pair of identical deletions —
/// the third rule kind, and deliberately the narrowest.
///
/// A deletion is expressible as a [`GuardedRule`] (`replace` empty), so
/// [`expand_rule`] would already reach these but for its multi-line
/// guard. The guard is right to be there: measured against the 387
/// negatives, deleting on a *single*-line repeat fires wrongly 13 times
/// (`neg_exhausted` ×8, `neg_literal_trap` ×5), because a short literal
/// recurs innocently all over a document and a wrong deletion is the
/// most destructive edit this system can make.
///
/// Requiring the repeated block to be MULTI-LINE removes every one of
/// those: at ≥2 lines the bank measures 0 wrong fires on negatives and
/// 0 wrong on positives, for +5 useful edits. That is a small win taken
/// at zero measured risk, not a general deletion capability.
///
/// **What this deliberately does NOT do.** `delete_propagation`'s real
/// shape on the golden set is a developer bulk-removing a *run* of
/// sibling blocks, where each block differs and the truth is "remove
/// the rest of the list" — knowing where the list ends is a structural
/// judgment, not a repeat. The two obvious generalisations were
/// measured and rejected: identifier-scoped line deletion scores 2.9%
/// useful with 14 wrong fires on negatives, and single-line repeat
/// deletion 50% with 13. Neither is a trade this lane makes; 38 of the
/// 43 `delete_propagation` episodes stay open (note `53abe423`).
pub fn induce_deletion(history: &[HistoryUnit]) -> Option<GuardedRule> {
    let [a, b] = match history {
        [.., a, b] => [a, b],
        _ => return None,
    };
    if !a.after.is_empty() || !b.after.is_empty() {
        return None;
    }
    if a.before != b.before || a.before.trim().is_empty() {
        return None;
    }
    if a.before.lines().count() < MIN_DELETE_LINES || a.before.len() > MAX_RULE_BYTES {
        return None;
    }
    Some(GuardedRule {
        find: a.before.clone(),
        replace: String::new(),
        guard_left: false,
        guard_right: false,
    })
}

/// Minimum repeated-block size for [`induce_deletion`], in lines. At 1
/// the bank measures 13 wrong fires on negatives; at 2, zero.
const MIN_DELETE_LINES: usize = 2;

/// Minimum anchor length for [`induce_insertion`]. An anchor shorter
/// than this is not specific enough to name a site — it would match
/// punctuation runs and blank structure all over the document.
const MIN_ANCHOR_CHARS: usize = 3;

/// Narrows a rule's candidate sites using knowledge this lane does not
/// have and must not acquire — syntax, in practice.
///
/// THE POINT OF THE INDIRECTION. This module is pure by contract: no
/// inference, no state, no editor knowledge. Deciding that a match sits
/// inside a comment rather than in code needs a parser, which is
/// editor-adjacent knowledge and a dependency this lane should not
/// carry. So the lane keeps expressing POLICY and the caller supplies
/// the ORACLE (`routes_edit_predictions` builds it from the buffer the
/// request already carries). Dependency inversion, and the lane stays
/// testable with a closure that returns its input.
///
/// Contract: return a SUBSET of `sites`, order preserved. Returning
/// them unchanged must always be safe — an oracle that cannot judge
/// (no grammar for this language, no exemplar to compare against)
/// declines by returning the input rather than by guessing.
pub type SiteOracle<'a> = &'a dyn Fn(&GuardedRule, Vec<usize>) -> Vec<usize>;

/// The whole rule-lane pipeline: history → rule → sites → threshold
/// → queue. `cursor` is a byte offset into `text`.
///
/// Site selection is unfiltered here. That is the right default for
/// tests and for any caller without a parser, but it is NOT what the
/// route does — see [`predict_filtered`] and note `e8ecaef7`: only 34%
/// of the hunks this lane proposes are ones the author actually made,
/// and site selection is where that is won or lost.
pub fn predict(history: &[HistoryUnit], text: &str, cursor: usize) -> Prediction {
    predict_filtered(history, text, cursor, &|_rule, sites| sites)
}

/// [`predict`], with the caller's [`SiteOracle`] applied to every
/// candidate site set — including the ones the pair kinds find, since
/// an anchored insertion can land in a comment just as a rewrite can.
///
/// The threshold reads the FILTERED count, deliberately: `should_fire`
/// asks "is there a remaining site", and a site the oracle rejected is
/// not one. Filtering after the threshold would fire on an empty queue.
pub fn predict_filtered(
    history: &[HistoryUnit],
    text: &str,
    cursor: usize,
    keep: SiteOracle<'_>,
) -> Prediction {
    let silent = |reason, rule: Option<GuardedRule>, support, sites| Prediction {
        edits: Vec::new(),
        rule,
        support,
        sites,
        edits_capped: false,
        reason_silent: Some(reason),
    };

    // The literal lane first; the insertion lane only picks up what it
    // declines. Ordering matters: where both could speak the literal
    // rule is the more specific claim, and letting the anchor lane
    // preempt it measured as pure loss (11 cases the literal lane
    // already won).
    let fire = |rule: GuardedRule, support, sites: Vec<usize>| Prediction {
        edits_capped: sites.len() > MAX_EDITS,
        sites: sites.len(),
        support,
        edits: sites
            .iter()
            .take(MAX_EDITS)
            .map(|&s| Edit {
                start: s,
                end: s + rule.find.len(),
                new_text: rule.replace.clone(),
            })
            .collect(),
        rule: Some(rule),
        reason_silent: None,
    };
    // An insertion pair is its own support: two units agreeing on one
    // payload IS the repetition the threshold exists to require, so
    // `should_fire`'s length bar (written for rewrites) does not apply.
    // The anchor carries the specificity instead, via MIN_ANCHOR_CHARS.
    let insertion = |reason, rule: Option<GuardedRule>, support, n_sites| {
        for pair_rule in [induce_insertion(history), induce_deletion(history)]
            .into_iter()
            .flatten()
        {
            let sites = keep(&pair_rule, find_guarded_sites(text, &pair_rule, cursor));
            if !sites.is_empty() {
                return fire(pair_rule, 2, sites);
            }
        }
        silent(reason, rule, support, n_sites)
    };

    let rules: Vec<Option<GuardedRule>> = history.iter().map(expand_rule).collect();
    let Some((rule, support)) = induce(&rules) else {
        return insertion("no_rule", None, 0, 0);
    };
    let sites = keep(&rule, find_guarded_sites(text, &rule, cursor));
    if !should_fire(&rule, support, sites.len()) {
        let reason = if sites.is_empty() {
            "no_sites"
        } else {
            "below_threshold"
        };
        return insertion(reason, Some(rule), support, sites.len());
    }
    let edits: Vec<Edit> = sites
        .iter()
        .take(MAX_EDITS)
        .map(|&s| Edit {
            start: s,
            end: s + rule.find.len(),
            new_text: rule.replace.clone(),
        })
        .collect();
    Prediction {
        edits_capped: sites.len() > MAX_EDITS,
        sites: sites.len(),
        support,
        rule: Some(rule),
        edits,
        reason_silent: None,
    }
}

// ---- UTF-16 ↔ byte offset mapping ------------------------------------
//
// The wire contract (§3) is UTF-16 code units — the native offset
// space of every editor client we serve — while all Rust-side work is
// byte offsets. Conversion lives here, next to the code that makes it
// necessary.

/// Byte offset for a UTF-16 code-unit offset, clamped into the text.
pub fn utf16_to_byte(text: &str, utf16: usize) -> usize {
    let mut u = 0;
    for (i, c) in text.char_indices() {
        if u >= utf16 {
            return i;
        }
        u += c.len_utf16();
    }
    text.len()
}

/// UTF-16 code-unit offsets for the given byte offsets (any order),
/// in one pass over the text. Offsets inside a char round down.
pub fn bytes_to_utf16(text: &str, byte_offsets: &[usize]) -> Vec<usize> {
    let mut sorted: Vec<usize> = byte_offsets.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut map = std::collections::HashMap::new();
    let mut next = sorted.iter().copied().peekable();
    let mut u = 0;
    for (i, c) in text.char_indices() {
        while next.peek().is_some_and(|&b| b <= i) {
            map.insert(next.next().unwrap(), u);
        }
        u += c.len_utf16();
    }
    for b in next {
        map.insert(b, u);
    }
    byte_offsets.iter().map(|b| map[b]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(before: &str, after: &str, left: &str, right: &str) -> HistoryUnit {
        HistoryUnit {
            before: before.into(),
            after: after.into(),
            left: left.into(),
            right: right.into(),
        }
    }

    fn console_rule() -> GuardedRule {
        expand_rule(&unit("log", "debug", "  console.", "(\"a\");")).unwrap()
    }

    #[test]
    fn expand_absorbs_member_access_and_call_paren() {
        let r = console_rule();
        assert_eq!(r.find, "console.log(");
        assert_eq!(r.replace, "console.debug(");
        assert!(r.guard_left, "identifier start must be guarded");
        assert!(!r.guard_right, "call paren needs no guard");
    }

    #[test]
    fn expand_guards_pure_identifier_rename_both_ends() {
        // inserted "Next" after "count": `count` → `countNext`
        let r = expand_rule(&unit("", "Next", "let count", " = count + 1;")).unwrap();
        assert_eq!(r.find, "count");
        assert_eq!(r.replace, "countNext");
        assert!(r.guard_left && r.guard_right);
        // the guards prevent the rule re-matching its own output
        assert_eq!(
            find_guarded_sites("let countNext = countNext + 1;", &r, 0),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn expand_declines_noops_and_multiline() {
        assert!(expand_rule(&unit("b", "b", "a", "c")).is_none());
        assert!(expand_rule(&unit("x", "\n", "a", "c")).is_none());
    }

    #[test]
    fn induce_anchors_most_recent_and_window_counts() {
        let a = Some(console_rule());
        let b = expand_rule(&unit("foo", "bar", " ", " "));
        let got = induce(&[a.clone(), b, None, a.clone()]).unwrap();
        assert_eq!(got.0, console_rule());
        assert_eq!(got.1, 2, "support survives interleaved noise");
        assert!(induce(&[]).is_none());
        assert!(induce(&[None, None]).is_none());
    }

    #[test]
    fn threshold_table() {
        let specific = console_rule(); // find len 12
        let short = GuardedRule {
            find: "ab".into(),
            replace: "ax".into(),
            guard_left: true,
            guard_right: true,
        };
        let single = GuardedRule {
            find: "a".into(),
            replace: "x".into(),
            guard_left: true,
            guard_right: true,
        };
        let at_bar = GuardedRule {
            find: "abcde".into(), // exactly MIN_RULE_CHARS
            replace: "abcdx".into(),
            guard_left: true,
            guard_right: true,
        };
        let under_bar = GuardedRule {
            find: "abcd".into(), // one short
            replace: "abcx".into(),
            guard_left: true,
            guard_right: true,
        };
        assert!(!should_fire(&specific, 5, 0), "never without a site");
        assert!(should_fire(&specific, 2, 1));
        assert!(!should_fire(&specific, 1, 10), "one edit never fires");
        // The minimum `find` is 5 trimmed chars, RAISED from 2 on the
        // measured frontier (see `should_fire`): at 2 the config is
        // Pareto-dominated — 138 strict-useful against 52 wrong fires,
        // where 5 gets 141 against 39. Pin both sides of the bar so the
        // constant cannot drift without a test saying so.
        assert!(should_fire(&at_bar, 2, 10), "a rule at the bar fires");
        assert!(
            !should_fire(&under_bar, 2, 10),
            "one char under the bar does not"
        );
        assert!(
            !should_fire(&short, 2, 10),
            "a two-char rule is the literal trap"
        );
        assert!(
            !should_fire(&single, 2, 10),
            "one-char rule is never specific enough"
        );
        assert!(
            !should_fire(&single, 9, 10),
            "and no amount of support rescues it"
        );
        // The support tier stayed collapsed across the 2→5 move: more
        // support still does not buy a shorter rule.
        assert_eq!(should_fire(&short, 2, 10), should_fire(&short, 7, 10));
        assert_eq!(
            should_fire(&under_bar, 2, 10),
            should_fire(&under_bar, 9, 10)
        );
    }

    /// Two insertions of one block, at siblings differing only in the
    /// value on the anchor line.
    fn insert_pair() -> Vec<HistoryUnit> {
        vec![
            unit(
                "",
                "  retries: 3,\n",
                "a = {\n  port: 1,\n  os: linux,\n",
                "};\n",
            ),
            unit(
                "",
                "  retries: 3,\n",
                "b = {\n  port: 2,\n  os: linux,\n",
                "};\n",
            ),
        ]
    }

    const INSERT_ANCHOR: &str = "  os: linux,\n";

    #[test]
    fn insertion_induces_an_anchored_rule() {
        let r = induce_insertion(&insert_pair()).expect("pair induces");
        // The anchor is the common line-aligned tail — the shared line
        // that precedes both insertion points. The differing `port:`
        // lines above it are not part of what the contexts agree on.
        assert_eq!(r.find, INSERT_ANCHOR);
        assert_eq!(r.replace, format!("{}{}", r.find, "  retries: 3,\n"));
        assert!(
            !r.guard_left && !r.guard_right,
            "line anchors have no word boundaries"
        );
    }

    #[test]
    fn insertion_refuses_what_it_cannot_know() {
        // Per-site-varying payload: which variant belongs at an unseen
        // site is a guess, and this lane does not guess.
        let varying = vec![
            unit("", "  retries: 3,\n", "a = {\n  port: 1,\n", "};\n"),
            unit("", "  retries: 9,\n", "b = {\n  port: 2,\n", "};\n"),
        ];
        assert!(
            induce_insertion(&varying).is_none(),
            "varying payload is param_insert's shape"
        );

        // Not a pure insertion — that is a rewrite, and `expand_rule`
        // already owns it.
        let rewrite = vec![
            unit("log", "debug", "console.", "(1);"),
            unit("log", "debug", "console.", "(2);"),
        ];
        assert!(induce_insertion(&rewrite).is_none());

        // Nothing in common to anchor on.
        let unanchored = vec![
            unit("", "x\n", "totally\n", ""),
            unit("", "x\n", "different\n", ""),
        ];
        assert!(
            induce_insertion(&unanchored).is_none(),
            "an anchor must be specific"
        );

        assert!(induce_insertion(&[]).is_none());
    }

    #[test]
    fn insertion_is_a_fallback_and_never_preempts_the_literal_lane() {
        // Literal lane declines (no rule from a bare insertion), so the
        // anchor lane fires on the remaining sibling.
        let text = "a = {\n  port: 1,\n  os: linux,\n  retries: 3,\n};\n\
                    b = {\n  port: 2,\n  os: linux,\n  retries: 3,\n};\n\
                    c = {\n  port: 3,\n  os: linux,\n};\n";
        let p = predict(&insert_pair(), text, 0);
        assert!(
            p.reason_silent.is_none(),
            "anchor lane fires where the literal lane cannot"
        );
        assert_eq!(p.edits.len(), 1, "only the site still missing the block");
        assert_eq!(
            p.edits[0].new_text,
            format!("{INSERT_ANCHOR}  retries: 3,\n")
        );

        // A site that already carries the payload is excluded, so a
        // fully-propagated document stays silent rather than stacking.
        let done = "a = {\n  port: 1,\n  os: linux,\n  retries: 3,\n};\n";
        let q = predict(&insert_pair(), done, 0);
        assert!(
            q.edits.is_empty(),
            "already-applied sites are not re-proposed"
        );

        // And where the literal lane DOES fire it keeps the floor: the
        // more specific claim wins.
        let owns = vec![
            unit("log", "debug", "console.", "(1);"),
            unit("log", "debug", "console.", "(2);"),
        ];
        let r = predict(&owns, "console.log(3);\n", 0);
        assert_eq!(r.rule.as_ref().unwrap().find, "console.log(");
    }

    #[test]
    fn deletion_repeats_only_multi_line_blocks() {
        let block = "  legacy: true,\n  deprecated: yes,\n";
        let pair = vec![
            unit(block, "", "a = {\n", "};\n"),
            unit(block, "", "b = {\n", "};\n"),
        ];
        let r = induce_deletion(&pair).expect("multi-line repeat induces");
        assert_eq!(r.find, block);
        assert!(r.replace.is_empty(), "a deletion is an empty replace");

        // ONE line is refused: measured at 13 wrong fires across the
        // bank's negatives, because a short literal recurs innocently.
        let one = vec![
            unit("  legacy: true,\n", "", "a = {\n", "};\n"),
            unit("  legacy: true,\n", "", "b = {\n", "};\n"),
        ];
        assert!(
            induce_deletion(&one).is_none(),
            "single-line deletion repeat is not safe"
        );

        // Differing blocks are a bulk-removal run, not a repeat — the
        // shape this lane explicitly does not attempt.
        let differing = vec![
            unit("  a: 1,\n  b: 2,\n", "", "x = {\n", "};\n"),
            unit("  c: 3,\n  d: 4,\n", "", "y = {\n", "};\n"),
        ];
        assert!(induce_deletion(&differing).is_none());

        // Not a deletion at all.
        assert!(induce_deletion(&insert_pair()).is_none());
    }

    fn bare_rule(find: &str, replace: &str) -> GuardedRule {
        GuardedRule {
            find: find.into(),
            replace: replace.into(),
            guard_left: false,
            guard_right: false,
        }
    }

    #[test]
    fn sites_order_after_cursor_then_wrap() {
        let text = "log a\nlog b\nlog c\n";
        assert_eq!(
            find_guarded_sites(text, &bare_rule("log", "dbg"), 3),
            vec![6, 12, 0]
        );
        // non-overlapping stepping
        assert_eq!(
            find_guarded_sites("aaaa", &bare_rule("aa", "bb"), 0),
            vec![0, 2]
        );
    }

    #[test]
    fn insertion_rule_skips_already_applied_sites() {
        // `fetch(` → `await fetch(`: replace CONTAINS find, so edited
        // lines still match textually — re-proposing them would stack
        // the insertion. Found by gym/next-edit case a11 (2026-07-30).
        let text = "a = await fetch(u1)\nb = await fetch(u2)\nc = fetch(u3)\nd = fetch(u4)\n";
        let u = unit("", "await ", "b = ", "fetch(u2)");
        let p = predict(&[u.clone(), u], text, 0);
        assert!(p.reason_silent.is_none());
        assert_eq!(p.edits.len(), 2, "only the two un-edited sites");
        for e in &p.edits {
            assert_eq!(&text[e.start..e.end], "fetch(");
            assert!(
                !text[..e.start].ends_with("await "),
                "already-edited site re-proposed"
            );
        }
        // Deletion-shaped rules (find contains replace) are unaffected.
        let d = expand_rule(&unit(".unwrap()", "", "res", ";")).unwrap();
        assert_eq!(find_guarded_sites("res;\nres.unwrap();\n", &d, 0), vec![5]);
    }

    /// The already-applied probe used to re-derive `find`'s positions
    /// inside `replace` at EVERY site, so a self-similar rule over a
    /// large file was quadratic: a crafted 512 KiB request measured
    /// 23 s of blocking CPU on one tokio worker, needing no model lane
    /// and no opt-in to reach. Rules this shape are declined, and the
    /// site scan is bounded regardless.
    #[test]
    fn pathological_rules_cannot_wedge_the_scan() {
        let text = "(".repeat(512 * 1024);
        let degenerate = bare_rule("(", &format!("{}{}", "(".repeat(2047), ")"));
        let t0 = std::time::Instant::now();
        assert!(
            find_guarded_sites(&text, &degenerate, 0).is_empty(),
            "a replacement containing its own target 2047 times is not an edit pattern"
        );
        // Same file, a rule that is merely common rather than
        // degenerate: bounded work, still a usable queue.
        let common = bare_rule("(", "(x");
        let sites = find_guarded_sites(&text, &common, 0);
        assert!(!sites.is_empty() && sites.len() <= MAX_SITE_SCAN);
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(2),
            "bounded work, not seconds of blocking CPU (took {:?})",
            t0.elapsed()
        );
        // An oversized induced rule is refused outright.
        assert!(find_guarded_sites("abc", &bare_rule(&"a".repeat(600), "b"), 0).is_empty());
    }

    #[test]
    fn predict_end_to_end_console_case() {
        // Two sites already edited (units), three remain; cursor after
        // the second edit.
        let text = "console.debug(1);\nconsole.debug(2);\nconsole.log(3);\nconsole.log(4);\nconsole.log(5);\n";
        let u = unit("log", "debug", "console.", "(2);");
        let p = predict(&[u.clone(), u], text, 30);
        assert!(p.reason_silent.is_none());
        assert_eq!(p.support, 2);
        assert_eq!(p.sites, 3);
        assert_eq!(p.edits.len(), 3);
        let first = &p.edits[0];
        assert_eq!(&text[first.start..first.end], "console.log(");
        assert_eq!(first.new_text, "console.debug(");
    }

    #[test]
    fn predict_silences_carry_reasons() {
        assert_eq!(predict(&[], "x", 0).reason_silent, Some("no_rule"));
        let u = unit("log", "debug", "console.", "(1);");
        // one support only
        assert_eq!(
            predict(&[u.clone()], "console.log(9);", 0).reason_silent,
            Some("below_threshold")
        );
        // rule supported but no sites left
        let p = predict(&[u.clone(), u], "console.debug(9);", 0);
        assert_eq!(p.reason_silent, Some("no_sites"));
        assert_eq!(p.support, 2);
    }

    #[test]
    fn utf16_mapping_round_trips_past_astral_chars() {
        // "a💡b" — '💡' is 4 bytes / 2 UTF-16 units.
        let text = "a💡bc";
        assert_eq!(utf16_to_byte(text, 0), 0);
        assert_eq!(utf16_to_byte(text, 1), 1);
        assert_eq!(utf16_to_byte(text, 3), 5); // past the emoji
        assert_eq!(utf16_to_byte(text, 99), text.len());
        assert_eq!(bytes_to_utf16(text, &[5, 0, 1, 6, 7]), vec![3, 0, 1, 4, 5]);
    }
}
