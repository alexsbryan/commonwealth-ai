// SPDX-License-Identifier: AGPL-3.0-or-later
//! Model-lane next-edit prediction (`sovereign/docs/NEXT_EDIT.md` §2,
//! §4, §8 P2) — the pure half: consult gate, needle derivation, region
//! selection, prompt shaping, output parsing, and region diffing. No
//! inference here; the route layer owns the `LocalInferenceService`
//! call and threads its output through [`parse_rewrite`] +
//! [`diff_region`].
//!
//! The gate is deterministic and mirrored by the Python replica in
//! `gym/next-edit/gen/author.py` — a divergence fails the §6
//! generalization bank loudly whichever side is wrong. Every check
//! prefers silence: the model is consulted only when the rule lane
//! declined AND the recent units are similar-but-not-identical, and a
//! model output that fails validation is dropped, not repaired.

use crate::next_edit::{expand_rule, find_guarded_sites, GuardedRule, HistoryUnit, Prediction,
                       HISTORY_WINDOW};

/// Total lines in the rewrite region handed to the model.
pub const REGION_LINES: usize = 24;
/// Two per-site-varying replacements must share this many prefix chars.
const MIN_PARAM_PREFIX: usize = 4;
/// A shorter needle is noise, not an anchor.
const MIN_NEEDLE: usize = 3;
/// Most-recent real units rendered into the prompt.
const MAX_PROMPT_UNITS: usize = 4;
/// A rewrite growing the region beyond this is a runaway, not an edit.
const MAX_GROWTH_BYTES: usize = 2048;
/// A rewrite adding/removing more lines than this is a runaway.
const MAX_LINE_DELTA: usize = 16;

/// One region-relative proposed replacement (route adds the region
/// origin before converting to the UTF-16 wire).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionEdit {
    pub start: usize,
    pub end: usize,
    pub new_text: String,
}

/// Outcome of the consult gate — glassbox either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Consult {
    No { skipped: &'static str },
    Yes { reason: &'static str, needle: Option<String> },
}

// ---- casing renderings ------------------------------------------------

const STYLES: [Style; 4] = [Style::Snake, Style::Screaming, Style::Camel, Style::Pascal];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Snake,
    Screaming,
    Camel,
    Pascal,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `getUserData` → `[get, user, data]`; `PARSE_ARGS` → `[parse, args]`.
fn split_words(run: &str) -> Vec<String> {
    let mut words = Vec::new();
    for piece in run.split('_') {
        let mut w = String::new();
        for ch in piece.chars() {
            if !w.is_empty() && ch.is_uppercase() && !w.chars().next_back().unwrap().is_uppercase()
            {
                words.push(w);
                w = String::new();
            }
            w.push(ch);
        }
        if !w.is_empty() {
            words.push(w);
        }
    }
    words.iter().map(|w| w.to_lowercase()).collect()
}

fn render_words(words: &[String], style: Style) -> String {
    fn cap(w: &str) -> String {
        let mut cs = w.chars();
        match cs.next() {
            Some(c) => c.to_uppercase().collect::<String>() + cs.as_str(),
            None => String::new(),
        }
    }
    match style {
        Style::Snake => words.join("_"),
        Style::Screaming => {
            words.iter().map(|w| w.to_uppercase()).collect::<Vec<_>>().join("_")
        }
        Style::Camel => {
            let mut out = words.first().cloned().unwrap_or_default();
            for w in &words[1.min(words.len())..] {
                out.push_str(&cap(w));
            }
            out
        }
        Style::Pascal => words.iter().map(|w| cap(w)).collect(),
    }
}

/// Re-render every identifier run of `s` in `style`, separators verbatim.
fn restyle(s: &str, style: Style) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut run = String::new();
    for c in s.chars() {
        if is_word(c) {
            run.push(c);
        } else {
            if !run.is_empty() {
                out.push_str(&render_words(&split_words(&run), style));
                run.clear();
            }
            out.push(c);
        }
    }
    if !run.is_empty() {
        out.push_str(&render_words(&split_words(&run), style));
    }
    out
}

