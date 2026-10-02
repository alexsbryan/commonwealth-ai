// SPDX-License-Identifier: AGPL-3.0-or-later
//! Markup stripping over raw text: HTML tags and entities, MediaWiki markup.
//!
//! Pure `&str -> String` functions, no dependency. Both programs strip the
//! same markup: ingest's extractors and svrn's corpus parsers
//! (`sovereign_tools::corpus`, `sec_edgar`). Their copies drifted once, so
//! the one implementation lives here, below both (pb-ingest-dial-tools-close).
//! `corpus-engine` re-exports each at its historical path
//! (`extractors::strip_html`, `extractors::xml::strip_mediawiki`).

/// Strip HTML tags and decode common entities.
///
/// `pub` since 2026-08-20: `sovereign-tools` carried a hand-copy of this
/// function for its crawl / StackExchange / SEC-EDGAR parsers, and the copies
/// DRIFTED — the fork never got the closing-tag clause below, so it discarded
/// every character after the first `</script>` in a document. One
/// implementation, named once (`ARCH_PRINCIPLES` §10.6).
pub fn strip_html(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut tag_name = String::new();
    let mut collecting_tag_name = false;

    let mut chars = html.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '<' {
            in_tag = true;
            collecting_tag_name = true;
            tag_name.clear();
            continue;
        }
        if in_tag {
            if collecting_tag_name {
                // Include '/' as part of the tag name for closing tags like </script>.
                if ch == '/' && tag_name.is_empty() {
                    tag_name.push(ch);
                    continue;
                }
                if ch.is_ascii_whitespace() || ch == '>' || ch == '/' {
                    collecting_tag_name = false;
                    let lower = tag_name.to_lowercase();
                    if lower == "script" {
                        in_script = true;
                    } else if lower == "/script" {
                        in_script = false;
                    } else if lower == "style" {
                        in_style = true;
                    } else if lower == "/style" {
                        in_style = false;
                    } else if lower == "br"
                        || lower == "br/"
                        || ((lower == "p" || lower == "/p" || lower == "div" || lower == "/div")
                            && !result.ends_with('\n'))
                    {
                        result.push('\n');
                    }
                } else {
                    tag_name.push(ch);
                }
            }
            if ch == '>' {
                in_tag = false;
            }
            continue;
        }
        if in_script || in_style {
            continue;
        }
        if ch == '&' {
            let mut entity = String::new();
            for ec in chars.by_ref() {
                if ec == ';' {
                    break;
                }
                entity.push(ec);
                if entity.len() > 10 {
                    break;
                }
            }
            match entity.as_str() {
                "amp" => result.push('&'),
                "lt" => result.push('<'),
                "gt" => result.push('>'),
                "quot" => result.push('"'),
                "apos" => result.push('\''),
                "nbsp" => result.push(' '),
                s if s.starts_with('#') => {
                    let num_str = &s[1..];
                    let code = if let Some(hex) = num_str.strip_prefix('x') {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        num_str.parse::<u32>().ok()
                    };
                    if let Some(c) = code.and_then(char::from_u32) {
                        result.push(c);
                    }
                }
                _ => {
                    result.push('&');
                    result.push_str(&entity);
                    result.push(';');
                }
            }
            continue;
        }
        result.push(ch);
    }

    // Collapse excessive whitespace.
    let mut collapsed = String::with_capacity(result.len());
    let mut prev_newline = false;
    for line in result.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !prev_newline {
                collapsed.push('\n');
                prev_newline = true;
            }
        } else {
            collapsed.push_str(trimmed);
            collapsed.push('\n');
            prev_newline = false;
        }
    }

    collapsed.trim().to_string()
}

// ─── MediaWiki Markup Stripping ───────────────────────────────

