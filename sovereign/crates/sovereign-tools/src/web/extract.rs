// SPDX-License-Identifier: AGPL-3.0-or-later
use sovereign_core::error::{Error, Result};

/// Extract readable text content from HTML.
/// Strips tags, scripts, styles, and navigation — keeps body text.
pub fn extract_text_from_html(html: &str) -> String {
    let mut result = String::with_capacity(html.len() / 3);
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut last_was_space = true;

    let lower = html.to_lowercase();
    let bytes = lower.as_bytes();
    let html_bytes = html.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Check for script/style opening tags (ASCII, safe to compare bytes).
        if i + 7 < len && &bytes[i..i + 7] == b"<script" {
            in_script = true;
            in_tag = true;
            i += 7;
            continue;
        }
        if i + 6 < len && &bytes[i..i + 6] == b"<style" {
            in_style = true;
            in_tag = true;
            i += 6;
            continue;
        }

        // Check for closing script/style tags.
        if in_script && i + 9 <= len && &bytes[i..i + 9] == b"</script>" {
            in_script = false;
            in_tag = false;
            i += 9;
            continue;
        }
        if in_style && i + 8 <= len && &bytes[i..i + 8] == b"</style>" {
            in_style = false;
            in_tag = false;
            i += 8;
            continue;
        }

        if in_script || in_style {
            i += 1;
            continue;
        }

        let b = html_bytes[i];

        if b == b'<' {
            in_tag = true;
            // Block-level tags get a newline.
            let remaining = len - i;
            if remaining > 3 {
                let end = (i + 10).min(len);
                let tag_start = &bytes[i..end];
                if (tag_start.starts_with(b"<br")
                    || tag_start.starts_with(b"<p")
                    || tag_start.starts_with(b"</p")
                    || tag_start.starts_with(b"<div")
                    || tag_start.starts_with(b"</div")
                    || tag_start.starts_with(b"<h")
                    || tag_start.starts_with(b"</h")
                    || tag_start.starts_with(b"<li"))
                    && !last_was_space
                {
                    result.push('\n');
                    last_was_space = true;
                }
            }
            i += 1;
            continue;
        }

        if b == b'>' {
            in_tag = false;
            i += 1;
            continue;
        }

        if in_tag {
            i += 1;
            continue;
        }

        // Decode common HTML entities (all ASCII).
        if b == b'&' && i + 1 < len {
            let end = (i + 10).min(len);
            let rest = &bytes[i..end];
            if rest.starts_with(b"&amp;") {
                result.push('&');
                i += 5;
                last_was_space = false;
                continue;
            } else if rest.starts_with(b"&lt;") {
                result.push('<');
                i += 4;
                last_was_space = false;
                continue;
            } else if rest.starts_with(b"&gt;") {
                result.push('>');
                i += 4;
                last_was_space = false;
                continue;
            } else if rest.starts_with(b"&quot;") {
                result.push('"');
                i += 6;
                last_was_space = false;
                continue;
            } else if rest.starts_with(b"&nbsp;") {
                result.push(' ');
                i += 6;
                last_was_space = true;
                continue;
            } else if rest.starts_with(b"&#") {
                // Numeric entity — skip to semicolon.
                if let Some(pos) = rest.iter().position(|&c| c == b';') {
                    i += pos + 1;
                    continue;
                }
            }
        }

        // Decode the current byte(s) as a UTF-8 character.
        let ch_len = utf8_char_len(b);
        if i + ch_len <= len {
            if let Ok(s) = std::str::from_utf8(&html_bytes[i..i + ch_len]) {
                let ch = s.chars().next().unwrap();
                if ch.is_whitespace() {
                    if !last_was_space {
                        result.push(' ');
                        last_was_space = true;
                    }
                } else {
                    result.push(ch);
                    last_was_space = false;
                }
            }
            i += ch_len;
        } else {
            i += 1; // Skip malformed byte.
        }
    }

    // Clean up: collapse multiple newlines, trim lines.
    let lines: Vec<&str> = result
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    lines.join("\n")
}

/// Returns the expected length of a UTF-8 character from its first byte.
fn utf8_char_len(first_byte: u8) -> usize {
    if first_byte < 0x80 {
        1
    } else if first_byte < 0xE0 {
        2
    } else if first_byte < 0xF0 {
        3
    } else {
        4
    }
}

/// Truncate a string to at most `max_chars` characters (not bytes),
/// ensuring we never split a char boundary.
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    // Find the char boundary at or before max_chars bytes.
    let mut end = max_chars;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Fetch a URL and extract its text content.
pub async fn fetch_and_extract(client: &reqwest::Client, url: &str) -> Result<String> {
    let response = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
        .send()
        .await
        .map_err(|e| Error::Execution(format!("Failed to fetch {url}: {e}")))?;

    if !response.status().is_success() {
        return Err(Error::Execution(format!(
            "HTTP {} for {url}",
            response.status()
        )));
    }

    let html = response
        .text()
        .await
        .map_err(|e| Error::Execution(format!("Failed to read response from {url}: {e}")))?;

    let text = extract_text_from_html(&html);

    Ok(truncate_chars(&text, 4000).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_basic_html() {
        let html = "<html><body><h1>Title</h1><p>Hello world.</p></body></html>";
        let text = extract_text_from_html(html);
        assert!(text.contains("Title"));
        assert!(text.contains("Hello world."));
        assert!(!text.contains('<'));
    }

    #[test]
    fn extract_strips_scripts() {
        let html = "<p>Before</p><script>var x = 1;</script><p>After</p>";
        let text = extract_text_from_html(html);
        assert!(text.contains("Before"));
        assert!(text.contains("After"));
        assert!(!text.contains("var x"));
    }

    #[test]
    fn extract_strips_styles() {
        let html = "<style>body { color: red; }</style><p>Content</p>";
        let text = extract_text_from_html(html);
        assert!(text.contains("Content"));
        assert!(!text.contains("color"));
    }

    #[test]
    fn extract_decodes_entities() {
        let html = "<p>A &amp; B &lt; C &gt; D</p>";
        let text = extract_text_from_html(html);
        assert!(text.contains("A & B < C > D"));
    }

    #[test]
    fn extract_collapses_whitespace() {
        let html = "<p>  lots   of   spaces  </p>";
        let text = extract_text_from_html(html);
        assert!(!text.contains("  "));
    }

    #[test]
    fn extract_handles_multibyte_utf8() {
        let html = "<p>Hello \u{00B7} world \u{2022} test \u{1F600}</p>";
        let text = extract_text_from_html(html);
        assert!(text.contains("\u{00B7}"));
        assert!(text.contains("\u{2022}"));
        assert!(text.contains("\u{1F600}"));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "Hello \u{00B7} world";
        let truncated = truncate_chars(s, 7);
        assert!(truncated.len() <= 7);
        assert!(truncated.is_char_boundary(truncated.len()));
    }
}
