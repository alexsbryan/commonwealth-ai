// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fill-in-the-middle (FIM) inline-completion support — the prompt
//! builder, the marker-style vocab probe, and the stream stop
//! tracker (`sovereign/docs/INLINE_COMPLETION.md`).
//!
//! FIM for the coder families we ship is expressed as a plain-text
//! prompt using the model's own special-token markers, tokenized with
//! special-token parsing on and no chat-template wrapping
//! (`PromptShape::Raw`). This module owns:
//!
//! - the **marker table** ([`FimMarkers`]) — one row per supported
//!   family; a second family is a table addition, not a rewrite;
//! - [`build_fim_prompt`] — PSM-ordered assembly
//!   (`{prefix-marker}{prefix}{suffix-marker}{suffix}{middle-marker}`),
//!   which is both Qwen's documented shape and prefix-cache friendly
//!   (the prefix section only appends as the user types);
//! - [`detect_fim_style`] — vocab probe run once at slot install.
//!   `ModelFamily` is `Unknown` on all production slots, so family-
//!   keyed detection won't work; instead every marker must tokenize
//!   to EXACTLY ONE token in the loaded model's vocab. No match →
//!   `None` → the daemon refuses the slot with an actionable message;
//! - [`FimStopTracker`] — the pure stream filter that decides when a
//!   completion is done (INLINE_COMPLETION.md §3.3). F0 implements
//!   the stop-string scan with a holdback buffer (a stop string split
//!   across token boundaries must never leak into the suggestion);
//!   the single/multi-line mode decision, depth tracking, and suffix
//!   dedupe land in F1.

use sovereign_core::types::FimStyle;

use crate::llama::cpp::model::{AddBos, LlamaModel};

/// One row of the FIM marker table: the special tokens a coder
/// family's tokenizer must carry, plus the strings that terminate a
/// completion when the model emits them.
pub struct FimMarkers {
    /// Style this row describes.
    pub style: FimStyle,
    /// Marker preceding the prefix (code before the cursor).
    pub prefix: &'static str,
    /// Marker preceding the suffix (code after the cursor).
    pub suffix: &'static str,
    /// Marker at the generation point (model infills after it).
    pub middle: &'static str,
    /// Additional tokens that must ALSO be atomic in the vocab for
    /// this row to match. Needed because Mellum and StarCoder2 share
    /// the `<fim_prefix>`/`<fim_suffix>`/`<fim_middle>` spellings —
    /// Mellum rows require `<|im_start|>`, StarCoder2 requires
    /// `<|end_of_text|>`, so the probe stays deterministic.
    pub also_requires: &'static [&'static str],
    /// Strings that end the completion when seen in the output. The
    /// family's EOG token is included defensively — the decode loop
    /// normally stops on it first, but raw prompts skip the template
    /// machinery that usually guarantees EOG recognition, so the
    /// tracker treats it as an ordinary stop string too.
    pub stop_strings: &'static [&'static str],
}

/// The marker table. Ordered by preference — the probe returns the
/// FIRST row whose markers all tokenize cleanly, so more specific /
/// more common families come first.
pub const FIM_MARKER_TABLE: &[FimMarkers] = &[
    FimMarkers {
        style: FimStyle::QwenCoder,
        prefix: "<|fim_prefix|>",
        suffix: "<|fim_suffix|>",
        middle: "<|fim_middle|>",
        also_requires: &[],
        stop_strings: &["<|endoftext|>", "<|fim_pad|>", "<|file_sep|>", "<|repo_name|>"],
    },
    // Mellum BEFORE StarCoder2: identical marker spellings, so the
    // discriminator tokens do the disambiguation (`<|im_start|>` on
    // Mellum2's chat-trained vocab vs `<|end_of_text|>` on
    // StarCoder2's). Verified against Mellum2-12B-A2.5B (Thinking AND
    // Instruct, 2026-07-21): vocab carries <fim_prefix>/<fim_suffix>/
    // <fim_middle>/<fim_pad>/<|im_start|>/<|im_end|>/<|endoftext|>.
    FimMarkers {
        style: FimStyle::Mellum,
        prefix: "<fim_prefix>",
        suffix: "<fim_suffix>",
        middle: "<fim_middle>",
        also_requires: &["<|im_start|>"],
        stop_strings: &["<|endoftext|>", "<|im_end|>", "<fim_pad>"],
    },
    FimMarkers {
        style: FimStyle::StarCoder2,
        prefix: "<fim_prefix>",
        suffix: "<fim_suffix>",
        middle: "<fim_middle>",
        also_requires: &["<|end_of_text|>"],
        stop_strings: &["<|end_of_text|>", "<file_sep>", "<fim_pad>"],
    },
];

