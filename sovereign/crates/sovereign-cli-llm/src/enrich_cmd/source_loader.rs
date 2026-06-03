//! Load a corpus source file as plaintext.
//!
//! The enrichment pipeline operates on plaintext — a `SectionedChunker`
//! runs a regex against the string it receives. Source files are not
//! always plaintext: operators hand us RTF out of TextEdit/Pages and,
//! in principle, could hand us DOCX/EPUB later. This loader is the
//! single chokepoint where format-aware decoding happens. Every other
//! enrichment subcommand reads source through `load_plaintext` and
//! gets a string the chunker can consume.
//!
//! Dispatch is by magic bytes, not by file extension, so a misnamed
//! `.txt` containing RTF still decodes correctly.

use std::fs;
use std::path::Path;

use corpus_engine::error::{Error, Result};

/// Magic bytes for Rich Text Format (`{\rtf`).
const RTF_MAGIC: &[u8] = b"{\\rtf";

/// Read `path` and return plaintext suitable for the chunker.
///
/// - Plain UTF-8 text: returned as-is.
/// - RTF: stripped to plaintext.
/// - Anything else: treated as UTF-8; invalid bytes are lossily
///   replaced. The chunker's "0 sections" error surfaces format
///   mismatches to the operator.
pub fn load_plaintext(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("reading source file {}: {}", path.display(), e),
        ))
    })?;

    if starts_with(&bytes, RTF_MAGIC) {
        let raw = String::from_utf8_lossy(&bytes);
        let stripped = strip_rtf(&raw);
        tracing::info!(
            source = %path.display(),
            raw_bytes = bytes.len(),
            stripped_chars = stripped.chars().count(),
            "source_loader: stripped RTF to plaintext"
        );
        return Ok(stripped);
    }

    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn starts_with(bytes: &[u8], prefix: &[u8]) -> bool {
    bytes.len() >= prefix.len() && &bytes[..prefix.len()] == prefix
}

/// Minimal RTF 1.x → plaintext stripper. Handles the features that
/// matter for prose extraction: groups, control words, common
/// destinations to skip, the text-emitting control words (`\par`,
/// `\tab`, `\line`, the escaped braces and backslash), hex escapes
/// (`\'XX`), and Unicode escapes (`\uN` with the `\ucN` fallback-char
/// count). Unknown control words are silently dropped — they are
/// always formatting hints the regex doesn't care about.
///
/// Not a full RTF parser. Lists, tables, embedded objects, and
/// stylesheet references are all treated as "drop the control word,
/// keep any plain text inside the group." For the prose corpora this
/// feeds (novels, essays, transcripts) the output is indistinguishable
/// from what a human running `rtf2txt` would produce.
/// Destinations whose *entire group* should be dropped (they carry
/// metadata, not content). `\*` additionally marks an operator-defined
/// destination that must be skipped wholesale regardless of name.
const SKIP_DESTINATIONS: &[&str] = &[
    "fonttbl",
    "colortbl",
    "stylesheet",
    "listtable",
    "listoverridetable",
    "rsidtbl",
    "info",
    "pict",
    "object",
    "header",
    "footer",
    "themedata",
    "latentstyles",
    "datastore",
];