/// Strip MediaWiki markup, producing plain text.
/// Strip MediaWiki markup down to plain text.
///
/// `pub` since 2026-08-20 — `sovereign_tools::corpus::wikipedia` carried a
/// byte-for-byte hand-copy (comments aside). One implementation (§10.6).
pub fn strip_mediawiki(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            // Templates: {{...}} -- remove entirely (may be nested).
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                skip_nested(&mut chars, '{', '}');
            }
            // Tables: {|...|} -- remove entirely.
            '{' if chars.peek() == Some(&'|') => {
                chars.next();
                let mut depth = 1;
                while let Some(c) = chars.next() {
                    if c == '{' && chars.peek() == Some(&'|') {
                        chars.next();
                        depth += 1;
                    } else if c == '|' && chars.peek() == Some(&'}') {
                        chars.next();
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
            }
            // Wikilinks: [[target|display]] -> display, or [[target]] -> target.
            '[' if chars.peek() == Some(&'[') => {
                chars.next();
                let mut link_text = String::new();
                let mut depth = 1;
                while let Some(c) = chars.next() {
                    if c == '[' && chars.peek() == Some(&'[') {
                        chars.next();
                        depth += 1;
                        link_text.push_str("[[");
                    } else if c == ']' && chars.peek() == Some(&']') {
                        chars.next();
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        link_text.push_str("]]");
                    } else {
                        link_text.push(c);
                    }
                }
                // Use display text (after |), or the full link text.
                let display = link_text
                    .rsplit_once('|')
                    .map(|(_, d)| d)
                    .unwrap_or(&link_text);
                // Skip file/image links.
                if !link_text.starts_with("File:")
                    && !link_text.starts_with("Image:")
                    && !link_text.starts_with("Category:")
                {
                    result.push_str(display);
                }
            }
            // External links: [url text] -> text.
            '[' => {
                let mut link = String::new();
                for c in chars.by_ref() {
                    if c == ']' {
                        break;
                    }
                    link.push(c);
                }
                // Display text is everything after the first space.
                if let Some(pos) = link.find(' ') {
                    result.push_str(&link[pos + 1..]);
                }
            }
            // Bold/italic: '''text''' or ''text''.
            '\'' if chars.peek() == Some(&'\'') => {
                while chars.peek() == Some(&'\'') {
                    chars.next();
                }
            }
            // HTML-like tags: <ref>...</ref>, <nowiki>, etc.
            '<' => {
                let mut tag = String::new();
                let mut is_closing = false;
                for c in chars.by_ref() {
                    if c == '>' {
                        break;
                    }
                    tag.push(c);
                }
                if tag.starts_with('/') {
                    is_closing = true;
                    tag = tag[1..].to_string();
                }
                let tag_name = tag.split_whitespace().next().unwrap_or("").to_lowercase();
                // For ref, nowiki, gallery, etc. -- skip content until closing tag.
                if !is_closing
                    && !tag.ends_with('/')
                    && matches!(
                        tag_name.as_str(),
                        "ref"
                            | "nowiki"
                            | "gallery"
                            | "math"
                            | "source"
                            | "syntaxhighlight"
                            | "code"
                    )
                {
                    let close = format!("</{tag_name}>");
                    let mut buf = String::new();
                    for c in chars.by_ref() {
                        buf.push(c);
                        if buf.ends_with(&close) {
                            break;
                        }
                    }
                }
            }
            // Section headers: == Title == -> preserved as text.
            '=' if result.ends_with('\n') || result.is_empty() => {
                result.push(ch);
            }
            _ => result.push(ch),
        }
    }

    result
}

/// Skip nested pairs, e.g., {{ ... {{ ... }} ... }}.
fn skip_nested(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, open: char, close: char) {
    let mut depth = 1;
    while let Some(c) = chars.next() {
        if c == open && chars.peek() == Some(&open) {
            chars.next();
            depth += 1;
        } else if c == close && chars.peek() == Some(&close) {
            chars.next();
            depth -= 1;
            if depth == 0 {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_html_basic() {
        assert_eq!(strip_html("<p>Hello</p>"), "Hello");
    }

    #[test]
    fn strip_html_entities() {
        assert_eq!(strip_html("a &amp; b &lt; c"), "a & b < c");
    }

    #[test]
    fn strip_html_script() {
        let html = "before<script>var x = 1;</script>after";
        let result = strip_html(html);
        assert!(result.contains("before"));
        assert!(result.contains("after"));
        assert!(!result.contains("var x"));
    }

    #[test]
    fn strip_html_numeric_entities() {
        assert_eq!(strip_html("&#65;&#x42;"), "AB");
    }

    #[test]
    fn strip_mediawiki_templates() {
        let text = "Before {{Infobox|name=Test}} after.";
        let result = strip_mediawiki(text);
        assert!(result.contains("Before"));
        assert!(result.contains("after."));
        assert!(!result.contains("Infobox"));
    }

    #[test]
    fn strip_mediawiki_wikilinks() {
        let text = "A [[programming language]] and [[Rust (lang)|Rust]].";
        let result = strip_mediawiki(text);
        assert!(result.contains("programming language"));
        assert!(result.contains("Rust"));
        assert!(!result.contains("[["));
    }

    #[test]
    fn strip_mediawiki_bold_italic() {
        let text = "'''Bold''' and ''italic'' text.";
        let result = strip_mediawiki(text);
        assert!(result.contains("Bold"));
        assert!(result.contains("italic"));
        assert!(!result.contains("'''"));
    }
}