/// Look up the marker row for a detected style.
pub fn markers_for(style: FimStyle) -> &'static FimMarkers {
    FIM_MARKER_TABLE
        .iter()
        .find(|m| m.style == style)
        .expect("every FimStyle has a marker-table row")
}

/// Assemble the raw FIM prompt in PSM ordering:
/// `{prefix-marker}{prefix}{suffix-marker}{suffix}{middle-marker}`.
/// The model generates the infill immediately after the middle
/// marker. Fed to the tokenizer via `PromptShape::Raw` (verbatim,
/// special-token parsing on, no BOS).
pub fn build_fim_prompt(style: FimStyle, prefix: &str, suffix: &str) -> String {
    let m = markers_for(style);
    let mut out = String::with_capacity(
        m.prefix.len() + prefix.len() + m.suffix.len() + suffix.len() + m.middle.len(),
    );
    out.push_str(m.prefix);
    out.push_str(prefix);
    out.push_str(m.suffix);
    out.push_str(suffix);
    out.push_str(m.middle);
    out
}

/// Probe the loaded model's vocab for a FIM marker set. Every marker
/// (prefix/suffix/middle) must tokenize to EXACTLY ONE token — a
/// marker that splits into pieces means the model was never trained
/// with it as an atomic unit and FIM prompting would degrade into
/// garbage. Returns the first matching table row, `None` when no
/// family's markers are all atomic (caller refuses the slot).
pub fn detect_fim_style(model: &LlamaModel) -> Option<FimStyle> {
    'rows: for row in FIM_MARKER_TABLE {
        for marker in row
            .also_requires
            .iter()
            .chain([row.prefix, row.suffix, row.middle].iter())
        {
            match model.str_to_token(marker, AddBos::Never) {
                Ok(tokens) if tokens.len() == 1 => {}
                _ => continue 'rows,
            }
        }
        return Some(row.style);
    }
    None
}

/// Which rule stopped the completion — glassbox payload for
/// `sovereign_debug.stop_rule` (INLINE_COMPLETION.md §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopRule {
    /// A family stop string (`<|endoftext|>`, `<|fim_pad|>`, …) was
    /// seen in the output.
    StopString,
    /// Single-line mode: a newline ended the completion.
    Newline,
    /// Multi-line mode: brace/paren depth returned to the opener's
    /// level — the block is complete.
    DepthClose,
    /// Multi-line mode: a blank line ended the completion.
    BlankLine,
    /// Multi-line mode: the max-lines budget was exhausted.
    MaxLines,
    /// The caller's suffix began repeating at the end of the
    /// completion — trimmed to the overlap point.
    SuffixDuplication,
}

impl StopRule {
    /// Stable wire string for the debug payload.
    pub const fn as_str(self) -> &'static str {
        match self {
            StopRule::StopString => "stop_string",
            StopRule::Newline => "newline",
            StopRule::DepthClose => "depth_close",
            StopRule::BlankLine => "blank_line",
            StopRule::MaxLines => "max_lines",
            StopRule::SuffixDuplication => "suffix_duplication",
        }
    }
}

/// Single-line vs multi-line completion decision (§3.3). Made
/// here, not by the model: the text immediately before the cursor
/// tells us which shape the completion should take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FimMode {
    /// Complete the current line only — stop at the first newline.
    Single,
    /// Complete a block body — stop on net-depth close, a blank
    /// line, or the max-lines budget.
    Multi,
}

