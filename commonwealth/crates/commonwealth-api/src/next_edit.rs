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
/// [`should_fire`] for the measured curve that chose 2.
const MIN_RULE_CHARS: usize = 2;

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
/// The support-2 minimum was 4 and is 2, lowered 2026-08-06 on a
/// measured curve rather than taste. Swept over the whole golden set
/// (`gym/next-edit/golden/`, 1,098 cases, model lane off so the rule
/// lane is isolated):
///
/// | min | useful | wrong-fire |
/// |-----|--------|------------|
/// |  2  | 35.4%  | 16.6%      |
/// |  3  | 34.7%  | 16.8%      |  <- dominated: fewer useful, same 50 wrong
/// |  4  | 33.1%  | 15.8%      |
/// |  5  | 30.0%  | 14.1%      |
///
/// In the shipped configuration the 4→2 move is **+17 useful edits for
/// +6 wrong fires, with nothing regressing** (paired, deterministic:
/// 19 positives changed and every one came out of `missed` — 14 to
/// `partial`, 3 to `useful`, 2 to `wrong`; 4 negatives went
/// silent→wrong). 14 of the 17 are `partial`, which §6 reports and
/// deliberately does not gate — the user tabs past an over-offer.
/// Raising it back costs those 17; note `53abe423` carries the sweep.
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

/// The whole rule-lane pipeline: history → rule → sites → threshold
/// → queue. `cursor` is a byte offset into `text`.
pub fn predict(history: &[HistoryUnit], text: &str, cursor: usize) -> Prediction {
    let silent = |reason, rule: Option<GuardedRule>, support, sites| Prediction {
        edits: Vec::new(),
        rule,
        support,
        sites,
        edits_capped: false,
        reason_silent: Some(reason),
    };

    let rules: Vec<Option<GuardedRule>> = history.iter().map(expand_rule).collect();
    let Some((rule, support)) = induce(&rules) else {
        return silent("no_rule", None, 0, 0);
    };
    let sites = find_guarded_sites(text, &rule, cursor);
    if !should_fire(&rule, support, sites.len()) {
        let reason = if sites.is_empty() {
            "no_sites"
        } else {
            "below_threshold"
        };
        return silent(reason, Some(rule), support, sites.len());
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
        assert!(!should_fire(&specific, 5, 0), "never without a site");
        assert!(should_fire(&specific, 2, 1));
        assert!(!should_fire(&specific, 1, 10), "one edit never fires");
        // The minimum `find` is 2 chars, lowered from 4 at support 2 on
        // the golden set's measured curve (see `should_fire`). A
        // two-char rule at two supports is exactly the band that
        // recovered — 17 useful edits for 6 wrong fires.
        assert!(should_fire(&short, 2, 10), "two-char rule fires at 2 supports");
        assert!(!should_fire(&single, 2, 10), "one-char rule is never specific enough");
        assert!(!should_fire(&single, 9, 10), "and no amount of support rescues it");
        // The support tier collapsed when the curve chose 2: more
        // support no longer buys a shorter rule.
        assert_eq!(should_fire(&short, 2, 10), should_fire(&short, 7, 10));
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