/// First casing rendering of `rule.find` with a live guarded site in
/// `text` (styles probed in fixed order; already-applied sites of the
/// same-styled replace are excluded, mirroring the rule lane).
fn casing_variant_needle(text: &str, rule: &GuardedRule) -> Option<String> {
    for style in STYLES {
        let vfind = restyle(&rule.find, style);
        if vfind == rule.find {
            continue;
        }
        let vrule = GuardedRule {
            guard_left: vfind.chars().next().is_some_and(crate::next_edit::is_word_char),
            guard_right: vfind.chars().next_back().is_some_and(crate::next_edit::is_word_char),
            replace: restyle(&rule.replace, style),
            find: vfind,
        };
        if !find_guarded_sites(text, &vrule, 0).is_empty() {
            return Some(vrule.find);
        }
    }
    None
}

// ---- similarity helpers ----------------------------------------------

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Longest common substring (chars). Inputs are short unit strings.
fn lcsubstr(a: &str, b: &str) -> String {
    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    let mut best_len = 0usize;
    let mut best_end = 0usize;
    let mut prev = vec![0usize; bc.len() + 1];
    for i in 1..=ac.len() {
        let mut cur = vec![0usize; bc.len() + 1];
        for j in 1..=bc.len() {
            if ac[i - 1] == bc[j - 1] {
                cur[j] = prev[j - 1] + 1;
                if cur[j] > best_len {
                    best_len = cur[j];
                    best_end = i;
                }
            }
        }
        prev = cur;
    }
    ac[best_end - best_len..best_end].iter().collect()
}

fn ctx_needle(a: &HistoryUnit, b: &HistoryUnit) -> Option<String> {
    let sa = format!("{}{}{}", a.left, a.before, a.right);
    let sb = format!("{}{}{}", b.left, b.before, b.right);
    let s = lcsubstr(&sa, &sb).trim().to_string();
    (s.chars().count() >= MIN_NEEDLE).then_some(s)
}

// ---- the consult gate -------------------------------------------------

/// Decide whether the model lane may be consulted. `p` is the rule
/// lane's outcome on the same request; a fired rule lane always wins.
pub fn should_consult(history: &[HistoryUnit], text: &str, p: &Prediction) -> Consult {
    if !p.edits.is_empty() {
        return Consult::No { skipped: "rule_fired" };
    }
    let window_start = history.len().saturating_sub(HISTORY_WINDOW);
    let cores: Vec<&HistoryUnit> =
        history[window_start..].iter().filter(|u| u.before != u.after).collect();
    if cores.len() < 2 {
        return Consult::No { skipped: "gate" };
    }
    let (a, b) = (cores[cores.len() - 1], cores[cores.len() - 2]);
    let (ra, rb) = (expand_rule(a), expand_rule(b));

    // 1. Casing variant: a real, exhausted literal rule whose rename
    // remains at another casing of the same token sequence. DETECTED
    // but DEFERRED in v1: bank runs 1–2 (2026-07-30) showed Mellum2
    // destructive on exactly this shape — 4 of 9 casing fires wrong
    // (block deletion, reversed rename) against 0 of 60 wrong
    // everywhere else, and a casing-specific instruction made it
    // worse, not better. The variant find/replace computed here is
    // fully deterministic, so the right home for this category is a
    // rule-engine sub-lane, not a model consult; until that exists
    // the gate declines, by name.
    if p.reason_silent == Some("no_sites") && p.support >= 2 {
        if let Some(rule) = p.rule.as_ref() {
            if casing_variant_needle(text, rule).is_some() {
                return Consult::No { skipped: "casing_deferred" };
            }
        }
    }

    let multiline = |u: &HistoryUnit| u.before.contains('\n') || u.after.contains('\n');

    // 2. Multiline fan-out: identical multi-line insertion at two sites —
    // uninducible by design, but unmistakably one pattern.
    if ra.is_none()
        && rb.is_none()
        && multiline(a)
        && multiline(b)
        && a.before == b.before
        && a.after == b.after
        && a.after.trim().chars().count() >= MIN_NEEDLE
    {
        return Consult::Yes { reason: "multiline_fanout", needle: ctx_needle(a, b) };
    }

    if let (Some(ra), Some(rb)) = (&ra, &rb) {
        if ra != rb {
            // 3. Fan-out insert: identical cores, differing contexts.
            if a.before == b.before && a.after == b.after {
                return Consult::Yes { reason: "fanout_insert", needle: ctx_needle(a, b) };
            }
            // 4. Param insert: same target, per-site-varying replacement
            // sharing a meaningful prefix.
            if a.before == b.before
                && a.after != b.after
                && common_prefix_len(&a.after, &b.after) >= MIN_PARAM_PREFIX
            {
                let needle = if a.before.trim().is_empty() {
                    ctx_needle(a, b)
                } else {
                    Some(a.before.clone())
                };
                return Consult::Yes { reason: "param_insert", needle };
            }
        }
    }
    Consult::No { skipped: "gate" }
}