impl FimMode {
    /// Stable wire string for the debug payload.
    pub const fn as_str(self) -> &'static str {
        match self {
            FimMode::Single => "single",
            FimMode::Multi => "multi",
        }
    }
}

/// Decide the mode from the text before the cursor (the clamped
/// prefix's tail). Multi ONLY when the cursor sits at an obvious
/// block opening — trailing `{` / `(` / `[` / `:` / `=>`. Everything
/// else is Single: mid-line continuations are the common case, and a
/// false Multi lets the model ramble a whole block where the user
/// wanted an expression finished.
pub fn decide_mode(prefix_tail: &str) -> FimMode {
    let t = prefix_tail.trim_end();
    if t.ends_with('{')
        || t.ends_with('(')
        || t.ends_with('[')
        || t.ends_with(':')
        || t.ends_with("=>")
    {
        FimMode::Multi
    } else {
        FimMode::Single
    }
}

/// Terminal outcome reported by [`FimStopTracker`] — the rule that
/// fired plus how many already-fed chars were trimmed (they were
/// held back, never emitted, so "trimmed" here means "withheld").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopOutcome {
    /// The rule that fired.
    pub rule: StopRule,
    /// Chars withheld from emission at the stop point (e.g. the
    /// matched stop string itself, or the duplicated suffix span).
    pub trimmed: usize,
}

/// What [`FimStopTracker::feed`] decided for one token's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feed {
    /// Safe text to append to the suggestion (may be empty when the
    /// whole token is still held back).
    Emit(String),
    /// Stop now. `Emit`-equivalent text (anything before the stop
    /// point) is included; the adapter appends it, synthesizes
    /// `Finish{Stop}`, and drops the inner stream.
    Stop {
        /// Final safe text before the stop point.
        text: String,
        /// Which rule fired + withheld-char count.
        outcome: StopOutcome,
    },
}

/// Pure stream filter applying the FIM stop conditions
/// (INLINE_COMPLETION.md §3.3). Constructed per request; one `feed`
/// call per decoded token's text.
///
/// Rules (checked positionally; the EARLIEST stop wins):
///
/// 1. **Stop strings** (family markers + client-supplied) — scanned
///    over the whole pending buffer; a holdback tail of
///    `longest-stop − 1` bytes is never released unconditionally, so
///    a stop string split across token boundaries cannot leak.
/// 2. **Suffix duplication** — if the first ≤40 chars of the
///    caller's suffix (whitespace-trimmed, ≥3 chars) appears in the
///    output, the model started regenerating code that already
///    exists after the cursor: stop at the overlap point.
/// 3. **Single-line mode** — stop at the first newline.
/// 4. **Multi-line mode** — stop when bracket depth (`{}()[]`) goes
///    net-negative (the model closed the construct containing the
///    cursor — depth-0 stops are deliberately NOT used: nested
///    opens/closes inside the body would fire prematurely), on a
///    blank line, or when `max_lines` newlines have been emitted.
pub struct FimStopTracker {
    stop_strings: Vec<String>,
    /// Text received but not yet released (holdback tail).
    pending: String,
    /// Longest stop string length minus one — the number of bytes we
    /// must hold back so a split stop string can't slip through.
    holdback: usize,
    mode: FimMode,
    /// Head of the caller's suffix used for duplication detection
    /// (empty = disabled — suffix too short/trivial to probe safely).
    suffix_probe: String,
    /// Net bracket depth over all scanned output (`({[` − `)}]`).
    depth: i64,
    /// Newlines seen over all scanned output.
    lines: usize,
    /// Bytes of `pending` already structurally accounted (depth /
    /// lines). Advances per feed; blank-line candidates hold it back
    /// until the next token disambiguates.
    scanned: usize,
    /// Multi-line newline budget (§3.3).
    max_lines: usize,
    /// Total chars emitted (debug payload).
    emitted_chars: usize,
}

