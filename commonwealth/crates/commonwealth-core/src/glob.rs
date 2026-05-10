/// Simple glob matching with `*` wildcard.
///
/// `*` matches any sequence of characters (including empty).
/// Matching is case-insensitive.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let text = text.to_lowercase();
    glob_match_inner(&pattern, &text)
}

fn glob_match_inner(pattern: &str, text: &str) -> bool {
    // Split pattern on '*'.
    let parts: Vec<&str> = pattern.split('*').collect();

    if parts.len() == 1 {
        // No wildcard — exact match.
        return pattern == text;
    }

    let mut pos = 0;

    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue; // Leading, trailing, or consecutive '*'.
        }

        if i == 0 {
            // First part: text must start with it.
            if !text.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == parts.len() - 1 {
            // Last part: text must end with it.
            if !text[pos..].ends_with(part) {
                return false;
            }
            pos = text.len();
        } else {
            // Middle part: find it anywhere after current position.
            match text[pos..].find(part) {
                Some(found) => pos += found + part.len(),
                None => return false,
            }
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        assert!(glob_match("hello", "hello"));
        assert!(!glob_match("hello", "world"));
    }

    #[test]
    fn leading_wildcard() {
        assert!(glob_match("*codex", "gpt-5.3-codex"));
        assert!(glob_match("*codex", "codex"));
        assert!(!glob_match("*codex", "codex-plus"));
    }

    #[test]
    fn trailing_wildcard() {
        assert!(glob_match("gpt-5*", "gpt-5.3-codex"));
        assert!(glob_match("gpt-5*", "gpt-5"));
        assert!(!glob_match("gpt-5*", "gpt-4"));
    }

    #[test]
    fn middle_wildcard() {
        assert!(glob_match("gpt-5*codex", "gpt-5.3-codex"));
        assert!(glob_match("gpt-5*codex", "gpt-5-codex"));
        assert!(!glob_match("gpt-5*codex", "gpt-4.3-codex"));
    }

    #[test]
    fn multiple_wildcards() {
        assert!(glob_match("gpt*5*codex*", "gpt-5.3-codex"));
        assert!(glob_match("*code*", "gpt-5.3-codex"));
        assert!(glob_match("*coder*", "my-coder-model"));
        assert!(!glob_match("*coder*", "my-model"));
    }

    #[test]
    fn case_insensitive() {
        assert!(glob_match("GPT-5*", "gpt-5.3-codex"));
        assert!(glob_match("claude-opus*", "Claude-Opus-4-6"));
    }

    #[test]
    fn only_wildcard() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*", ""));
    }

    #[test]
    fn no_match() {
        assert!(!glob_match("claude*", "gpt-5"));
        assert!(!glob_match("opus", "claude-opus"));
    }

    #[test]
    fn empty_strings() {
        assert!(glob_match("", ""));
        assert!(!glob_match("", "something"));
        assert!(glob_match("*", ""));
    }
}