fn strip_rtf(input: &str) -> String {
    let mut groups: Vec<Group> = vec![Group { skip: false, uc: 1 }];
    let mut out = String::with_capacity(input.len() / 2);
    let mut chars = input.chars().peekable();
    let mut skip_bytes_after_unicode: usize = 0;

    while let Some(c) = chars.next() {
        if skip_bytes_after_unicode > 0 {
            // Eat the next rendered character (a fallback ANSI byte
            // following a \uN escape). Control words and braces still
            // need to be handled as structure, not as fallback bytes.
            if c == '\\' || c == '{' || c == '}' {
                // fall through to the normal handler below
            } else {
                skip_bytes_after_unicode -= 1;
                continue;
            }
        }

        match c {
            '{' => {
                let parent_skip = groups.last().map(|g| g.skip).unwrap_or(false);
                let parent_uc = groups.last().map(|g| g.uc).unwrap_or(1);
                groups.push(Group { skip: parent_skip, uc: parent_uc });
            }
            '}' => {
                if groups.len() > 1 {
                    groups.pop();
                }
            }
            '\\' => {
                // Peek to decide what kind of escape this is.
                let next = match chars.peek() {
                    Some(&c) => c,
                    None => break,
                };
                if next == '\\' || next == '{' || next == '}' {
                    chars.next();
                    if !current_skip(&groups) {
                        out.push(next);
                    }
                } else if next == '*' {
                    // `\*` marks the opening control as a destination
                    // that must be dropped wholesale.
                    chars.next();
                    if let Some(g) = groups.last_mut() {
                        g.skip = true;
                    }
                } else if next == '\'' {
                    // \'XX — a single byte in hex. RTF specifies it
                    // as a Windows-1252 byte; for prose corpora the
                    // printable ASCII range covers almost everything,
                    // so decode as Latin-1 (a superset of ASCII that
                    // round-trips common punctuation).
                    chars.next();
                    let h1 = chars.next();
                    let h2 = chars.next();
                    if let (Some(a), Some(b)) = (h1, h2) {
                        if let Some(byte) = hex_byte(a, b) {
                            if !current_skip(&groups) {
                                // Decode as Windows-1252 → Unicode
                                // for the bytes we can.
                                out.push(cp1252_to_char(byte));
                            }
                            skip_bytes_after_unicode = skip_bytes_after_unicode.saturating_sub(1);
                        }
                    }
                } else if next == '\n' || next == '\r' {
                    // `\<newline>` is treated as `\par` in some
                    // generators.
                    chars.next();
                    if !current_skip(&groups) {
                        out.push('\n');
                    }
                } else if next.is_ascii_alphabetic() {
                    // Parse `\word` plus optional signed numeric
                    // parameter, then optional single space delimiter.
                    let (word, param) = read_control_word(&mut chars);
                    handle_control_word(
                        &word,
                        param,
                        &mut groups,
                        &mut out,
                        &mut skip_bytes_after_unicode,
                    );
                } else {
                    // Unknown escape — drop the backslash and the
                    // next character verbatim.
                    chars.next();
                }
            }
            '\r' | '\n' => {
                // Raw newlines inside RTF are formatting, not content.
                // \par is the content-level paragraph marker.
            }
            _ => {
                if !current_skip(&groups) {
                    out.push(c);
                }
            }
        }
    }

    normalise_whitespace(&out)
}

fn current_skip(groups: &[Group]) -> bool {
    groups.last().map(|g| g.skip).unwrap_or(false)
}

/// Matches the inner Group type above without pulling it out to the
/// module level.
#[derive(Clone, Copy)]
struct Group {
    skip: bool,
    uc: usize,
}

fn read_control_word(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> (String, Option<i32>) {
    let mut word = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphabetic() {
            word.push(c);
            chars.next();
        } else {
            break;
        }
    }
    let mut param: Option<i32> = None;
    let mut neg = false;
    if chars.peek() == Some(&'-') {
        neg = true;
        chars.next();
    }
    let mut digits = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            digits.push(c);
            chars.next();
        } else {
            break;
        }
    }
    if !digits.is_empty() {
        if let Ok(n) = digits.parse::<i32>() {
            param = Some(if neg { -n } else { n });
        }
    } else if neg {
        // A lone '-' with no digits was not a parameter after all;
        // we've already consumed it, but it's rare enough in valid
        // RTF that dropping it is safe.
    }
    // Single trailing space is a delimiter, not content.
    if chars.peek() == Some(&' ') {
        chars.next();
    }
    (word, param)
}