// ---- region selection -------------------------------------------------

/// Byte range of the ~[`REGION_LINES`]-line window the model may
/// rewrite, anchored on the needle's next occurrence from the cursor
/// (wrapping), else on the cursor line. Returns `(start, end,
/// needle_hit)`; the range always ends on a line boundary (or EOF).
pub fn select_region(text: &str, cursor: usize, needle: Option<&str>) -> (usize, usize, bool) {
    let anchor = needle
        .filter(|n| !n.is_empty())
        .and_then(|n| {
            text.get(cursor..)
                .and_then(|t| t.find(n).map(|i| cursor + i))
                .or_else(|| text.get(..cursor).and_then(|t| t.rfind(n)))
        });
    let hit = anchor.is_some();
    let target = anchor.unwrap_or_else(|| cursor.min(text.len()));

    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.char_indices().filter(|(_, c)| *c == '\n').map(|(i, _)| i + 1))
        .collect();
    let line_count = if text.ends_with('\n') { starts.len() - 1 } else { starts.len() };
    let target_line = starts.partition_point(|&s| s <= target).saturating_sub(1);
    // Center on the target, then slide up at EOF so the window keeps
    // its full height whenever the document allows it.
    let first = target_line
        .saturating_sub(REGION_LINES / 2 - 1)
        .min(line_count.saturating_sub(REGION_LINES));
    let start = starts[first.min(starts.len() - 1)];
    let end = starts.get(first + REGION_LINES).copied().unwrap_or(text.len());
    (start, end, hit)
}

// ---- prompt -----------------------------------------------------------

pub const REGION_START_MARKER: &str = "<|editable_region_start|>";
pub const REGION_END_MARKER: &str = "<|editable_region_end|>";

/// Zeta-shaped instruct prompt: edit history as diff snippets + the
/// marker-bracketed region, output = the rewritten region. `reason` is
/// the consult-gate verdict — casing consults carry an extra
/// instruction, because the first bank run showed the model either
/// ignores cross-casing renames or turns destructive on them
/// (gym/next-edit/gen, run 1: cv01 deleted an unrelated block, cv07
/// applied the rename backwards).
pub fn build_prompt(
    history: &[HistoryUnit],
    region: &str,
    path: Option<&str>,
    language: Option<&str>,
    reason: &str,
) -> String {
    let mut p = String::with_capacity(region.len() * 2 + 512);
    p.push_str(
        "You are the next-edit engine of a code editor. \
         The developer just made these edits, oldest first:\n",
    );
    let cores: Vec<&HistoryUnit> = history.iter().filter(|u| u.before != u.after).collect();
    let shown = &cores[cores.len().saturating_sub(MAX_PROMPT_UNITS)..];
    for (i, u) in shown.iter().enumerate() {
        p.push_str(&format!(
            "\nEdit {}:\n-{}{}{}\n+{}{}{}\n",
            i + 1,
            u.left,
            u.before,
            u.right,
            u.left,
            u.after,
            u.right
        ));
    }
    let file = match (path, language) {
        (Some(pa), Some(l)) => format!("`{pa}` ({l})"),
        (Some(pa), None) => format!("`{pa}`"),
        (None, Some(l)) => format!("a {l} file"),
        (None, None) => "the file".to_string(),
    };
    let casing_note = if reason == "casing_variant" {
        "The developer's rename may also apply to identifiers spelled in \
         other casing styles (snake_case, camelCase, PascalCase, \
         SCREAMING_SNAKE). Apply the same rename to those too, keeping \
         each identifier's existing casing style. "
    } else {
        ""
    };
    p.push_str(&format!(
        "\nContinue the developer's pattern. Below is a region of {file}. \
         Rewrite it, applying the developer's pattern wherever it applies \
         within the region. {casing_note}Make ONLY edits that continue \
         the pattern, in the same direction the developer made them: \
         never undo their edits, never delete or rewrite unrelated code, \
         never add anything the pattern does not call for. Reply with \
         ONLY the rewritten region — every line of it, no markers, no \
         code fence, no explanation. If the pattern applies nowhere in \
         the region, reply with the region exactly \
         unchanged.\n\n{REGION_START_MARKER}\n{region}{REGION_END_MARKER}\n",
    ));
    p
}

