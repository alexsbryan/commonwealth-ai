// SPDX-License-Identifier: AGPL-3.0-or-later
//! PP-OCR recognition character dictionary.
//!
//! PaddleOCR dict files list one character per line (UTF-8). The CTC
//! blank occupies model output index 0, so we prepend a sentinel and
//! index `dict[argmax]` directly — matching PaddleOCR's `CTCLabelDecode`
//! (which prepends `"blank"` to the char list). Some exports also emit a
//! trailing space class; rather than hardcode `+1` vs `+2`, the decoder
//! reconciles `dict.len()` against the actual logits width at runtime
//! and pads with spaces if needed (logged), so a dict/model mismatch is
//! loud rather than a silent off-by-one.

use std::path::Path;

use super::PaddleError;

/// Index 0 sentinel for the CTC blank token.
pub const BLANK: &str = "<blank>";

/// Load a PaddleOCR dictionary as `[<blank>, line0, line1, …]`.
pub fn load_dict(path: &Path) -> Result<Vec<String>, PaddleError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| PaddleError::Model(format!("read dict {}: {e}", path.display())))?;
    // Preserve lines verbatim (a line may be a multi-byte glyph). Do NOT
    // trim — a dict entry can legitimately be a space-like char — but
    // drop a trailing empty line from the file's final newline.
    let mut dict = Vec::with_capacity(raw.lines().count() + 1);
    dict.push(BLANK.to_string());
    for line in raw.split('\n') {
        // The final split element after a trailing '\n' is empty; skip
        // only that case, not interior entries.
        if line.is_empty() {
            continue;
        }
        dict.push(line.strip_suffix('\r').unwrap_or(line).to_string());
    }
    Ok(dict)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn load_prepends_blank_and_preserves_order() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        // 3 chars, trailing newline.
        write!(f, "a\nb\nc\n").unwrap();
        let dict = load_dict(f.path()).unwrap();
        assert_eq!(dict, vec!["<blank>", "a", "b", "c"]);
        // blank at 0, first real char at index 1.
        assert_eq!(dict[0], BLANK);
        assert_eq!(dict[1], "a");
    }

    #[test]
    fn load_handles_crlf_and_no_trailing_newline() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "x\r\ny").unwrap();
        let dict = load_dict(f.path()).unwrap();
        assert_eq!(dict, vec!["<blank>", "x", "y"]);
    }
}