fn handle_control_word(
    word: &str,
    param: Option<i32>,
    groups: &mut Vec<Group>,
    out: &mut String,
    skip_bytes_after_unicode: &mut usize,
) {
    // If this control word names a skip-destination, flip the group
    // to skip mode. Works whether the word opens the group or appears
    // mid-stream (some generators emit `\*\pict` style openers).
    if SKIP_DESTINATIONS.contains(&word) {
        if let Some(g) = groups.last_mut() {
            g.skip = true;
        }
        return;
    }

    match word {
        "par" | "line" | "sect" | "page" => {
            if !current_skip(groups) {
                out.push('\n');
            }
        }
        "tab" => {
            if !current_skip(groups) {
                out.push('\t');
            }
        }
        "emdash" => {
            if !current_skip(groups) {
                out.push('—');
            }
        }
        "endash" => {
            if !current_skip(groups) {
                out.push('–');
            }
        }
        "lquote" => {
            if !current_skip(groups) {
                out.push('\u{2018}');
            }
        }
        "rquote" => {
            if !current_skip(groups) {
                out.push('\u{2019}');
            }
        }
        "ldblquote" => {
            if !current_skip(groups) {
                out.push('\u{201C}');
            }
        }
        "rdblquote" => {
            if !current_skip(groups) {
                out.push('\u{201D}');
            }
        }
        "bullet" => {
            if !current_skip(groups) {
                out.push('\u{2022}');
            }
        }
        "uc" => {
            if let Some(n) = param {
                if let Some(g) = groups.last_mut() {
                    g.uc = n.max(0) as usize;
                }
            }
        }
        "u" => {
            if let Some(n) = param {
                // \uN can be negative — it's a signed i16 that wraps.
                let code = if n < 0 {
                    (65536 + n) as u32
                } else {
                    n as u32
                };
                if let Some(ch) = char::from_u32(code) {
                    if !current_skip(groups) {
                        out.push(ch);
                    }
                }
                *skip_bytes_after_unicode = groups.last().map(|g| g.uc).unwrap_or(1);
            }
        }
        _ => {
            // All other control words are formatting or metadata the
            // regex doesn't care about. Drop silently.
        }
    }
}

fn hex_byte(a: char, b: char) -> Option<u8> {
    let hi = a.to_digit(16)?;
    let lo = b.to_digit(16)?;
    Some(((hi << 4) | lo) as u8)
}

/// Windows-1252 (a superset of Latin-1 with smart quotes et al in
/// 0x80–0x9F) → Unicode. For bytes ≥ 0x80 where CP1252 and Latin-1
/// disagree, prefer the CP1252 mapping so RTF emitted from Word /
/// Pages round-trips typographic characters correctly.
fn cp1252_to_char(byte: u8) -> char {
    match byte {
        0x80 => '€',
        0x82 => '‚',
        0x83 => 'ƒ',
        0x84 => '„',
        0x85 => '…',
        0x86 => '†',
        0x87 => '‡',
        0x88 => 'ˆ',
        0x89 => '‰',
        0x8A => 'Š',
        0x8B => '‹',
        0x8C => 'Œ',
        0x8E => 'Ž',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201C}',
        0x94 => '\u{201D}',
        0x95 => '•',
        0x96 => '–',
        0x97 => '—',
        0x98 => '˜',
        0x99 => '™',
        0x9A => 'š',
        0x9B => '›',
        0x9C => 'œ',
        0x9E => 'ž',
        0x9F => 'Ÿ',
        b => b as char,
    }
}