/// Multi-line newline budget (§3.3: bodies longer than ~8 lines are
/// almost always the model rambling past the useful completion).
pub const FIM_DEFAULT_MAX_LINES: usize = 8;

/// Max chars of the caller's suffix probed for duplication (§3.3).
pub const FIM_SUFFIX_PROBE_CHARS: usize = 40;

impl FimStopTracker {
    /// New tracker for a marker family (single-line mode, no suffix
    /// probe — the historical minimal behaviour).
    pub fn new(style: FimStyle) -> Self {
        Self::with_stop_strings(markers_for(style).stop_strings)
    }

    /// Tracker with an explicit stop-string set.
    pub fn with_stop_strings(stop_strings: &[&'static str]) -> Self {
        Self::build(
            stop_strings.iter().map(|s| s.to_string()).collect(),
            FimMode::Single,
            "",
        )
    }

    /// Full-craft tracker: family stops ∪ client extras, mode from
    /// [`decide_mode`], suffix probe from the caller's suffix.
    pub fn new_with_extra(
        style: FimStyle,
        extra: Vec<String>,
        mode: FimMode,
        suffix: &str,
    ) -> Self {
        let m = markers_for(style);
        let mut owned: Vec<String> = m.stop_strings.iter().map(|s| s.to_string()).collect();
        owned.extend(extra.into_iter().filter(|s| !s.is_empty()));
        Self::build(owned, mode, suffix)
    }

    fn build(stop_strings: Vec<String>, mode: FimMode, suffix: &str) -> Self {
        let holdback = stop_strings
            .iter()
            .map(|s| s.len().saturating_sub(1))
            .max()
            .unwrap_or(0);
        // Duplication probe: the first FIM_SUFFIX_PROBE_CHARS of the
        // suffix, leading-whitespace trimmed. Require ≥3 chars —
        // shorter probes ("}") false-positive on ordinary output.
        let probe_raw: String = suffix.chars().take(FIM_SUFFIX_PROBE_CHARS).collect();
        let probe = probe_raw.trim_start();
        let suffix_probe = if probe.chars().count() >= 3 {
            probe.to_string()
        } else {
            String::new()
        };
        Self {
            stop_strings,
            pending: String::new(),
            holdback,
            mode,
            suffix_probe,
            depth: 0,
            lines: 0,
            scanned: 0,
            max_lines: FIM_DEFAULT_MAX_LINES,
            emitted_chars: 0,
        }
    }

    /// Feed one token's text. See [`Feed`] for the contract.
    pub fn feed(&mut self, text: &str) -> Feed {
        self.pending.push_str(text);

        // Candidate stops: (byte position, rule, emit-through length).
        // The earliest position wins; ties prefer the structural rule
        // over the string rule (same text either way).
        let mut best: Option<(usize, StopRule, usize)> = None;
        let mut consider = |pos: usize, rule: StopRule, emit: usize| {
            let better = match best {
                Some((cur, _, _)) => pos < cur,
                None => true,
            };
            if better {
                best = Some((pos, rule, emit));
            }
        };

        // Rule 1: stop strings (earliest across the set).
        for s in &self.stop_strings {
            if let Some(idx) = self.pending.find(s.as_str()) {
                consider(idx, StopRule::StopString, idx);
            }
        }

        // Rule 2: suffix duplication.
        if !self.suffix_probe.is_empty() {
            if let Some(idx) = self.pending.find(self.suffix_probe.as_str()) {
                consider(idx, StopRule::SuffixDuplication, idx);
            }
        }

        // Rules 3/4: structural walk over newly-arrived bytes.
        if let Some((pos, rule, emit)) = self.structural_scan() {
            consider(pos, rule, emit);
        }

        if let Some((_, rule, emit)) = best {
            let safe = self.pending[..emit].to_string();
            let trimmed = self.pending.len() - emit;
            self.pending.clear();
            self.emitted_chars += safe.chars().count();
            return Feed::Stop {
                text: safe,
                outcome: StopOutcome { rule, trimmed },
            };
        }

        // No stop: release everything except the holdback tail.
        let release_to = self.pending.len().saturating_sub(self.holdback);
        let release_to = self.pending.floor_char_boundary(release_to);
        let safe: String = self.pending.drain(..release_to).collect();
        self.scanned = self.scanned.saturating_sub(release_to);
        self.emitted_chars += safe.chars().count();
        Feed::Emit(safe)
    }

    /// Structural stop rules over `pending[self.scanned..]`. Returns
    /// `(byte position, rule, emit-through length)` on a hit, else
    /// advances `scanned` and returns `None`. Byte-level matching is
    /// safe: the interesting characters are ASCII, which never
    /// appears inside a multi-byte UTF-8 sequence.
    fn structural_scan(&mut self) -> Option<(usize, StopRule, usize)> {
        let bytes = self.pending.as_bytes();
        let mut i = self.scanned;
        let mut stop: Option<(usize, StopRule, usize)> = None;
        while i < bytes.len() {
            match bytes[i] {
                b'{' | b'(' | b'[' => self.depth += 1,
                b'}' | b')' | b']' => {
                    self.depth -= 1;
                    if self.mode == FimMode::Multi && self.depth < 0 {
                        // Closed the construct containing the cursor.
                        // Emit THROUGH the closer — the user's block
                        // needs it — then stop.
                        stop = Some((i + 1, StopRule::DepthClose, i + 1));
                        break;
                    }
                }
                b'\n' => {
                    if self.mode == FimMode::Single {
                        // Emit up to (not including) the newline.
                        stop = Some((i, StopRule::Newline, i));
                        break;
                    }
                    self.lines += 1;
                    if self.lines >= self.max_lines {
                        stop = Some((i + 1, StopRule::MaxLines, i + 1));
                        break;
                    }
                    // Blank line: this newline, optional horizontal
                    // whitespace, then a second newline.
                    let mut j = i + 1;
                    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                        j += 1;
                    }
                    if j < bytes.len() && bytes[j] == b'\n' {
                        // Emit through the first newline; the blank
                        // line itself is the stop signal.
                        stop = Some((i + 1, StopRule::BlankLine, i + 1));
                        break;
                    }
                    if j >= bytes.len() {
                        // Can't decide until the next token arrives —
                        // hold `scanned` here so the check re-runs.
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        self.scanned = i;
        stop
    }

    /// Flush at end-of-stream (the model hit EOS/EOG or max_tokens
    /// without any stop rule firing). Returns any held-back text —
    /// safe by definition: the stream is over, so no split stop
    /// string can still complete.
    pub fn flush(&mut self) -> String {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_prompt_psm_order_qwen() {
        let p = build_fim_prompt(FimStyle::QwenCoder, "fn main() {", "}");
        assert_eq!(p, "<|fim_prefix|>fn main() {<|fim_suffix|>}<|fim_middle|>");
    }

    #[test]
    fn build_prompt_psm_order_starcoder() {
        let p = build_fim_prompt(FimStyle::StarCoder2, "a", "b");
        assert_eq!(p, "<fim_prefix>a<fim_suffix>b<fim_middle>");
    }

    #[test]
    fn build_prompt_psm_order_mellum() {
        let p = build_fim_prompt(FimStyle::Mellum, "a", "b");
        assert_eq!(p, "<fim_prefix>a<fim_suffix>b<fim_middle>");
    }

    #[test]
    fn every_style_has_marker_row() {
        for style in [FimStyle::QwenCoder, FimStyle::Mellum, FimStyle::StarCoder2] {
            let m = markers_for(style);
            assert!(m.stop_strings.len() >= 2, "{:?} needs stop strings", style);
        }
    }

    #[test]
    fn mellum_row_disambiguates_from_starcoder2() {
        // Identical marker spellings; the discriminator tokens are
        // what keeps the probe deterministic.
        let mellum = markers_for(FimStyle::Mellum);
        let sc2 = markers_for(FimStyle::StarCoder2);
        assert_eq!(mellum.prefix, sc2.prefix);
        assert!(mellum.also_requires.contains(&"<|im_start|>"));
        assert!(sc2.also_requires.contains(&"<|end_of_text|>"));
        // Table order: Mellum first (chat-trained vocab wins the
        // shared spelling over StarCoder2's).
        let pos = |s: FimStyle| FIM_MARKER_TABLE.iter().position(|m| m.style == s).unwrap();
        assert!(pos(FimStyle::Mellum) < pos(FimStyle::StarCoder2));
    }

    #[test]
    fn tracker_passes_plain_text_through() {
        let mut t = FimStopTracker::new(FimStyle::QwenCoder);
        // Holdback is max-stop-len-1 bytes; feed enough that text releases.
        let f1 = t.feed("let x = compute_the_thing(arg1, arg2, arg3);");
        let Feed::Emit(s) = f1 else { panic!("expected Emit, got {f1:?}") };
        assert!(s.starts_with("let x = compute"));
    }

    #[test]
    fn tracker_stops_on_marker_and_withholds_it() {
        let mut t = FimStopTracker::new(FimStyle::QwenCoder);
        let f = t.feed("done();<|endoftext|>garbage after");
        let Feed::Stop { text, outcome } = f else {
            panic!("expected Stop, got {f:?}")
        };
        assert!(text.ends_with("done();"), "text was {text:?}");
        assert!(!text.contains("<|endoftext|>"));
        assert_eq!(outcome.rule, StopRule::StopString);
        assert!(outcome.trimmed >= "<|endoftext|>".len());
    }

    #[test]
    fn tracker_catches_stop_string_split_across_tokens() {
        let mut t = FimStopTracker::new(FimStyle::QwenCoder);
        // "<|endo" then "ftext|>" — the split marker must not leak.
        let f1 = t.feed("x = 1;<|endo");
        if let Feed::Emit(s) = &f1 {
            assert!(!s.contains("<|endo"), "split marker leaked: {s:?}");
        }
        let f2 = t.feed("ftext|>more");
        let Feed::Stop { text, .. } = f2 else {
            panic!("expected Stop on completed marker, got {f2:?}")
        };
        assert!(!text.contains("<|endoftext|>"));
    }

    #[test]
    fn tracker_flush_releases_holdback() {
        let mut t = FimStopTracker::new(FimStyle::QwenCoder);
        let _ = t.feed("tail");
        let rest = t.flush();
        assert_eq!(rest, "tail");
    }

    #[test]
    fn tracker_earliest_of_multiple_stops_wins() {
        let mut t = FimStopTracker::new(FimStyle::QwenCoder);
        let f = t.feed("abc<|fim_pad|>mid<|file_sep|>end");
        let Feed::Stop { text, .. } = f else {
            panic!("expected Stop, got {f:?}")
        };
        assert!(text.ends_with("abc"), "earliest stop should win: {text:?}");
    }

    // ── F1: mode decision ──────────────────────────────────────────

    #[test]
    fn decide_mode_multi_on_block_openers() {
        for tail in [
            "fn main() {",
            "if (x) {",
            "foo(",
            "items[",
            "def f():",
            "    match n {",
            "x =>",
            "_ =>",
        ] {
            assert_eq!(decide_mode(tail), FimMode::Multi, "{tail:?} should be Multi");
        }
    }

    #[test]
    fn decide_mode_single_by_default() {
        for tail in [
            "let x = ",
            "foo(bar",
            "return a +",
            "",
            "};",         // closes, not opens
            "fn main() {}", // already closed
        ] {
            assert_eq!(decide_mode(tail), FimMode::Single, "{tail:?} should be Single");
        }
    }

    // ── F1: single-line rule ───────────────────────────────────────

    #[test]
    fn single_mode_stops_at_newline_excluding_it() {
        let mut t = FimStopTracker::new_with_extra(
            FimStyle::QwenCoder,
            vec![],
            FimMode::Single,
            "",
        );
        let f = t.feed("a + b;\nnext_line()");
        let Feed::Stop { text, outcome } = f else {
            panic!("expected Stop, got {f:?}")
        };
        assert!(text.ends_with("a + b;"), "text was {text:?}");
        assert!(!text.contains('\n'));
        assert_eq!(outcome.rule, StopRule::Newline);
    }

    #[test]
    fn single_mode_ignores_brackets() {
        let mut t = FimStopTracker::new_with_extra(
            FimStyle::QwenCoder,
            vec![],
            FimMode::Single,
            "",
        );
        // A close-bracket in single mode is just text (completing a
        // call expression), not a depth stop. (Feed exceeds the
        // holdback tail so the text actually releases.)
        let f = t.feed("let result = foo(bar, baz, quux)");
        let Feed::Emit(s) = f else { panic!("expected Emit, got {f:?}") };
        assert!(s.contains("foo(bar"), "brackets pass through: {s:?}");
    }

    // ── F1: multi-line rules ───────────────────────────────────────

    #[test]
    fn multi_mode_depth_close_emits_through_closer() {
        let mut t = FimStopTracker::new_with_extra(
            FimStyle::QwenCoder,
            vec![],
            FimMode::Multi,
            "",
        );
        let f = t.feed("\n    _ => a + b,\n}\nfn next() {");
        let Feed::Stop { text, outcome } = f else {
            panic!("expected Stop, got {f:?}")
        };
        assert!(text.ends_with("\n}"), "closer kept, trailing fn dropped: {text:?}");
        assert!(!text.contains("fn next"));
        assert_eq!(outcome.rule, StopRule::DepthClose);
    }

    #[test]
    fn multi_mode_nested_brackets_do_not_stop_at_zero() {
        let mut t = FimStopTracker::new_with_extra(
            FimStyle::QwenCoder,
            vec![],
            FimMode::Multi,
            "",
        );
        // Model opens + closes its OWN brackets inside the body:
        // depth returns to 0 but never negative — must NOT stop.
        // (Prefix ended with `{`, so the user's opener is at −1.)
        let f1 = t.feed("if (x) { y(); }");
        assert!(matches!(f1, Feed::Emit(_)), "depth-0 must not stop: {f1:?}");
        let f2 = t.feed("\n z();\n}");
        let Feed::Stop { text, outcome } = f2 else {
            panic!("expected Stop at enclosing close, got {f2:?}")
        };
        assert!(text.ends_with('}'), "closer kept: {text:?}");
        assert_eq!(outcome.rule, StopRule::DepthClose);
    }

    #[test]
    fn multi_mode_depth_close_split_across_tokens() {
        let mut t = FimStopTracker::new_with_extra(
            FimStyle::QwenCoder,
            vec![],
            FimMode::Multi,
            "",
        );
        // Body arrives balanced, then the closer of the USER's
        // opener lands in a later token.
        let _ = t.feed("x,\n  y,\n");
        let f = t.feed(")\nmore");
        let Feed::Stop { text, outcome } = f else {
            panic!("expected Stop, got {f:?}")
        };
        assert!(text.ends_with(')'), "closer kept: {text:?}");
        assert_eq!(outcome.rule, StopRule::DepthClose);
    }

    #[test]
    fn multi_mode_blank_line_stops() {
        let mut t = FimStopTracker::new_with_extra(
            FimStyle::QwenCoder,
            vec![],
            FimMode::Multi,
            "",
        );
        // Python-style body (prefix ended with ':'): no brackets, so
        // the blank line is the terminator.
        let f = t.feed("    total = sum(items)\n    return total\n\ndef next():");
        let Feed::Stop { text, outcome } = f else {
            panic!("expected Stop, got {f:?}")
        };
        assert!(text.ends_with("return total\n"), "text was {text:?}");
        assert!(!text.contains("def next"));
        assert_eq!(outcome.rule, StopRule::BlankLine);
    }

    #[test]
    fn multi_mode_blank_line_split_across_tokens() {
        let mut t = FimStopTracker::new_with_extra(
            FimStyle::QwenCoder,
            vec![],
            FimMode::Multi,
            "",
        );
        let f1 = t.feed("return x\n");
        assert!(matches!(f1, Feed::Emit(_)), "lone newline is not blank: {f1:?}");
        let f2 = t.feed("\nrest");
        let Feed::Stop { outcome, .. } = f2 else {
            panic!("expected blank-line Stop once the second newline arrived, got {f2:?}")
        };
        assert_eq!(outcome.rule, StopRule::BlankLine);
    }

    #[test]
    fn multi_mode_max_lines_stops() {
        let mut t = FimStopTracker::new_with_extra(
            FimStyle::QwenCoder,
            vec![],
            FimMode::Multi,
            "",
        );
        let mut last = Feed::Emit(String::new());
        for i in 0..FIM_DEFAULT_MAX_LINES {
            last = t.feed(&format!("line {i}\n"));
            if matches!(last, Feed::Stop { .. }) {
                break;
            }
        }
        let Feed::Stop { outcome, .. } = last else {
            panic!("expected MaxLines stop, got {last:?}")
        };
        assert_eq!(outcome.rule, StopRule::MaxLines);
    }

    // ── F1: suffix duplication ─────────────────────────────────────

    #[test]
    fn suffix_duplication_trims_overlap() {
        let suffix = "    return total;\n}";
        let mut t = FimStopTracker::new_with_extra(
            FimStyle::QwenCoder,
            vec![],
            FimMode::Multi,
            suffix,
        );
        // Model finishes the body then starts REGENERATING the suffix.
        let f = t.feed("    for x in xs {\n        total += x;\n    }\n    return total;\n}\n");
        let Feed::Stop { text, outcome } = f else {
            panic!("expected Stop, got {f:?}")
        };
        assert!(
            text.trim_end().ends_with("    }"),
            "dup span trimmed, body kept: {text:?}"
        );
        assert!(!text.contains("return total"));
        assert_eq!(outcome.rule, StopRule::SuffixDuplication);
    }

    #[test]
    fn suffix_probe_too_short_disables_dup_detection() {
        // Suffix "}" trims to 1 char — unsafe to probe, disabled.
        let mut t = FimStopTracker::new_with_extra(
            FimStyle::QwenCoder,
            vec![],
            FimMode::Single,
            "}",
        );
        let f = t.feed("x }");
        assert!(matches!(f, Feed::Emit(_)), "trivial probe must not fire: {f:?}");
    }

    // ── F1: cross-rule precedence ──────────────────────────────────

    #[test]
    fn earliest_position_wins_across_rules() {
        // Newline BEFORE the stop string → Newline.
        let mut t = FimStopTracker::new_with_extra(
            FimStyle::QwenCoder,
            vec![],
            FimMode::Single,
            "",
        );
        let f = t.feed("abc\n<|endoftext|>");
        let Feed::Stop { outcome, .. } = f else { panic!("got {f:?}") };
        assert_eq!(outcome.rule, StopRule::Newline);

        // Stop string BEFORE the newline → StopString.
        let mut t = FimStopTracker::new_with_extra(
            FimStyle::QwenCoder,
            vec![],
            FimMode::Single,
            "",
        );
        let f = t.feed("abc<|endoftext|>\n");
        let Feed::Stop { outcome, .. } = f else { panic!("got {f:?}") };
        assert_eq!(outcome.rule, StopRule::StopString);
    }

    #[test]
    fn client_extra_stop_string_fires() {
        let mut t = FimStopTracker::new_with_extra(
            FimStyle::QwenCoder,
            vec!["\n\n".to_string()],
            FimMode::Single,
            "",
        );
        let f = t.feed("line one\n\nline two");
        let Feed::Stop { text, outcome } = f else { panic!("got {f:?}") };
        assert!(text.ends_with("line one"), "text was {text:?}");
        assert_eq!(outcome.rule, StopRule::StopString);
    }
}