// ---- output parsing ---------------------------------------------------

/// Validate + normalize the model's output into a rewritten region.
/// Errors are glassbox drop reasons (`sovereign_debug.model.dropped`):
/// no suggestion beats a wrong one, so anything suspicious is dropped
/// whole — never repaired into a partial edit.
pub fn parse_rewrite(raw: &str, region: &str) -> Result<String, &'static str> {
    let mut out = raw;
    // Thinking-model preamble (alias mode can route to a chat model).
    if let Some(rest) = out.trim_start().strip_prefix("<think>") {
        out = match rest.split_once("</think>") {
            Some((_, tail)) => tail,
            None => return Err("invalid"),
        };
    }
    let mut s = out.trim_start_matches('\n').to_string();
    // One wrapping code fence, with or without a language tag.
    let t = s.trim();
    if t.starts_with("```") {
        let inner = t.trim_start_matches("```");
        let body = inner.split_once('\n').map(|(_, b)| b).unwrap_or("");
        s = match body.rfind("```") {
            Some(i) => body[..i].to_string(),
            None => return Err("invalid"),
        };
    }
    // Echoed markers: strip marker-only lines; any other echo is malformed.
    if s.contains(REGION_START_MARKER) || s.contains(REGION_END_MARKER) {
        s = s
            .lines()
            .filter(|l| {
                let lt = l.trim();
                lt != REGION_START_MARKER && lt != REGION_END_MARKER
            })
            .map(|l| format!("{l}\n"))
            .collect();
    }
    if s.contains("editable_region") {
        return Err("invalid");
    }
    if region.ends_with('\n') && !s.ends_with('\n') {
        s.push('\n');
    }
    if s.trim().is_empty() {
        return Err("invalid");
    }
    if s.len() > region.len() + MAX_GROWTH_BYTES {
        return Err("invalid");
    }
    let delta = (s.lines().count() as i64 - region.lines().count() as i64).unsigned_abs() as usize;
    if delta > MAX_LINE_DELTA {
        return Err("invalid");
    }
    if s == region {
        return Err("noop");
    }
    Ok(s)
}

// ---- region diff ------------------------------------------------------

