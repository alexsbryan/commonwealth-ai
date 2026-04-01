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
    let chars: Vec<char> = html.chars().collect();
    let _lower_chars: Vec<char> = lower.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Check for script/style opening tags.
        if i + 7 < len && &lower[i..i + 7] == "<script" {
            in_script = true;
            in_tag = true;
            i += 7;
            continue;
        }
        if i + 6 < len && &lower[i..i + 6] == "<style" {
            in_style = true;
            in_tag = true;
            i += 6;
            continue;
        }

        // Check for closing script/style tags.
        if in_script && i + 9 <= len && &lower[i..i + 9] == "</script>" {
            in_script = false;
            in_tag = false;
            i += 9;
            continue;
        }
        if in_style && i + 8 <= len && &lower[i..i + 8] == "</style>" {
            in_style = false;
            in_tag = false;
            i += 8;
            continue;
        }

        if in_script || in_style {
            i += 1;
            continue;
        }

        let ch = chars[i];

        if ch == '<' {
            in_tag = true;
            // Block-level tags get a newline.
            if i + 3 < len {
                let tag_start = &lower[i..lower.len().min(i + 10)];
                if tag_start.starts_with("<br")
                    || tag_start.starts_with("<p")
                    || tag_start.starts_with("</p")
                    || tag_start.starts_with("<div")
                    || tag_start.starts_with("</div")
                    || tag_start.starts_with("<h")
                    || tag_start.starts_with("</h")
                    || tag_start.starts_with("<li")
                {
                    if !last_was_space {
                        result.push('\n');
                        last_was_space = true;
                    }
                }
            }
            i += 1;
            continue;
        }

        if ch == '>' {
            in_tag = false;
            i += 1;
            continue;
        }

        if in_tag {
            i += 1;
            continue;
        }

        // Decode common HTML entities.
        if ch == '&' && i + 1 < len {
            let rest = &lower[i..lower.len().min(i + 10)];
            if rest.starts_with("&amp;") {
                result.push('&');
                i += 5;
                last_was_space = false;
                continue;
            } else if rest.starts_with("&lt;") {
                result.push('<');
                i += 4;
                last_was_space = false;
                continue;
            } else if rest.starts_with("&gt;") {
                result.push('>');
                i += 4;
                last_was_space = false;
                continue;
            } else if rest.starts_with("&quot;") {
                result.push('"');
                i += 6;
                last_was_space = false;
                continue;
            } else if rest.starts_with("&nbsp;") {
                result.push(' ');
                i += 6;
                last_was_space = true;
                continue;
            } else if rest.starts_with("&#") {
                // Numeric entity — skip it.
                if let Some(semi) = rest.find(';') {
                    i += semi + 1;
                    continue;
                }
            }
        }

        // Collapse whitespace.
        if ch.is_whitespace() {
            if !last_was_space {
                result.push(' ');
                last_was_space = true;
            }
        } else {
            result.push(ch);
            last_was_space = false;
        }

        i += 1;
    }

    // Clean up: collapse multiple newlines, trim lines.
    let lines: Vec<&str> = result
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    lines.join("\n")
}

/// Fetch a URL and extract its text content.
pub async fn fetch_and_extract(client: &reqwest::Client, url: &str) -> Result<String> {
    let response = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (compatible; Sovereign/0.1)")
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

    // Truncate to a reasonable size for LLM context.
    let max_chars = 4000;
    if text.len() > max_chars {
        Ok(text[..max_chars].to_string())
    } else {
        Ok(text)
    }
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
        assert!(!text.contains("  ")); // No double spaces.
    }
}
