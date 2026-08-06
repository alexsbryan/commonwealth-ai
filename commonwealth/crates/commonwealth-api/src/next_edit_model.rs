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
/// Byte ceiling on that same region. Lines bound it for ordinary
/// source; bytes bound it for files that are mostly one line (a
/// minified bundle is a single 512 KiB line, and `text` is capped at
/// 512 KiB, so without this the "24-line" region is the whole file
/// and the prompt is ~1 MiB of prefill on the shared slot).
pub const MAX_REGION_BYTES: usize = 8 * 1024;
/// Per-side context fed to the needle's longest-common-substring
/// search. The needle is a short anchor (`MIN_NEEDLE` is 3), so the
/// quadratic DP never needs the full 2 KiB-per-field the wire allows.
const MAX_NEEDLE_CTX: usize = 192;
/// Two per-site-varying replacements must share this many prefix chars.
const MIN_PARAM_PREFIX: usize = 4;
/// A shorter needle is noise, not an anchor.
const MIN_NEEDLE: usize = 3;
/// Most-recent real units rendered into the prompt.
const MAX_PROMPT_UNITS: usize = 4;
/// A rewrite growing the region beyond this is a runaway, not an edit.
const MAX_GROWTH_BYTES: usize = 2048;
/// …and so is one that shrinks it by more than this. The growth cap
/// alone is one-sided: a truncated or lazy completion is *smaller*
/// than the region, and without this a single accepted hunk could
/// delete almost the whole region (measured 2026-07-30: 480 KiB
/// deleted from a 24-line region whose line count was unchanged).
const MAX_SHRINK_BYTES: usize = 2048;
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