/// Line-LCS diff of the region against its rewrite, one [`RegionEdit`]
/// per contiguous hunk (char-trimmed), in document order — the same
/// tab-through queue shape the rule lane produces.
pub fn diff_region(orig: &str, new: &str) -> Vec<RegionEdit> {
    let o: Vec<&str> = orig.split_inclusive('\n').collect();
    let n: Vec<&str> = new.split_inclusive('\n').collect();
    // LCS table over lines.
    let mut dp = vec![vec![0usize; n.len() + 1]; o.len() + 1];
    for i in (0..o.len()).rev() {
        for j in (0..n.len()).rev() {
            dp[i][j] = if o[i] == n[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut opos = Vec::with_capacity(o.len() + 1);
    let mut acc = 0;
    for l in &o {
        opos.push(acc);
        acc += l.len();
    }
    opos.push(acc);

    let mut edits = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let mut hunk_start: Option<(usize, usize)> = None;
    loop {
        let matched = i < o.len() && j < n.len() && o[i] == n[j] && dp[i][j] == dp[i + 1][j + 1] + 1;
        if matched || (i >= o.len() && j >= n.len()) {
            if let Some((hi, hj)) = hunk_start.take() {
                let old_start = opos[hi];
                let old_end = opos[i];
                let new_text: String = n[hj..j].concat();
                edits.push(trim_hunk(orig, old_start, old_end, new_text));
            }
            if i >= o.len() && j >= n.len() {
                break;
            }
            i += 1;
            j += 1;
        } else {
            if hunk_start.is_none() {
                hunk_start = Some((i, j));
            }
            if i < o.len() && (j >= n.len() || dp[i + 1][j] >= dp[i][j + 1]) {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    edits
}

/// Char-level prefix/suffix trim inside one hunk, keeping offsets on
/// char boundaries.
fn trim_hunk(orig: &str, old_start: usize, old_end: usize, new_text: String) -> RegionEdit {
    let old = &orig[old_start..old_end];
    let mut p = 0;
    for (co, cn) in old.chars().zip(new_text.chars()) {
        if co != cn {
            break;
        }
        p += co.len_utf8();
    }
    let mut s = 0;
    for (co, cn) in old[p..].chars().rev().zip(new_text[p..].chars().rev()) {
        if co != cn {
            break;
        }
        s += co.len_utf8();
    }
    RegionEdit {
        start: old_start + p,
        end: old_end - s,
        new_text: new_text[p..new_text.len() - s].to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::next_edit::predict;

    fn unit(before: &str, after: &str, left: &str, right: &str) -> HistoryUnit {
        HistoryUnit {
            before: before.into(),
            after: after.into(),
            left: left.into(),
            right: right.into(),
        }
    }

    #[test]
    fn restyle_renders_all_casings() {
        assert_eq!(restyle("getUserData(", Style::Snake), "get_user_data(");
        assert_eq!(restyle("getUserData(", Style::Screaming), "GET_USER_DATA(");
        assert_eq!(restyle("get_user_data(", Style::Camel), "getUserData(");
        assert_eq!(restyle("parse_args", Style::Pascal), "ParseArgs");
        assert_eq!(restyle("MAX_RETRIES", Style::Camel), "maxRetries");
    }

    #[test]
    fn gate_casing_variant_is_detected_but_deferred() {
        let h = vec![
            unit("getUserData", "fetchUserData", "  const raw = ", "(userId);"),
            unit("getUserData", "fetchUserData", "  const avatar = ", "(uid);"),
        ];
        let with_variant = "const a = fetchUserData(x);\nconst b = fetchUserData(y);\n\
                            const c = get_user_data(z);\n";
        let p = predict(&h, with_variant, 0);
        assert_eq!(p.reason_silent, Some("no_sites"));
        // Detection is live (the needle machinery must keep working for
        // the future rule sub-lane) but the consult is declined by name.
        assert_eq!(
            casing_variant_needle(with_variant, p.rule.as_ref().unwrap()).as_deref(),
            Some("get_user_data(")
        );
        assert_eq!(
            should_consult(&h, with_variant, &p),
            Consult::No { skipped: "casing_deferred" }
        );
        // Same history, no variant anywhere: plain gate refusal.
        let without = "const a = fetchUserData(x);\nconst b = fetchUserData(y);\n";
        let p2 = predict(&h, without, 0);
        assert_eq!(
            should_consult(&h, without, &p2),
            Consult::No { skipped: "gate" }
        );
    }

    #[test]
    fn gate_fanout_param_and_multiline() {
        let text = "dial(a, x)\ndial(b, y)\ndial(c, z)\n";
        let fanout = vec![
            unit("", ", tmo", "dial(a, x", ")"),
            unit("", ", tmo", "dial(b, y", ")"),
        ];
        let p = predict(&fanout, text, 0);
        assert!(matches!(
            should_consult(&fanout, text, &p),
            Consult::Yes { reason: "fanout_insert", .. }
        ));

        let param = vec![
            unit("unwrap()", "expect(\"a\")", "x().", ";"),
            unit("unwrap()", "expect(\"b\")", "y().", ";"),
        ];
        let t2 = "z().unwrap();\n";
        let p2 = predict(&param, t2, 0);
        match should_consult(&param, t2, &p2) {
            Consult::Yes { reason: "param_insert", needle } => {
                assert_eq!(needle.as_deref(), Some("unwrap()"));
            }
            other => panic!("expected param consult, got {other:?}"),
        }

        let ml = vec![
            unit("", "\n  retries: 3,", "port: 1,", "\n};"),
            unit("", "\n  retries: 3,", "port: 2,", "\n};"),
        ];
        let t3 = "a\nb\nc\n";
        let p3 = predict(&ml, t3, 0);
        assert!(matches!(
            should_consult(&ml, t3, &p3),
            Consult::Yes { reason: "multiline_fanout", .. }
        ));
    }

    #[test]
    fn gate_refusals() {
        // Dissimilar cores.
        let h = vec![
            unit("parseHeader", "readHeader", "h = ", "(buf);"),
            unit("5000", "8000", "t = wait(", ");"),
        ];
        let t = "x = wait(5000);\n";
        let p = predict(&h, t, 0);
        assert_eq!(should_consult(&h, t, &p), Consult::No { skipped: "gate" });
        // Rule lane fired: the model must never be consulted.
        let owns = vec![
            unit("log", "debug", "console.", "(\"a\");"),
            unit("log", "debug", "console.", "(\"b\");"),
        ];
        let t2 = "console.log(\"c\");\n";
        let p2 = predict(&owns, t2, 0);
        assert!(!p2.edits.is_empty());
        assert_eq!(
            should_consult(&owns, t2, &p2),
            Consult::No { skipped: "rule_fired" }
        );
        // Identical rule below threshold: restraint is policy.
        let short = vec![
            unit("id", "iid", "const ", " = next();"),
            unit("id", "iid", "let ", " = 0;"),
        ];
        let t3 = "const id = parse(row);\n";
        let p3 = predict(&short, t3, 0);
        assert_eq!(p3.reason_silent, Some("below_threshold"));
        assert_eq!(should_consult(&short, t3, &p3), Consult::No { skipped: "gate" });
        // Fewer than two real units.
        let one = vec![unit("a", "b", "x", "y")];
        let p4 = predict(&one, "a\n", 0);
        assert_eq!(should_consult(&one, "a\n", &p4), Consult::No { skipped: "gate" });
    }

    #[test]
    fn region_anchors_on_needle_and_clamps() {
        let text: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let needle_text = text.replace("line 30", "dial(a, b)");
        let (s, e, hit) = select_region(&needle_text, 0, Some("dial("));
        assert!(hit);
        assert!(needle_text[s..e].contains("dial(a, b)"));
        assert_eq!(needle_text[s..e].lines().count(), REGION_LINES);
        // No needle: cursor-centered; small doc: whole doc.
        let (s2, e2, hit2) = select_region("a\nb\nc\n", 2, None);
        assert!(!hit2);
        assert_eq!((s2, e2), (0, 6));
        // Needle behind the cursor is still found (wrap).
        let (_, _, hit3) = select_region(&needle_text, needle_text.len(), Some("dial("));
        assert!(hit3);
    }

    #[test]
    fn parse_rewrite_normalizes_and_drops() {
        let region = "a\nb\nc\n";
        assert_eq!(parse_rewrite("a\nB\nc\n", region).unwrap(), "a\nB\nc\n");
        // Wrapping fence + language tag.
        assert_eq!(parse_rewrite("```rust\na\nB\nc\n```", region).unwrap(), "a\nB\nc\n");
        // Echoed markers stripped.
        let echoed = format!("{REGION_START_MARKER}\na\nB\nc\n{REGION_END_MARKER}\n");
        assert_eq!(parse_rewrite(&echoed, region).unwrap(), "a\nB\nc\n");
        // Think-block preamble stripped.
        assert_eq!(
            parse_rewrite("<think>hmm</think>\na\nB\nc\n", region).unwrap(),
            "a\nB\nc\n"
        );
        // Unchanged output is an explained noop, not an edit.
        assert_eq!(parse_rewrite("a\nb\nc\n", region), Err("noop"));
        // Runaway growth dropped.
        let runaway = "x\n".repeat(REGION_LINES + MAX_LINE_DELTA + 2);
        assert_eq!(parse_rewrite(&runaway, region), Err("invalid"));
        assert_eq!(parse_rewrite("", region), Err("invalid"));
    }

    #[test]
    fn diff_region_yields_per_site_hunks() {
        // Two separated line changes → two edits, char-trimmed.
        let orig = "aa\nbb\ncc\ndd\nee\n";
        let new = "aa\nbX\ncc\ndY\nee\n";
        let edits = diff_region(orig, new);
        assert_eq!(edits.len(), 2);
        assert_eq!(&orig[edits[0].start..edits[0].end], "b");
        assert_eq!(edits[0].new_text, "X");
        assert_eq!(&orig[edits[1].start..edits[1].end], "d");
        assert_eq!(edits[1].new_text, "Y");
        // Pure line insertion → zero-width edit at the insertion point.
        let ins = diff_region("aa\ncc\n", "aa\nbb\ncc\n");
        assert_eq!(ins.len(), 1);
        assert_eq!(ins[0].start, ins[0].end);
        assert_eq!(ins[0].new_text, "bb\n");
        // Identical inputs → no edits.
        assert!(diff_region("aa\n", "aa\n").is_empty());
    }
}