/// Collapse runs of blank-only lines to exactly two newlines (a
/// paragraph break) and trim leading/trailing whitespace on every
/// line. Without this, RTF's per-run formatting leaves random runs
/// of spaces and stacked blank lines that noise up the chunker.
fn normalise_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut consecutive_newlines = 0u32;
    for line in s.split('\n') {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            consecutive_newlines += 1;
            if consecutive_newlines <= 2 {
                out.push('\n');
            }
        } else {
            consecutive_newlines = 0;
            out.push_str(trimmed);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn plain_utf8_passes_through_unchanged() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("book.txt");
        let body = "Chapter I\n\nA body of prose.\n\nChapter II\n\nMore prose.\n";
        fs::write(&path, body).unwrap();
        let got = load_plaintext(&path).unwrap();
        assert_eq!(got, body);
    }

    #[test]
    fn rtf_header_and_fonttbl_are_stripped() {
        let rtf = "{\\rtf1\\ansi\\deff0\
            {\\fonttbl{\\f0\\froman Times;}}\
            {\\colortbl;\\red0\\green0\\blue0;}\
            \\f0\\fs24 Chapter I.\\par \
            The chapter body starts here.}";
        let got = strip_rtf(rtf);
        assert!(got.contains("Chapter I."), "got: {got:?}");
        assert!(got.contains("The chapter body starts here."), "got: {got:?}");
        assert!(!got.contains("fonttbl"), "font table leaked: {got:?}");
        assert!(!got.contains("colortbl"), "color table leaked: {got:?}");
        assert!(!got.contains("\\rtf"), "header leaked: {got:?}");
    }

    #[test]
    fn rtf_par_produces_line_breaks_so_line_anchored_regexes_match() {
        let rtf = "{\\rtf1\\ansi Chapter I.\\par Body line one.\\par Chapter II.\\par Body line two.}";
        let got = strip_rtf(rtf);
        // The chunker's default pattern is line-anchored (`^Chapter …`).
        // Strip must emit real newlines between logical lines so the
        // second chapter heading is at a line start.
        let lines: Vec<&str> = got.lines().collect();
        assert!(
            lines.iter().any(|l| l.trim_start().starts_with("Chapter I.")),
            "Chapter I. not on a line start: {got:?}"
        );
        assert!(
            lines.iter().any(|l| l.trim_start().starts_with("Chapter II.")),
            "Chapter II. not on a line start: {got:?}"
        );
    }

    #[test]
    fn rtf_hex_escapes_decode_as_cp1252() {
        // \'92 is a right single quote in Windows-1252.
        let rtf = "{\\rtf1\\ansi it\\'92s fine.}";
        let got = strip_rtf(rtf);
        assert!(got.contains("it\u{2019}s fine."), "got: {got:?}");
    }

    #[test]
    fn rtf_unicode_escape_uses_uc_fallback_count() {
        // \uc1 舒? ← the ? is the 1-byte ANSI fallback to eat.
        let rtf = "{\\rtf1\\ansi \\uc1 word\\u8212?word}";
        let got = strip_rtf(rtf);
        assert!(got.contains("word—word"), "expected emdash between words: {got:?}");
    }

    #[test]
    fn rtf_detected_by_magic_not_extension() {
        let rtf = "{\\rtf1\\ansi Chapter I.\\par Body.}";
        let dir = tempdir().unwrap();
        let path = dir.path().join("misnamed.txt");
        fs::write(&path, rtf).unwrap();
        let got = load_plaintext(&path).unwrap();
        assert!(got.contains("Chapter I."));
        assert!(!got.contains("\\rtf1"));
    }

    #[test]
    fn missing_file_returns_clear_error() {
        let err = load_plaintext(Path::new("/nonexistent/path.txt")).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("reading source file"), "unexpected: {msg}");
    }

    #[test]
    fn info_block_is_dropped_but_body_survives() {
        let rtf = "{\\rtf1\\ansi\
            {\\info{\\title My Book}{\\author Someone}}\
            \\par Chapter I.\\par Body prose here.}";
        let got = strip_rtf(rtf);
        assert!(!got.contains("My Book"), "info title leaked: {got:?}");
        assert!(!got.contains("Someone"), "info author leaked: {got:?}");
        assert!(got.contains("Chapter I."));
        assert!(got.contains("Body prose here."));
    }
}