/// Longest common substring (chars). Callers MUST bound the inputs:
/// this is an O(n·m) DP on a request-path thread with no `.await` in
/// it, and the wire allows 2 KiB per unit field.
fn lcsubstr(a: &str, b: &str) -> String {
    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    let mut best_len = 0usize;
    let mut best_end = 0usize;
    // Two rolling rows, reused — not one allocation per row.
    let mut prev = vec![0usize; bc.len() + 1];
    let mut cur = vec![0usize; bc.len() + 1];
    for i in 1..=ac.len() {
        cur.fill(0);
        for j in 1..=bc.len() {
            if ac[i - 1] == bc[j - 1] {
                cur[j] = prev[j - 1] + 1;
                if cur[j] > best_len {
                    best_len = cur[j];
                    best_end = i;
                }
            }
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    ac[best_end - best_len..best_end].iter().collect()
}

/// Last `n` chars of `s` (char-boundary safe).
fn tail(s: &str, n: usize) -> &str {
    match s.char_indices().nth_back(n.saturating_sub(1)) {
        Some((i, _)) if s.chars().count() > n => &s[i..],
        _ => s,
    }
}

/// First `n` chars of `s` (char-boundary safe).
fn head(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// The needle anchors a region; it is drawn from the text immediately
/// around each edit, so only the nearest [`MAX_NEEDLE_CTX`] chars per
/// side can contribute. Bounding here keeps [`lcsubstr`] off the
/// wire's full 6 KiB-per-unit worst case.
fn ctx_needle(a: &HistoryUnit, b: &HistoryUnit) -> Option<String> {
    let near = |u: &HistoryUnit| {
        format!(
            "{}{}{}",
            tail(&u.left, MAX_NEEDLE_CTX),
            head(&u.before, MAX_NEEDLE_CTX),
            head(&u.right, MAX_NEEDLE_CTX)
        )
    };
    let s = lcsubstr(&near(a), &near(b)).trim().to_string();
    (s.chars().count() >= MIN_NEEDLE).then_some(s)
}

// ---- the consult gate -------------------------------------------------

/// The two most recent content-bearing units in the history window —
/// the exemplars every consult shape is defined over, `(most recent,
/// second most recent)`. Shared by [`should_consult`] (which decides
/// FROM them) and [`verify_pattern`] (which holds the model's output
/// TO them), so the two stages can never disagree about which edits
/// form the pattern.
pub fn exemplar_pair(history: &[HistoryUnit]) -> Option<(&HistoryUnit, &HistoryUnit)> {
    let window_start = history.len().saturating_sub(HISTORY_WINDOW);
    let cores: Vec<&HistoryUnit> =
        history[window_start..].iter().filter(|u| u.before != u.after).collect();
    if cores.len() < 2 {
        return None;
    }
    Some((cores[cores.len() - 1], cores[cores.len() - 2]))
}

/// Decide whether the model lane may be consulted. `p` is the rule
/// lane's outcome on the same request; a fired rule lane always wins.
pub fn should_consult(history: &[HistoryUnit], text: &str, p: &Prediction) -> Consult {
    if !p.edits.is_empty() {
        return Consult::No { skipped: "rule_fired" };
    }
    let Some((a, b)) = exemplar_pair(history) else {
        return Consult::No { skipped: "gate" };
    };
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
    if end - start <= MAX_REGION_BYTES {
        return (start, end, hit);
    }
    // Over the byte budget: rebuild the window by growing outward from
    // the target line while both budgets hold. Ordinary source never
    // reaches this branch (24 lines is far under 8 KiB), so region
    // selection is unchanged for every real file. When even the target
    // line alone exceeds the budget the region stays that one line and
    // the route declines it (`region_too_large`) rather than prefilling
    // a megabyte nobody can read.
    let line_end = |l: usize| starts.get(l + 1).copied().unwrap_or(text.len());
    let (mut lo, mut hi) = (target_line, target_line);
    let mut used = line_end(target_line) - starts[target_line];
    loop {
        let mut grew = false;
        if lo > 0 && hi - lo + 1 < REGION_LINES {
            let add = starts[lo] - starts[lo - 1];
            if used + add <= MAX_REGION_BYTES {
                lo -= 1;
                used += add;
                grew = true;
            }
        }
        if hi + 1 < line_count && hi - lo + 1 < REGION_LINES {
            let add = line_end(hi + 1) - starts[hi + 1];
            if used + add <= MAX_REGION_BYTES {
                hi += 1;
                used += add;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    (starts[lo], line_end(hi), hit)
}

// ---- prompt -----------------------------------------------------------

pub const REGION_START_MARKER: &str = "<|editable_region_start|>";
pub const REGION_END_MARKER: &str = "<|editable_region_end|>";

// ---- bakeoff formats: zeta2 / sweep raw prompts -----------------------
//
// Completion-style edit models speak their fine-tune's own contract,
// not ours. Each format below reproduces the model's published prompt
// shape exactly (Zeta 2.1 model card; Sweep's run_model.py) and rides
// the FIM slot's raw path (`FimCompletionRequest.raw_prompt`,
// `FimMode::Verbatim`). Format selection is explicit config
// (`[models.fim].next_edit_format`) — see NEXT_EDIT.md.

// Zeta 2.x brackets its editable region with GIT-MERGE markers, not
// with `<|marker_N|>` sentinels. Corrected 2026-08-05 against the
// canonical `sample.prompt` / `sample.output` in `zed-industries/zeta-2`
// itself; the previous constants were written from a model-card
// description and had never been run against the weights. The bakeoff's
// first zeta-2 arm scored 0/30 with 19 `invalid` + 11 `truncated` — a
// 100% parse failure, which is what an unexercised dialect looks like.
/// Zeta 2.x editable-region open marker.
pub const ZETA_MARKER_1: &str = "<<<<<<< CURRENT";
/// Zeta 2.x separator: the prompt ends here and the model writes the
/// UPDATED side after `<[fim-middle]>`.
pub const ZETA_MARKER_2: &str = "=======";
/// Zeta 2.x terminator the model emits after the rewritten region. Also
/// the stop string — without it a completion runs to the token ceiling
/// and lands as `truncated`.
pub const ZETA_UPDATED_END: &str = ">>>>>>> UPDATED";
/// Zeta 2.x cursor position marker, inserted inside the region.
pub const ZETA_CURSOR: &str = "<|user_cursor|>";
const ZETA_FIM_PREFIX: &str = "<[fim-prefix]>";
const ZETA_FIM_SUFFIX: &str = "<[fim-suffix]>";
const ZETA_FIM_MIDDLE: &str = "<[fim-middle]>";
/// Sweep's section separator (a Qwen2.5-Coder special token).
pub const SWEEP_FILE_SEP: &str = "<|file_sep|>";
/// File-context windows around the region in raw prompts, in bytes.
/// Same role as the FIM slot's prefix/suffix clamps: bound the
/// prefill an adversarial file can buy.
const RAW_PREFIX_WINDOW: usize = 4096;
const RAW_SUFFIX_WINDOW: usize = 2048;

/// Keep the TAIL of `s` beyond `max` bytes (char-boundary safe).
fn tail_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[s.ceil_char_boundary(s.len() - max)..]
    }
}

/// Keep the HEAD of `s` up to `max` bytes (char-boundary safe).
fn head_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..s.floor_char_boundary(max)]
    }
}

/// The edit-history units every prompt format shows: the same window
/// the consult gate judged (`should_consult`) — showing a model units
/// the gate never looked at would let it generalize from a pattern
/// nothing authorized — trimmed to the newest [`MAX_PROMPT_UNITS`]
/// real edits.
fn shown_units(history: &[HistoryUnit]) -> Vec<&HistoryUnit> {
    let window = &history[history.len().saturating_sub(HISTORY_WINDOW)..];
    let cores: Vec<&HistoryUnit> = window.iter().filter(|u| u.before != u.after).collect();
    cores[cores.len().saturating_sub(MAX_PROMPT_UNITS)..].to_vec()
}

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
    let shown = shown_units(history);
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

/// Zeta 2.x raw prompt (model card, SPM ordering): suffix section,
/// then prefix section carrying the edit history as unified-diff
/// snippets and the target file's code before the region, then the
/// marker-bracketed editable region with the cursor marker, then the
/// generation marker. `rs..re` is the region's byte range in `text`;
/// `cursor` a byte offset (marker inserted only when it falls inside
/// the region — a needle-anchored region away from the cursor has no
/// honest cursor position to mark).
pub fn build_prompt_zeta2(
    history: &[HistoryUnit],
    text: &str,
    rs: usize,
    re: usize,
    cursor: usize,
    path: Option<&str>,
) -> String {
    let region = &text[rs..re];
    let prefix_win = tail_bytes(&text[..rs], RAW_PREFIX_WINDOW);
    let suffix_win = head_bytes(&text[re..], RAW_SUFFIX_WINDOW);
    let path = path.unwrap_or("untitled");
    let mut p =
        String::with_capacity(region.len() * 2 + prefix_win.len() + suffix_win.len() + 512);
    p.push_str(ZETA_FIM_SUFFIX);
    p.push('\n');
    p.push_str(suffix_win);
    if !suffix_win.is_empty() && !suffix_win.ends_with('\n') {
        p.push('\n');
    }
    p.push_str(ZETA_FIM_PREFIX);
    p.push_str("<filename>edit_history\n");
    for u in shown_units(history) {
        p.push_str(&format!("--- a/{path}\n+++ b/{path}\n"));
        let old = format!("{}{}{}", u.left, u.before, u.right);
        let new = format!("{}{}{}", u.left, u.after, u.right);
        for l in old.lines() {
            p.push('-');
            p.push_str(l);
            p.push('\n');
        }
        for l in new.lines() {
            p.push('+');
            p.push_str(l);
            p.push('\n');
        }
    }
    p.push_str(&format!("\n<filename>{path}\n"));
    p.push_str(prefix_win);
    if !prefix_win.is_empty() && !prefix_win.ends_with('\n') {
        p.push('\n');
    }
    p.push_str(ZETA_MARKER_1);
    p.push('\n');
    if cursor >= rs && cursor <= re {
        p.push_str(&region[..cursor - rs]);
        p.push_str(ZETA_CURSOR);
        p.push_str(&region[cursor - rs..]);
    } else {
        p.push_str(region);
    }
    if !region.ends_with('\n') {
        p.push('\n');
    }
    p.push_str(ZETA_MARKER_2);
    p.push('\n');
    p.push_str(ZETA_FIM_MIDDLE);
    p
}

/// Sweep raw prompt (run_model.py): `<|file_sep|>` sections — one
/// `.diff` original/updated block per history unit, then the region
/// before the most recent edit (`original/`), the region as it stands
/// (`current/`), and the `updated/` header the model completes.
/// Sections joined by `\n`, matching the reference builder exactly.
pub fn build_prompt_sweep(history: &[HistoryUnit], region: &str, path: Option<&str>) -> String {
    let path = path.unwrap_or("untitled");
    let shown = shown_units(history);
    let mut parts: Vec<String> = Vec::new();
    for u in &shown {
        parts.push(format!("{SWEEP_FILE_SEP}{path}.diff"));
        parts.push("original:".to_string());
        parts.push(format!("{}{}{}", u.left, u.before, u.right));
        parts.push("updated:".to_string());
        parts.push(format!("{}{}{}", u.left, u.after, u.right));
    }
    // Sweep's training format encodes momentum as original→current:
    // `current/` includes the most recent edit, `original/` predates
    // it. Reconstruct that when the edit's site lies inside the
    // region; when it doesn't (region away from the edit), the two
    // sections are honestly identical.
    let original = shown
        .last()
        .and_then(|u| unapply_in_region(region, u))
        .unwrap_or_else(|| region.to_string());
    parts.push(format!("{SWEEP_FILE_SEP}original/{path}"));
    parts.push(original);
    parts.push(format!("{SWEEP_FILE_SEP}current/{path}"));
    parts.push(region.to_string());
    parts.push(format!("{SWEEP_FILE_SEP}updated/{path}"));
    parts.join("\n")
}

/// Reverse one history unit inside the region: find its post-edit
/// text (`left+after+right`) and restore the pre-edit text at that
/// one site. `None` when the site isn't in the region (or the unit
/// carries no change), so the caller falls back to the region itself.
fn unapply_in_region(region: &str, u: &HistoryUnit) -> Option<String> {
    let cur = format!("{}{}{}", u.left, u.after, u.right);
    let old = format!("{}{}{}", u.left, u.before, u.right);
    if cur.is_empty() || cur == old {
        return None;
    }
    let i = region.find(&cur)?;
    let mut out = String::with_capacity(region.len() + old.len());
    out.push_str(&region[..i]);
    out.push_str(&old);
    out.push_str(&region[i + cur.len()..]);
    Some(out)
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
    // One wrapping code fence, with or without a language tag — and
    // nothing else. Prose outside the fence, or a second fenced block
    // with commentary between, is a chat reply rather than a region;
    // repairing it would splice the commentary into the user's file.
    // Closing on the FIRST fence, not the last, is what stops that.
    let t = s.trim();
    if t.starts_with("```") {
        let inner = t.trim_start_matches("```");
        let body = inner.split_once('\n').map(|(_, b)| b).unwrap_or("");
        let Some(i) = body.find("```") else {
            return Err("invalid");
        };
        if !body[i + 3..].trim().is_empty() {
            return Err("invalid");
        }
        s = body[..i].to_string();
    } else if t.contains("```") {
        return Err("invalid");
    }
    // Echoed markers are malformed, whole stop. An earlier version
    // stripped marker-only lines and rebuilt the output from
    // `lines()`, which silently deleted any real file line that
    // happened to BE a marker and rewrote every line ending on the
    // way through — repairing suspicious output into a confident
    // wrong edit, which is exactly the trade this lane refuses.
    if s.contains("editable_region") {
        return Err("invalid");
    }
    validate_rewrite(s, region)
}

/// Format-agnostic rewrite validation — the guards every prompt
/// format shares, applied after the format-specific unwrap. The
/// region is the authority for line endings and trailing newline;
/// growth/shrink/line-delta bounds are the same bars the §6 bank
/// gates were pre-registered against, so a format adapter can never
/// quietly relax them.
fn validate_rewrite(mut s: String, region: &str) -> Result<String, &'static str> {
    // Line endings: a CRLF region against an LF rewrite makes every
    // line differ, so the line-LCS collapses to one hunk spanning the
    // whole region — the model gets free rein over code it never
    // meant to touch, and a faithful echo stops registering as a noop.
    if region.contains("\r\n") {
        s = s.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    // Trailing newline, both directions: the region is the authority.
    if region.ends_with('\n') && !s.ends_with('\n') {
        s.push('\n');
    } else if !region.ends_with('\n') && s.ends_with('\n') {
        s.truncate(s.trim_end_matches('\n').trim_end_matches('\r').len());
    }
    if s.trim().is_empty() {
        return Err("invalid");
    }
    if s.len() > region.len() + MAX_GROWTH_BYTES {
        return Err("invalid");
    }
    // Shrink is bounded two ways because one bound is not enough: the
    // absolute cap catches a big region truncated mid-rewrite, and the
    // proportional one catches a small region gutted in place (20
    // lines of code replaced by 20 lines of `//` loses far less than
    // 2 KiB, and its line delta is zero). Continuing an edit pattern
    // never halves the region.
    if s.len() + MAX_SHRINK_BYTES < region.len() || s.len() * 2 < region.len() {
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

/// Parse a Zeta 2.x completion. The prompt ends at `=======` +
/// `<[fim-middle]>`, so the model resumes with the UPDATED side
/// **bare** — no opening marker — and terminates it with
/// `>>>>>>> UPDATED`.
///
/// The terminator is not *required* here, because it is also the stop
/// string: llama.cpp consumes a matched stop rather than returning it,
/// so demanding it would fail every well-formed completion. An
/// unterminated run-on is already refused upstream — `finish` drops
/// `finish_reason == "length"` before parsing — so absence here means
/// the decode ended cleanly on stop or EOS.
pub fn parse_rewrite_zeta2(raw: &str, region: &str) -> Result<String, &'static str> {
    let body = match raw.find(ZETA_UPDATED_END) {
        Some(end) => {
            // Past the terminator, anything but whitespace is prose.
            if !raw[end + ZETA_UPDATED_END.len()..].trim().is_empty() {
                return Err("invalid");
            }
            &raw[..end]
        }
        None => raw,
    };
    let mut content = body.strip_suffix('\n').unwrap_or(body).to_string();
    if let Some(i) = content.find(ZETA_CURSOR) {
        content.replace_range(i..i + ZETA_CURSOR.len(), "");
    }
    // A second cursor, a re-emitted region marker, or a leaked FIM
    // sentinel all mean the model is writing protocol rather than code.
    if content.contains(ZETA_CURSOR)
        || content.contains(ZETA_MARKER_1)
        || content.contains(ZETA_UPDATED_END)
        || content.contains("<[fim-")
    {
        return Err("invalid");
    }
    validate_rewrite(content, region)
}


/// Parse a Sweep completion: the rewritten window, terminated by
/// `<|file_sep|>` or `</s>` when the stop tracker didn't already
/// consume them. The completion begins on the line after the
/// `updated/{path}` header, so exactly one leading newline is part of
/// the format, not the rewrite.
pub fn parse_rewrite_sweep(raw: &str, region: &str) -> Result<String, &'static str> {
    let mut s = raw;
    if let Some(i) = s.find(SWEEP_FILE_SEP) {
        s = &s[..i];
    }
    if let Some(i) = s.find("</s>") {
        s = &s[..i];
    }
    let s = s.strip_prefix('\n').unwrap_or(s);
    validate_rewrite(s.to_string(), region)
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

// ---- V0 content verifier ----------------------------------------------
//
// The structural guards bound HOW MUCH the model may change; nothing
// above bounds WHAT the change says. But the gate only consults when
// the exemplars agree on a transformation, and for the identical-core
// shapes that transformation fixes the correct content: remove the
// exemplars' `before`, add their `after` (for `param_insert`, the
// shared prefix of the two `after`s — the tails vary per site and are
// deliberately not judged here). Every check below is a predicate the
// gate's own shape definitions imply, so the verifier generalizes
// exactly as far as the gate does — it knows nothing any bank case
// taught it.

/// What the exemplar transformation adds, in a countable form: the
/// content with every whitespace run collapsed to one space. Both
/// the pattern and the text are normalized the same way before
/// substring counting, so neither indentation drift at a new site
/// nor the line-fragment shape of an exemplar (an `after` like
/// `",\n    \"retries\": 3"` STARTS mid-line — its leading comma
/// belongs to the previous line in situ, so no line-wise matcher can
/// ever see it) can hide content the pattern names.
struct Pat {
    norm: String,
}

fn norm_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn pat_of(s: &str) -> Option<Pat> {
    let norm = norm_ws(s);
    (!norm.is_empty()).then_some(Pat { norm })
}

fn count_pat(hay: &str, p: &Pat) -> usize {
    norm_ws(hay).match_indices(p.norm.as_str()).count()
}

/// Two occurrences with nothing but whitespace between them are one
/// site doubled, never two sites — the exemplars came from distinct
/// contexts by the gate's definition. In normalized space that means
/// consecutive matches separated by at most one space.
fn adjacent_dup(hay: &str, p: &Pat) -> bool {
    let h = norm_ws(hay);
    let pos: Vec<usize> = h.match_indices(p.norm.as_str()).map(|(i, _)| i).collect();
    pos.windows(2).any(|w| h[w[0] + p.norm.len()..w[1]].trim().is_empty())
}

/// Full-line span of a hunk within the region (`pad` adds one line of
/// context each side). The hunk offsets are char-trimmed, so the
/// evidence of a re-application lives in the surrounding line, not in
/// the hunk bytes themselves.
fn line_span(region: &str, start: usize, end: usize, pad: bool) -> (usize, usize) {
    let mut s = region[..start].rfind('\n').map_or(0, |i| i + 1);
    let mut e = region[end..].find('\n').map_or(region.len(), |i| end + i + 1);
    if pad {
        if s > 0 {
            s = region[..s - 1].rfind('\n').map_or(0, |i| i + 1);
        }
        if e < region.len() {
            e = region[e..].find('\n').map_or(region.len(), |i| e + i + 1);
        }
    }
    (s, e)
}

/// Hold the model's hunks to the transformation the exemplars agree
/// on. `Err` is the wire-visible `dropped` reason plus the index of
/// the offending hunk (glassbox: the route echoes that hunk back in
/// `sovereign_debug` so a drop is diagnosable from the response
/// alone): `inconsistent` (a hunk does not advance the pattern, or
/// moves against it) or `already_applied` (a hunk re-applies it where
/// it already holds, or stacks it at one site). Any bad hunk drops
/// the whole prediction — no suggestion beats a wrong one.
pub fn verify_pattern(
    reason: &str,
    a: &HistoryUnit,
    b: &HistoryUnit,
    region: &str,
    edits: &[RegionEdit],
) -> Result<(), (&'static str, usize)> {
    let (add, remove) = match reason {
        "param_insert" => {
            let n = common_prefix_len(&a.after, &b.after);
            let prefix: String = a.after.chars().take(n).collect();
            (pat_of(&prefix), pat_of(&a.before))
        }
        _ => (pat_of(&a.after), pat_of(&a.before)),
    };
    // Only identical-content shapes can prove a re-application: a
    // `param_insert` neighborhood may legitimately already carry the
    // shared prefix (an earlier site's differing tail).
    let identical_content = reason != "param_insert";

    let mut content_hunks = 0usize;
    for (i, e) in edits.iter().enumerate() {
        // A hunk that touches no content is outside the pattern's
        // jurisdiction: the shape predicates constrain WHAT the edit
        // says, and a whitespace-only reflow (e.g. a completion
        // format's trailing-newline artifact) says nothing. A hunk
        // that DELETES content into whitespace is still judged.
        if e.new_text.trim().is_empty() && region[e.start..e.end].trim().is_empty() {
            continue;
        }
        content_hunks += 1;
        let (os, oe) = line_span(region, e.start, e.end, false);
        let old_own = &region[os..oe];
        let new_own = format!("{}{}{}", &region[os..e.start], e.new_text, &region[e.end..oe]);
        if let Some(p) = &add {
            let oo = count_pat(old_own, p);
            let no = count_pat(&new_own, p);
            if no <= oo {
                return Err(("inconsistent", i));
            }
            if identical_content && oo >= 1 {
                return Err(("already_applied", i));
            }
            let (ps, pe) = line_span(region, e.start, e.end, true);
            let padded_old = &region[ps..pe];
            let padded_new =
                format!("{}{}{}", &region[ps..e.start], e.new_text, &region[e.end..pe]);
            if adjacent_dup(&padded_new, p) && !adjacent_dup(padded_old, p) {
                return Err(("already_applied", i));
            }
        }
        if let Some(r) = &remove {
            let or = count_pat(old_own, r);
            let nr = count_pat(&new_own, r);
            if nr > or {
                return Err(("inconsistent", i));
            }
            if add.is_none() && nr >= or {
                return Err(("inconsistent", i));
            }
        }
    }
    if content_hunks == 0 {
        // Every hunk was whitespace: a formatted echo, not an edit.
        // The gate consulted for a content pattern and the model
        // proposed no content — that is the completion-trap answer
        // ("nothing left to do"), and it must land as silence, not as
        // a whitespace edit the user is asked to accept.
        return Err(("noop", 0));
    }
    Ok(())
}

// ---- the lane as two pure halves, split at the inference call --------
//
// Everything the model lane decides lives in `plan` (before inference)
// and `finish` (after it). Inference itself is the ONLY impure step, so
// the daemon route and the offline scorer
// (`examples/next_edit_score.rs`) differ in exactly that one place and
// share every decision. This split exists so a candidate model can be
// scored without a daemon and the score still be the daemon's answer:
// a second implementation of this ordering would be two rulers on one
// contract, which is the defect class the §9a hardening pass already
// caught once (NEXT_EDIT.md §9a, "Two rulers on one contract").

/// What the model expects on the wire. `Chat` is one user turn through
/// the chat template; `Raw` is the model's own verbatim prompt, which
/// completion-style edit models need — a chat template would wrap their
/// special tokens in a user turn and the fine-tune would never see the
/// shape it was trained on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    Chat(String),
    Raw(String),
}

/// A consult the pre-inference half approved, with everything the
/// caller needs to issue it and nothing about how.
#[derive(Debug, Clone)]
pub struct ConsultPlan {
    pub reason: &'static str,
    pub needle: Option<String>,
    /// Byte offsets of the rewrite region in the request text.
    pub region_start: usize,
    pub region_end: usize,
    pub needle_hit: bool,
    pub format: String,
    pub prompt: Prompt,
    pub max_tokens: u32,
    pub stop: Vec<String>,
    /// Sampling temperature. Lives here rather than at the call site
    /// because it is lane policy, not transport: the completion-style
    /// formats are scored greedily (their fine-tunes are trained for a
    /// single right answer) while the instruct format keeps a sliver of
    /// temperature. An offline scorer that guessed this would be
    /// measuring a different model than the daemon runs.
    pub temperature: f32,
}

/// Outcome of the pre-inference half — glassbox in all three arms.
#[derive(Debug, Clone)]
pub enum Plan {
    /// The consult gate refused; no model was involved (`skipped`).
    Skip { skipped: &'static str },
    /// The gate said yes, then a region guard declined (`dropped`).
    Decline {
        reason: &'static str,
        needle: Option<String>,
        dropped: &'static str,
        /// Present only for `region_too_large`, which is the one
        /// decline whose magnitude the operator needs to see.
        region_bytes: Option<usize>,
    },
    Send(Box<ConsultPlan>),
}

/// Why a consulted prediction produced no edits. `hunk` is the
/// offending region edit when the V0 content verifier rejected one, so
/// the caller can show what it refused without re-deriving it.
#[derive(Debug, Clone)]
pub struct FinishDrop {
    pub dropped: &'static str,
    pub hunk: Option<RegionEdit>,
}

/// Everything the model lane decides BEFORE inference: consult gate,
/// region selection, the three region guards, and prompt shaping.
///
/// `format` is the wire dialect the slot speaks — it comes from the
/// inference service in the daemon and from a flag in the scorer, so it
/// is a parameter rather than a lookup. Callers that have no model
/// available should not call this; they report `unavailable` against
/// the gate's own answer instead (see `Plan::Skip` vs. the caller's
/// drop).
pub fn plan(
    history: &[HistoryUnit],
    text: &str,
    cursor: usize,
    p: &Prediction,
    path: Option<&str>,
    language: Option<&str>,
    format: &str,
    force: bool,
) -> Plan {
    // `force` is a MEASUREMENT affordance and the daemon always passes
    // `false`. It exists because every model number this project has is
    // conditioned on the consult gate: the gate admits ~9% of real
    // editing episodes (`gym/next-edit/golden/README.md`), so a model
    // scored through it has been judged on a sliver our routing chose,
    // not on what it can do. Forcing the consult measures the model's
    // ceiling independent of our gate, which is the only way to tell a
    // gate that protects us from a bad model apart from one that hides
    // a good one. The region guards below still apply — those bound
    // cost and correctness, not eligibility.
    let (reason, needle) = match should_consult(history, text, p) {
        Consult::No { skipped } if !force => return Plan::Skip { skipped },
        // Forced: no exemplar shape was recognised, so there is no
        // per-shape reason and no needle. Region selection falls back
        // to the cursor line, which is what an unanchored consult gets.
        Consult::No { .. } => ("forced", None),
        Consult::Yes { reason, needle } => (reason, needle),
    };
    let decline = |dropped, region_bytes| Plan::Decline {
        reason,
        needle: needle.clone(),
        dropped,
        region_bytes,
    };

    let (rs, re, needle_hit) = select_region(text, cursor, needle.as_deref());
    let region = &text[rs..re];
    // A region that blew the byte budget means a single line did (a
    // minified bundle is one 512 KiB line). Prefilling that on the
    // shared slot is a large, repeatable cost for a suggestion nobody
    // can read, so decline and say so.
    if region.len() > MAX_REGION_BYTES {
        return decline("region_too_large", Some(region.len()));
    }
    // An empty or blank region has nothing to rewrite, so every byte
    // the model returns is invention with no relationship to the file
    // — and the guards that normally bound a rewrite are all relative
    // to the region, so they bound nothing here.
    if region.trim().is_empty() {
        return decline("region_empty", None);
    }
    // A region already containing the active format's markers would
    // make the prompt ambiguous about where the editable span ends, and
    // every faithful echo would then fail parsing — the lane would look
    // silently broken on that one file forever. Which strings poison
    // the prompt depends on the format the slot speaks.
    let poisoned = match format {
        // `=======` also appears as a Markdown/RST setext underline and
        // inside a real merge conflict. Declining those is the correct
        // trade: an ambiguous region boundary corrupts a file, and
        // silence costs one suggestion.
        "zeta2" => {
            region.contains(ZETA_MARKER_1)
                || region.contains(ZETA_MARKER_2)
                || region.contains(ZETA_UPDATED_END)
                || region.contains("<[fim-")
                || region.contains(ZETA_CURSOR)
        }
        "sweep" => region.contains(SWEEP_FILE_SEP),
        _ => region.contains("editable_region"),
    };
    if poisoned {
        return decline("region_has_markers", None);
    }

    let max_tokens = ((region.len() / 3) + 160).clamp(64, 1024) as u32;
    // `</s>` is Sweep's documented terminator; zeta2's `<|marker_2|>`
    // is already a SeedCoder family stop.
    let (prompt, stop, temperature) = match format {
        "zeta2" => (
            Prompt::Raw(build_prompt_zeta2(history, text, rs, re, cursor, path)),
            vec![ZETA_UPDATED_END.to_string()],
            0.0,
        ),
        "sweep" => (
            Prompt::Raw(build_prompt_sweep(history, region, path)),
            vec!["</s>".to_string()],
            0.0,
        ),
        _ => (
            Prompt::Chat(build_prompt(history, region, path, language, reason)),
            Vec::new(),
            0.1,
        ),
    };

    Plan::Send(Box::new(ConsultPlan {
        reason,
        needle,
        region_start: rs,
        region_end: re,
        needle_hit,
        format: format.to_string(),
        prompt,
        max_tokens,
        stop,
        temperature,
    }))
}

/// Everything the model lane decides AFTER inference: finish-reason
/// screening, parsing, region diffing, and the V0 content verifier.
/// Returns REGION-RELATIVE edits; the caller rebases by
/// `plan.region_start`.
pub fn finish(
    plan: &ConsultPlan,
    history: &[HistoryUnit],
    region: &str,
    content: &str,
    finish_reason: Option<&str>,
) -> Result<Vec<RegionEdit>, FinishDrop> {
    let drop = |dropped| FinishDrop { dropped, hunk: None };

    // A completion that hit the token ceiling is a region cut off
    // mid-rewrite. Diffed against the whole region it reads as "delete
    // everything after here" — the tail is missing, not unchanged.
    if finish_reason == Some("length") {
        return Err(drop("truncated"));
    }
    // Cancelled/errored decodes carry partial content; a partial
    // rewrite is the same mass-deletion hazard.
    if matches!(finish_reason, Some("cancelled") | Some("error")) {
        return Err(drop("error"));
    }

    let rewritten = match plan.format.as_str() {
        "zeta2" => parse_rewrite_zeta2(content, region),
        "sweep" => parse_rewrite_sweep(content, region),
        _ => parse_rewrite(content, region),
    }
    .map_err(drop)?;

    let region_edits = diff_region(region, &rewritten);
    if region_edits.is_empty() {
        return Err(drop("noop"));
    }
    // V0 content verifier: the structural guards above bound how much
    // changed; this holds WHAT changed to the exemplar transformation
    // the gate consulted over. The pair cannot be absent here — the
    // gate already required it — but a defensive miss just skips
    // verification rather than inventing a drop.
    if let Some((a, b)) = exemplar_pair(history) {
        if let Err((dropped, idx)) = verify_pattern(plan.reason, a, b, region, &region_edits) {
            return Err(FinishDrop { dropped, hunk: Some(region_edits[idx].clone()) });
        }
    }
    Ok(region_edits)
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
    fn region_respects_the_byte_budget_on_long_lines() {
        // Ordinary source is untouched by the budget: still 24 lines.
        let ordinary: String = (0..60).map(|i| format!("let x{i} = {i};\n")).collect();
        let (s, e, _) = select_region(&ordinary, 0, None);
        assert_eq!(ordinary[s..e].lines().count(), REGION_LINES);

        // Long lines: fewer lines, but always within the budget.
        let long: String = (0..60).map(|i| format!("{}// {i}\n", "x".repeat(900))).collect();
        let (s, e, _) = select_region(&long, long.len() / 2, None);
        assert!(e - s <= MAX_REGION_BYTES, "region {} bytes", e - s);
        assert!(e > s, "still returns a usable window");
        assert!(long.is_char_boundary(s) && long.is_char_boundary(e));

        // The pathological file — one 200 KiB line, no newline at all.
        // The window cannot shrink below one line, so it comes back
        // over budget and the ROUTE declines it (`region_too_large`);
        // silently prefilling it is the failure this guards.
        let minified = "a".repeat(200 * 1024);
        let (s, e, _) = select_region(&minified, 0, None);
        assert!(e - s > MAX_REGION_BYTES, "route must see it as too large");
    }

    #[test]
    fn needle_context_is_bounded_but_still_anchors() {
        // 2 KiB per field is legal on the wire; the needle search must
        // not be handed all of it (quadratic DP on a request thread).
        let pad = "z".repeat(2000);
        let a = unit("", ", tmo", &format!("{pad}dial(a, x"), ")");
        let b = unit("", ", tmo", &format!("{pad}dial(b, y"), ")");
        let n = ctx_needle(&a, &b).expect("still finds an anchor");
        assert!(n.chars().count() >= MIN_NEEDLE);
        assert!(n.len() <= MAX_NEEDLE_CTX * 3);

        // Truncation is char-boundary safe on multi-byte context.
        let emoji = "💡".repeat(300);
        let c = unit("", "!", &emoji, &emoji);
        assert!(ctx_needle(&c, &c.clone()).is_some());
        assert_eq!(head("héllo", 2), "hé");
        assert_eq!(tail("héllo", 2), "lo");
        assert_eq!(head("hi", 9), "hi");
        assert_eq!(tail("hi", 9), "hi");
    }

    #[test]
    fn parse_rewrite_normalizes_and_drops() {
        let region = "a\nb\nc\n";
        assert_eq!(parse_rewrite("a\nB\nc\n", region).unwrap(), "a\nB\nc\n");
        // Wrapping fence + language tag.
        assert_eq!(parse_rewrite("```rust\na\nB\nc\n```", region).unwrap(), "a\nB\nc\n");
        // Echoed markers are dropped, not repaired — see
        // `echoed_markers_are_dropped_never_repaired` for why.
        let echoed = format!("{REGION_START_MARKER}\na\nB\nc\n{REGION_END_MARKER}\n");
        assert_eq!(parse_rewrite(&echoed, region), Err("invalid"));
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

    /// Every one of these was a way to turn a suspicious completion
    /// into a confident wrong edit. The posture is drop-whole: no
    /// suggestion beats a wrong one.
    #[test]
    fn parse_rewrite_refuses_chat_shaped_output() {
        let region = "a\nb\nc\n";
        // Prose before the fence — the fence check used to only look
        // at position 0, so this sailed through unwrapped and spliced
        // the prose into the file.
        assert_eq!(parse_rewrite("Sure! Here you go:\n```\na\nB\nc\n```", region), Err("invalid"));
        // Two fenced blocks with commentary between: closing on the
        // LAST fence used to swallow the commentary as file content.
        assert_eq!(
            parse_rewrite("```\na\nB\n```\nI also removed the dead code.\n```\nc\n```", region),
            Err("invalid")
        );
        // Trailing commentary after a well-formed fence.
        assert_eq!(parse_rewrite("```\na\nB\nc\n```\nHope that helps!", region), Err("invalid"));
        // A well-formed single fence still parses.
        assert_eq!(parse_rewrite("```rust\na\nB\nc\n```", region).unwrap(), "a\nB\nc\n");
    }

    /// A file line that IS a marker used to be deleted by the
    /// marker-stripping repair — a wrong edit manufactured out of a
    /// faithful echo.
    #[test]
    fn echoed_markers_are_dropped_never_repaired() {
        let region = format!("let x = 1;\n{REGION_END_MARKER}\n");
        assert_eq!(parse_rewrite(&region, &region), Err("invalid"));
        let echoed = format!("{REGION_START_MARKER}\na\nB\nc\n{REGION_END_MARKER}\n");
        assert_eq!(parse_rewrite(&echoed, "a\nb\nc\n"), Err("invalid"));
    }

    /// CRLF region + LF rewrite made every line differ, collapsing the
    /// line-LCS into one hunk over the whole region: the model got
    /// free rein over code it never claimed to touch, and a byte-
    /// faithful echo stopped registering as a noop.
    #[test]
    fn crlf_regions_survive_an_lf_rewrite() {
        let region = "one\r\ntwo\r\nthree\r\n";
        let out = parse_rewrite("one\r\nTWO\r\nthree\r\n", region).unwrap();
        assert_eq!(diff_region(region, &out).len(), 1, "one hunk, not a whole-region flip");

        let from_lf = parse_rewrite("one\ntwo\nthree\n", region);
        assert_eq!(from_lf, Err("noop"), "a faithful echo in LF is still a noop");
        let changed = parse_rewrite("one\nTWO\nthree\n", region).unwrap();
        assert!(changed.contains("\r\n") && !changed.contains("\n\n"));
        let edits = diff_region(region, &changed);
        assert_eq!(edits.len(), 1);
        assert_eq!(&region[edits[0].start..edits[0].end], "two");
    }

    #[test]
    fn parse_rewrite_bounds_shrink_as_well_as_growth() {
        let region: String = (0..20).map(|i| format!("line {i} with some real content\n")).collect();
        // A truncated or lazy completion deletes the rest of the
        // region; the growth cap alone never saw this.
        assert_eq!(parse_rewrite("line 0 with some real content\n", &region), Err("invalid"));
        // A same-line-count gutting: line delta 0, bytes gone.
        let gutted: String = (0..20).map(|_| "//\n".to_string()).collect();
        assert_eq!(parse_rewrite(&gutted, &region), Err("invalid"));
        // Trailing newline is honoured in both directions.
        assert_eq!(parse_rewrite("a\nB\nc\n", "a\nb\nc").unwrap(), "a\nB\nc");
        assert_eq!(parse_rewrite("a\nB\nc", "a\nb\nc\n").unwrap(), "a\nB\nc\n");
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

    // ---- bakeoff formats: zeta2 -------------------------------------

    #[test]
    fn zeta2_prompt_is_spm_with_cursor_and_history() {
        let h = vec![
            unit("get", "fetch", "a.", "(1);"),
            unit("get", "fetch", "b.", "(2);"),
        ];
        let text = "before\nAAA\nBBB\nafter\n";
        // region = bytes 7..15 ("AAA\nBBB\n"), cursor inside at 8.
        let p = build_prompt_zeta2(&h, text, 7, 15, 8, Some("t.rs"));
        let at = |m: &str| p.find(m).unwrap();
        assert!(at(ZETA_FIM_SUFFIX) < at(ZETA_FIM_PREFIX));
        assert!(at(ZETA_FIM_PREFIX) < at("<filename>edit_history"));
        assert!(at("<filename>edit_history") < at("<filename>t.rs"));
        assert!(p.ends_with(ZETA_FIM_MIDDLE));
        assert!(p.contains("-a.get(1);\n"));
        assert!(p.contains("+a.fetch(1);\n"));
        let (m1, m2, cur) = (at(ZETA_MARKER_1), at(ZETA_MARKER_2), at(ZETA_CURSOR));
        assert!(m1 < cur && cur < m2);
    }

    #[test]
    fn zeta2_prompt_omits_cursor_outside_region() {
        let h = vec![unit("x", "y", "", "")];
        let text = "before\nAAA\nBBB\nafter\n";
        let p = build_prompt_zeta2(&h, text, 7, 15, 2, Some("t.rs"));
        assert!(!p.contains(ZETA_CURSOR));
        assert!(p.contains(ZETA_MARKER_1) && p.contains(ZETA_MARKER_2));
    }

    #[test]
    fn zeta2_parse_happy_path_and_stop_eaten_terminator() {
        let region = "AAA\nBBB\n";
        // The model resumes after `=======` and writes the UPDATED side
        // bare, terminated by `>>>>>>> UPDATED`.
        assert_eq!(
            parse_rewrite_zeta2("AAA\nCCC\n>>>>>>> UPDATED", region).unwrap(),
            "AAA\nCCC\n"
        );
        // llama.cpp consumes a matched stop string rather than returning
        // it, so a terminator-less body is the COMMON production case,
        // not an edge one. An unterminated run-on is refused upstream by
        // the `finish_reason == "length"` guard, not here.
        assert_eq!(parse_rewrite_zeta2("AAA\nCCC\n", region).unwrap(), "AAA\nCCC\n");
    }

    #[test]
    fn zeta2_parse_rejects_trailing_prose_and_leaked_protocol() {
        let region = "AAA\nBBB\n";
        assert_eq!(
            parse_rewrite_zeta2(
                "AAA\nCCC\n>>>>>>> UPDATED\nI also removed the dead code",
                region
            ),
            Err("invalid")
        );
        // Re-emitting a region marker or a FIM sentinel means the model
        // is writing protocol, not code.
        assert_eq!(
            parse_rewrite_zeta2(&format!("AAA\nCCC\n{ZETA_MARKER_1}\n"), region),
            Err("invalid")
        );
        assert_eq!(
            parse_rewrite_zeta2("AAA\n<[fim-suffix]>\nCCC\n", region),
            Err("invalid")
        );
    }

    #[test]
    fn zeta2_parse_unwraps_one_cursor_echo_rejects_two() {
        let region = "AAA\nBBB\n";
        let one = format!("A{ZETA_CURSOR}AA\nBB2\n{ZETA_UPDATED_END}");
        assert_eq!(parse_rewrite_zeta2(&one, region).unwrap(), "AAA\nBB2\n");
        let two = format!("A{ZETA_CURSOR}A{ZETA_CURSOR}A\nBB2\n{ZETA_UPDATED_END}");
        assert_eq!(parse_rewrite_zeta2(&two, region), Err("invalid"));
    }

    #[test]
    fn zeta2_parse_rides_shared_guards() {
        let region = "AAA\nBBB\n";
        assert_eq!(
            parse_rewrite_zeta2(&format!("AAA\nBBB\n{ZETA_UPDATED_END}"), region),
            Err("noop")
        );
        let bomb = format!("{}\n{ZETA_UPDATED_END}", "X".repeat(9000));
        assert_eq!(parse_rewrite_zeta2(&bomb, region), Err("invalid"));
    }

    /// The dialect this lane speaks must match the weights it is aimed
    /// at. Pinned against the canonical `sample.prompt` in
    /// `zed-industries/zeta-2` (fetched 2026-08-05), because the
    /// previous constants were written from a prose model-card
    /// description and produced a 100% parse failure against the real
    /// model — 0/30 on the bakeoff's first zeta-2 arm.
    #[test]
    fn zeta2_region_markers_match_the_published_sample_prompt() {
        assert_eq!(ZETA_MARKER_1, "<<<<<<< CURRENT");
        assert_eq!(ZETA_MARKER_2, "=======");
        assert_eq!(ZETA_UPDATED_END, ">>>>>>> UPDATED");
        let h = vec![unit("get", "fetch", "a.", "(1);")];
        let text = "before\nAAA\nBBB\nafter\n";
        let p = build_prompt_zeta2(&h, text, 7, 15, 8, Some("t.rs"));
        // The prompt hands over mid-conflict: CURRENT block, separator,
        // then the FIM middle sentinel and nothing else.
        assert!(p.ends_with(&format!("{ZETA_MARKER_2}\n{ZETA_FIM_MIDDLE}")));
        assert!(!p.contains(ZETA_UPDATED_END));
    }

    // ---- bakeoff formats: sweep -------------------------------------

    #[test]
    fn sweep_prompt_sections_in_order_with_unapplied_original() {
        let h = vec![
            unit("get", "fetch", "a.", "(1);"),
            unit("get", "fetch", "b.", "(2);"),
        ];
        let region = "a.fetch(1);\nb.fetch(2);\nc.get(3);\n";
        let p = build_prompt_sweep(&h, region, Some("t.rs"));
        let at = |m: &str| p.find(m).unwrap();
        let (d, o, c, u) = (
            at("<|file_sep|>t.rs.diff"),
            at("<|file_sep|>original/t.rs"),
            at("<|file_sep|>current/t.rs"),
            at("<|file_sep|>updated/t.rs"),
        );
        assert!(d < o && o < c && c < u);
        assert!(p.ends_with("<|file_sep|>updated/t.rs"));
        assert!(p.contains("original:\na.get(1);\nupdated:\na.fetch(1);"));
        // original/ section shows the LAST unit un-applied at its site,
        // while the earlier fan-out site keeps its current state.
        let orig_section = &p[o..c];
        assert!(orig_section.contains("b.get(2);"));
        assert!(orig_section.contains("a.fetch(1);"));
    }

    #[test]
    fn sweep_original_falls_back_when_edit_site_outside_region() {
        let h = vec![unit("get", "fetch", "z.", "(9);")];
        let region = "a.other(1);\n";
        let p = build_prompt_sweep(&h, region, None);
        let at = |m: &str| p.find(m).unwrap();
        let (o, c, u) = (
            at("<|file_sep|>original/untitled"),
            at("<|file_sep|>current/untitled"),
            at("<|file_sep|>updated/untitled"),
        );
        assert_eq!(
            p[o..c].replace("original/", ""),
            p[c..u].replace("current/", "")
        );
    }

    #[test]
    fn sweep_parse_cuts_terminators_and_strips_format_newline() {
        let region = "AAA\nBBB\n";
        assert_eq!(
            parse_rewrite_sweep("\nAAA\nCCC\n<|file_sep|>next/t.rs", region).unwrap(),
            "AAA\nCCC\n"
        );
        assert_eq!(parse_rewrite_sweep("\nAAA\nCCC\n</s>", region).unwrap(), "AAA\nCCC\n");
        assert_eq!(parse_rewrite_sweep("\nAAA\nBBB\n", region), Err("noop"));
        assert_eq!(parse_rewrite_sweep("", region), Err("invalid"));
    }

    // ---- V0 content verifier ------------------------------------------
    //
    // Every fixture here is derived from a gate shape's definition,
    // not from any eval-bank case: the checks must hold for arbitrary
    // instances of the shape or the verifier is overfit.

    fn fanout_exemplars() -> (HistoryUnit, HistoryUnit) {
        (
            unit("", ", timeoutMS", "\tconn := dial(primaryHost, 8080", ")"),
            unit("", ", timeoutMS", "\tbackup := dial(backupHost, altPort", ")"),
        )
    }

    #[test]
    fn verify_fresh_site_passes_even_beside_a_done_neighbor() {
        // The done site sits on the ADJACENT line: the padded span sees
        // it, and the verifier must still recognize a fresh application
        // — dropping this would eat every correct fire in dense code.
        let (a, b) = fanout_exemplars();
        let region = "\tconn := dial(primaryHost, 8080, timeoutMS)\n\
                      \tmirror := dial(mirrorHost, 9090)\n";
        let at = region.find("9090)").unwrap() + 4;
        let edits = vec![RegionEdit { start: at, end: at, new_text: ", timeoutMS".into() }];
        assert_eq!(verify_pattern("fanout_insert", &a, &b, region, &edits), Ok(()));
    }

    #[test]
    fn verify_reapplying_at_a_done_site_drops() {
        // The hunk's own line already carries the insertion; adding it
        // again is the completion-trap failure, whatever the file.
        let (a, b) = fanout_exemplars();
        let region = "\tconn := dial(primaryHost, 8080, timeoutMS)\n";
        let at = region.find(")\n").unwrap();
        let edits = vec![RegionEdit { start: at, end: at, new_text: ", timeoutMS".into() }];
        assert_eq!(
            verify_pattern("fanout_insert", &a, &b, region, &edits),
            Err(("already_applied", 0))
        );
    }

    #[test]
    fn verify_stacking_in_one_hunk_drops() {
        // Two copies with only whitespace between are one site doubled
        // — the exemplars came from distinct contexts by definition.
        let (a, b) = fanout_exemplars();
        let region = "\tmirror := dial(mirrorHost, 9090)\n";
        let at = region.find(')').unwrap();
        let edits =
            vec![RegionEdit { start: at, end: at, new_text: ", timeoutMS, timeoutMS".into() }];
        assert_eq!(
            verify_pattern("fanout_insert", &a, &b, region, &edits),
            Err(("already_applied", 0))
        );
    }

    #[test]
    fn verify_hunk_that_ignores_the_pattern_drops() {
        // A rewrite that renames instead of applying the exemplar
        // transformation is not what the gate consulted for.
        let (a, b) = fanout_exemplars();
        let region = "\tmirror := dial(mirrorHost, 9090)\n";
        let s = region.find("dial").unwrap();
        let edits = vec![RegionEdit { start: s, end: s + 4, new_text: "connect".into() }];
        assert_eq!(
            verify_pattern("fanout_insert", &a, &b, region, &edits),
            Err(("inconsistent", 0))
        );
    }

    #[test]
    fn verify_multiline_insertion_fresh_vs_done() {
        let a = unit("", "\n        retries: 3,", "        max: 10,", "\n    }");
        let b = unit("", "\n        retries: 3,", "        cap: 4,", "\n    }");
        // Fresh literal: insertion passes despite indentation drift.
        let fresh = "    cfg := Config{\n        max: 10,\n    }\n";
        let at = fresh.find("\n    }").unwrap();
        let ins = vec![RegionEdit {
            start: at,
            end: at,
            new_text: "\n        retries: 3,".into(),
        }];
        assert_eq!(verify_pattern("multiline_fanout", &a, &b, fresh, &ins), Ok(()));
        // Done literal: the identical trimmed line is right above the
        // insertion point — stacked, drop.
        let done = "    cfg := Config{\n        retries: 3,\n    }\n";
        let at = done.find("\n    }").unwrap();
        let ins = vec![RegionEdit {
            start: at,
            end: at,
            new_text: "\n        retries: 3,".into(),
        }];
        assert_eq!(
            verify_pattern("multiline_fanout", &a, &b, done, &ins),
            Err(("already_applied", 0))
        );
    }

    #[test]
    fn verify_whitespace_only_hunk_is_exempt_content_deletion_is_not() {
        // A trailing-newline artifact beside a correct application
        // must not sink the prediction (completion formats append
        // one; observed live with Sweep 2026-07-31)…
        let (a, b) = fanout_exemplars();
        let region = "\tconn := dial(primaryHost, 8080, timeoutMS)\n\
                      \tmirror := dial(mirrorHost, 9090)\n";
        let at = region.find("9090)").unwrap() + 4;
        let end = region.len();
        let edits = vec![
            RegionEdit { start: at, end: at, new_text: ", timeoutMS".into() },
            RegionEdit { start: end, end, new_text: "\n".into() },
        ];
        assert_eq!(verify_pattern("fanout_insert", &a, &b, region, &edits), Ok(()));
        // …but a hunk that deletes content into whitespace is judged,
        // and fails: it does not advance the pattern.
        let s = region.find("mirror").unwrap();
        let gut = vec![RegionEdit {
            start: s,
            end: s + "mirror := dial(mirrorHost, 9090)".len(),
            new_text: " ".into(),
        }];
        assert_eq!(
            verify_pattern("fanout_insert", &a, &b, region, &gut),
            Err(("inconsistent", 0))
        );
        // …and a prediction that is ONLY whitespace is a formatted
        // echo — the completion-trap answer — and must land as noop,
        // never as an accepted-able edit (observed live: Sweep answers
        // exhausted fan-outs with a bare trailing newline).
        let echo = vec![RegionEdit { start: end, end, new_text: "\n".into() }];
        assert_eq!(
            verify_pattern("fanout_insert", &a, &b, region, &echo),
            Err(("noop", 0))
        );
    }

    #[test]
    fn verify_line_fragment_exemplar_matches_in_situ() {
        // JSON-ish insertion whose exemplar STARTS mid-line: the
        // leading comma attaches to the previous line in situ, so a
        // line-wise matcher can never see the pattern — regression
        // for exactly that false `inconsistent` (control run
        // 2026-07-31). Whitespace-normalized matching must both PASS
        // the fresh site and still CATCH the done site.
        let a = unit("", ",\n    \"retries\": 3", "    \"timeout\": 30", "\n}");
        let b = unit("", ",\n    \"retries\": 3", "    \"cap\": 4", "\n}");
        let fresh = "{\n    \"timeout\": 30\n}\n";
        let at = fresh.find("30").unwrap() + 2;
        let ins = vec![RegionEdit {
            start: at,
            end: at,
            new_text: ",\n    \"retries\": 3".into(),
        }];
        assert_eq!(verify_pattern("multiline_fanout", &a, &b, fresh, &ins), Ok(()));
        let done = "{\n    \"timeout\": 30,\n    \"retries\": 3\n}\n";
        let at = done.find('3').unwrap();
        let at = done[at..].find("3\n").unwrap() + at + 1;
        let ins = vec![RegionEdit {
            start: at,
            end: at,
            new_text: ",\n    \"retries\": 3".into(),
        }];
        assert_eq!(
            verify_pattern("multiline_fanout", &a, &b, done, &ins),
            Err(("already_applied", 0))
        );
    }

    #[test]
    fn verify_deletion_fanout_must_delete() {
        let a = unit(", legacyFlag", "", "\tstart(alpha", ")");
        let b = unit(", legacyFlag", "", "\tstart(beta", ")");
        let region = "\tstart(gamma, legacyFlag)\n";
        let s = region.find(", legacyFlag").unwrap();
        // Deleting the flag is the pattern.
        let del = vec![RegionEdit { start: s, end: s + ", legacyFlag".len(), new_text: "".into() }];
        assert_eq!(verify_pattern("fanout_insert", &a, &b, region, &del), Ok(()));
        // Rewriting the line while KEEPING the flag is not.
        let keep = vec![RegionEdit {
            start: s,
            end: s + ", legacyFlag".len(),
            new_text: ", legacyFlag /* soon */".into(),
        }];
        assert_eq!(
            verify_pattern("fanout_insert", &a, &b, region, &keep),
            Err(("inconsistent", 0))
        );
    }

    #[test]
    fn verify_param_insert_allows_varying_tails() {
        // Tails vary per site by definition; a neighboring earlier site
        // carrying the shared prefix must not read as re-application.
        let a = unit(".unwrap()", ".expect(\"cfg missing\")", "    let c = load(p)", ";");
        let b = unit(".unwrap()", ".expect(\"env missing\")", "    let e = read()", ";");
        let region = "    let c = load(p).expect(\"cfg missing\");\n    let t = parse(s).unwrap();\n";
        let s = region.rfind(".unwrap()").unwrap();
        let edits = vec![RegionEdit {
            start: s,
            end: s + ".unwrap()".len(),
            new_text: ".expect(\"tz missing\")".into(),
        }];
        assert_eq!(verify_pattern("param_insert", &a, &b, region, &edits), Ok(()));
        // But a hunk that never brings the shared prefix is not the
        // pattern.
        let off = vec![RegionEdit {
            start: s,
            end: s + ".unwrap()".len(),
            new_text: ".ok()".into(),
        }];
        assert_eq!(
            verify_pattern("param_insert", &a, &b, region, &off),
            Err(("inconsistent", 0))
        );
    }
}
