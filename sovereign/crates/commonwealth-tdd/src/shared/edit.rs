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
}

pub fn parse_response(content: &str) -> Option<ParsedResponse> {
    let action = parse_action_json(content)?;
    let body = parse_source_block(content)?;
    Some(ParsedResponse { action, body })
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
