// SPDX-License-Identifier: AGPL-3.0-or-later
//! Edit-action schema + emission parser.
//!
//! The model emits one fenced JSON action header + one fenced source
//! block per turn; this module parses both back into a typed
//! [`ParsedResponse`]. Tolerates inline JSON (no fence) and missing
//! closing fences on the source block (truncation-friendly), per the
//! 2026-05-24 isolation-probe findings.

use regex::Regex;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum EditAction {
    RewriteFunction {
        name: String,
    },
    PatchLines {
        start: u32,
        end: u32,
    },
    InsertBefore {
        line: u32,
    },
    /// Replace the entire file contents. `path` is optional — when
    /// the model knows where the file lands (Red writes a test
    /// file, multi-file extract writes a new module), it emits the
    /// path; the apply layer routes there. When `path` is None, the
    /// caller's discovered source-file path is used (the default
    /// Green-phase shape).
    WriteFile {
        #[serde(default)]
        path: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct ParsedResponse {
    pub action: EditAction,
    pub body: String,
    /// True when the action header was absent and the edit shape was
    /// inferred from the source block alone. Surfaced in candidate
    /// receipts so inference-rescued candidates stay attributable.
    pub inferred: bool,
}

pub fn parse_response(content: &str) -> Option<ParsedResponse> {
    if let Some(action) = parse_action_json(content) {
        let body = parse_source_block(content)?;
        return Some(ParsedResponse {
            action,
            body,
            inferred: false,
        });
    }
    // Header-inference fallback. Models — reliably at higher
    // temperatures — emit ONLY the source block and drop the JSON
    // action ceremony (2026-07-06 B-arm receipts: 6/8 candidates on
    // 2.2 were complete, close-fenced code with no header). The
    // block itself determines the edit shape; and a wrong inference
    // is harmless — the monotonic fitness gate only lands candidates
    // that strictly improve the passing count, so worst case equals
    // the parse-fail this replaces.
    let body = parse_source_block(content)?;
    let action = infer_action_from_body(&body)?;
    Some(ParsedResponse {
        action,
        body,
        inferred: true,
    })
}

/// Infer the edit shape from a bare source block. Rules, in order:
/// exactly ONE top-level function definition and no file-level
/// preamble → `RewriteFunction` on that name; any definitions or
/// file-level preamble (imports/module headers) → `WriteFile` to the
/// caller's discovered source file; otherwise None (prose blocks
/// stay parse-fails). Language-family general: indent-0 `def`/`class`
/// (Python) and keyword-introduced `fn`/`func`/`function` (brace
/// family) — the same families as the executor's bounds finder.
fn infer_action_from_body(body: &str) -> Option<EditAction> {
    let mut fn_names: Vec<String> = Vec::new();
    let mut class_like = 0usize;
    let mut preamble = false;
    for line in body.lines() {
        // Top-level only: no leading whitespace.
        if line.is_empty() || line.starts_with(char::is_whitespace) {
            continue;
        }
        for marker in ["use ", "import ", "from ", "package ", "#include", "mod "] {
            if line.starts_with(marker) {
                preamble = true;
            }
        }
        if line.starts_with("def ") || line.starts_with("async def ") {
            let after = line.split("def ").nth(1).unwrap_or("");
            if let Some(name) = ident_prefix(after) {
                fn_names.push(name);
            }
        } else if line.starts_with("class ") {
            class_like += 1;
        } else {
            for kw in ["fn ", "func ", "function "] {
                if let Some(pos) = line.find(kw) {
                    let prefix = &line[..pos];
                    let qualifier_only = prefix.chars().all(|c| {
                        c.is_alphanumeric() || c.is_whitespace() || c == '(' || c == ')' || c == '_'
                    });
                    if qualifier_only {
                        if let Some(name) = ident_prefix(&line[pos + kw.len()..]) {
                            fn_names.push(name);
                        }
                    }
                    break;
                }
            }
        }
    }
    if fn_names.len() == 1 && class_like == 0 && !preamble {
        return Some(EditAction::RewriteFunction {
            name: fn_names.remove(0),
        });
    }
    if !fn_names.is_empty() || class_like > 0 || preamble {
        return Some(EditAction::WriteFile { path: None });
    }
    None
}

fn ident_prefix(s: &str) -> Option<String> {
    let name: String = s
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn parse_action_json(content: &str) -> Option<EditAction> {
    let fenced = Regex::new(r"(?s)```json\s*\n(\{[^`]*?\})\s*\n```").unwrap();
    if let Some(c) = fenced.captures(content) {
        if let Ok(v) = serde_json::from_str::<EditAction>(&c[1]) {
            return Some(v);
        }
    }
    let inline = Regex::new(r#"(\{[^{}]*?"action"\s*:\s*"[^"]+?"[^{}]*?\})"#).unwrap();
    if let Some(c) = inline.captures(content) {
        if let Ok(v) = serde_json::from_str::<EditAction>(&c[1]) {
            return Some(v);
        }
    }
    None
}

fn parse_source_block(content: &str) -> Option<String> {
    let closed = Regex::new(r"(?s)```(?:python|py|rust|rs|go|ts|tsx|js)\s*\n(.*?)```").unwrap();
    if let Some(c) = closed.captures(content) {
        return Some(c[1].to_string());
    }
    let any_closed = Regex::new(r"(?s)```(\w*)\s*\n(.*?)```").unwrap();
    let mut last_non_json: Option<String> = None;
    for cap in any_closed.captures_iter(content) {
        if !cap[1].eq_ignore_ascii_case("json") {
            last_non_json = Some(cap[2].to_string());
        }
    }
    if let Some(b) = last_non_json {
        return Some(b);
    }
    // Truncation-friendly: opening fence without close.
    let open_only = Regex::new(r"(?s)```(?:python|py|rust|rs)\s*\n(.*)").unwrap();
    if let Some(c) = open_only.captures(content) {
        return Some(c[1].to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rewrite_function_action() {
        let content = r#"```json
{"action": "rewrite_function", "name": "tokenize"}
```

```python
def tokenize(source):
    return []
```"#;
        let r = parse_response(content).unwrap();
        match r.action {
            EditAction::RewriteFunction { name } => assert_eq!(name, "tokenize"),
            other => panic!("got {other:?}"),
        }
        assert!(r.body.contains("def tokenize"));
    }

    #[test]
    fn extracts_patch_lines_action() {
        let content = r#"```json
{"action": "patch_lines", "start": 12, "end": 18}
```

```python
    return 42
```"#;
        let r = parse_response(content).unwrap();
        assert!(matches!(
            r.action,
            EditAction::PatchLines { start: 12, end: 18 }
        ));
    }

    #[test]
    fn handles_unterminated_code_block() {
        let content = r#"```json
{"action": "patch_lines", "start": 89, "end": 100}
```

```python
if c == "<"
"#;
        let r = parse_response(content).unwrap();
        assert!(r.body.contains("if c =="));
    }

    #[test]
    fn returns_none_for_missing_action() {
        assert!(parse_response("no action here").is_none());
    }
}

#[cfg(test)]
mod inference_tests {
    use super::*;

    #[test]
    fn bare_rust_function_block_infers_rewrite() {
        let content = "```rust\npub fn group_anagrams(strs: Vec<String>) -> Vec<Vec<String>> {\n    let mut m = std::collections::HashMap::new();\n    m.insert(1, 2);\n    vec![]\n}\n```";
        let r = parse_response(content).unwrap();
        assert!(r.inferred);
        match r.action {
            EditAction::RewriteFunction { name } => assert_eq!(name, "group_anagrams"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn bare_python_function_block_infers_rewrite() {
        let content = "```python\ndef solve(grid):\n    return []\n```";
        let r = parse_response(content).unwrap();
        assert!(r.inferred);
        match r.action {
            EditAction::RewriteFunction { name } => assert_eq!(name, "solve"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn block_with_imports_infers_write_file() {
        let content = "```rust\nuse std::collections::HashMap;\n\npub fn solve(g: &[Vec<u8>]) -> Option<Vec<(usize, usize)>> {\n    None\n}\n```";
        let r = parse_response(content).unwrap();
        assert!(r.inferred);
        assert!(matches!(r.action, EditAction::WriteFile { path: None }));
    }

    #[test]
    fn multi_function_block_infers_write_file() {
        let content = "```go\nfunc a() {}\n\nfunc b() {}\n```";
        let r = parse_response(content).unwrap();
        assert!(matches!(r.action, EditAction::WriteFile { path: None }));
    }

    #[test]
    fn prose_block_still_fails_parse() {
        let content = "```\nThis is just an explanation of the approach.\n```";
        assert!(parse_response(content).is_none());
    }

    #[test]
    fn explicit_header_is_not_marked_inferred() {
        let content = "```json\n{\"action\": \"rewrite_function\", \"name\": \"f\"}\n```\n```python\ndef f():\n    return 1\n```";
        let r = parse_response(content).unwrap();
        assert!(!r.inferred);
    }
}
