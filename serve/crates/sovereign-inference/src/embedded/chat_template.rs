// SPDX-License-Identifier: AGPL-3.0-or-later
//! The model's own chat template, rendered in Rust with `minijinja`.
//!
//! One renderer for every caller: the single-turn engine path renders
//! `[system, user]`, and an OpenAI conversation renders its whole message
//! list plus its tools, so the template itself writes the model-native
//! tool instructions and history. The goal is the prompt llama-server
//! builds for the same request, byte for byte (fixtures under
//! `tests/fixtures/chat_template/`, expected text from llama-server's
//! `/apply-template`). No llama.cpp code is involved, so re-vendoring
//! llama.cpp does not touch this path.

use serde_json::Value as Json;
use sovereign_contracts::error::Error;
use sovereign_contracts::Result;

/// What a template render reads.
pub(crate) struct ChatTemplateInput<'a> {
    /// OpenAI chat-completions messages, as the client sent them.
    pub messages: &'a [Json],
    /// OpenAI tool definitions; `None` or empty renders no tool block.
    pub tools: Option<&'a [Json]>,
    pub enable_thinking: bool,
    pub add_generation_prompt: bool,
}

/// Render `template` (a GGUF `tokenizer.chat_template`) over `input`.
pub(crate) fn render(template: &str, input: &ChatTemplateInput<'_>) -> Result<String> {
    use minijinja::{context, Value};

    let env = environment(template)?;
    let tmpl = env
        .get_template("chat")
        .map_err(|e| Error::Inference(format!("minijinja: load chat template: {e}")))?;
    let messages: Vec<Value> = template_messages(input.messages)
        .iter()
        .map(Value::from_serialize)
        .collect();
    let tools: Option<Vec<Value>> = match input.tools.filter(|t| !t.is_empty()) {
        Some(t) => Some(
            template_tools(t)?
                .iter()
                .map(Value::from_serialize)
                .collect(),
        ),
        None => None,
    };
    // Variables every reasonable chat template touches. Templates that
    // reference unknown vars produce empty strings, which is the same
    // behaviour as the llama.cpp path.
    let ctx = context! {
        messages => messages,
        tools => tools,
        add_generation_prompt => input.add_generation_prompt,
        enable_thinking => input.enable_thinking,
        bos_token => "",
        eos_token => "",
        // Qwen3-family hint surfaced via a context variable on some
        // template revisions. Most templates inspect the system
        // message text instead.
        thinking_mode => if input.enable_thinking { "think" } else { "no_think" },
    };
    tmpl.render(ctx)
        .map_err(|e| Error::Inference(format!("minijinja: render chat template: {e}")))
}

/// The messages as llama.cpp hands them to a template
/// (`common_chat_msg::to_json_oaicompat`): a missing or null `content` is
/// the empty string, and a tool call's `arguments`, a JSON string on the
/// wire, is the object it encodes — templates iterate it as a mapping and
/// some refuse a string outright.
fn template_messages(messages: &[Json]) -> Vec<Json> {
    messages
        .iter()
        .map(|m| {
            let mut m = m.clone();
            let Some(obj) = m.as_object_mut() else {
                return m;
            };
            if obj.get("content").is_none_or(Json::is_null) {
                obj.insert("content".into(), Json::String(String::new()));
            }
            if let Some(calls) = obj.get_mut("tool_calls").and_then(Json::as_array_mut) {
                for call in calls {
                    if let Some(args) = call.pointer_mut("/function/arguments") {
                        if let Some(parsed) = args
                            .as_str()
                            .and_then(|s| serde_json::from_str::<Json>(s).ok())
                        {
                            *args = parsed;
                        }
                    }
                }
            }
            m
        })
        .collect()
}

/// The tools as llama.cpp hands them to a template: validated as
/// `common_chat_tools_parse_oaicompat` validates them, then rebuilt from name,
/// description and parameters alone (`common_chat_tools_to_json_oaicompat`),
/// so client extras such as `"strict"` never reach the prompt. A missing
/// description is `""` and missing parameters `{}`, as there; a tool that is
/// not `type: "function"`, has no `function`, or has no name is refused, as
/// there, rather than rendered under an empty name.
fn template_tools(tools: &[Json]) -> Result<Vec<Json>> {
    tools
        .iter()
        .map(|t| {
            let refuse = |why: &str| Error::InvalidInput(format!("tools: {why}: {t}"));
            if t.get("type").and_then(Json::as_str) != Some("function") {
                return Err(refuse("tool type must be \"function\""));
            }
            let f = t.get("function").ok_or_else(|| refuse("missing tool function"))?;
            let name = f
                .get("name")
                .and_then(Json::as_str)
                .ok_or_else(|| refuse("missing function name"))?;
            Ok(serde_json::json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": f.get("description").cloned().unwrap_or(Json::String(String::new())),
                    "parameters": f.get("parameters").cloned().unwrap_or_else(|| serde_json::json!({})),
                },
            }))
        })
        .collect()
}

fn environment(template: &str) -> Result<minijinja::Environment<'_>> {
    let mut env = minijinja::Environment::new();
    // Match Hugging Face's `Jinja2 Templates` behaviour: keep the
    // raise_exception filter available — some templates call it
    // (`{{ raise_exception("…") }}`) to halt on bad input.
    env.add_function(
        "raise_exception",
        |msg: String| -> std::result::Result<String, minijinja::Error> {
            Err(minijinja::Error::new(
                minijinja::ErrorKind::InvalidOperation,
                msg,
            ))
        },
    );
    // Python-compat method shim. HF templates routinely call
    // `.get(key)`, `.get(key, default)`, `.split(sep)`,
    // `.startswith(prefix)`, `.endswith(suffix)`, `.upper()`,
    // `.lower()`, `.strip()` — methods that exist on Python's
    // `dict`/`str`/`list` but aren't in stock minijinja. Without
    // this shim, Gemma 4's template (which calls
    // `message.get('reasoning')`, `message.get('tool_calls')`,
    // `value['type'] | upper`, `part.split('<|channel>')`, …) fails
    // at the first unknown method and we fall through to plain-text
    // concat. The pycompat surface in `minijinja-contrib` would
    // also do this, but pulling in another workspace dep for a
    // half-dozen methods is excessive — handle them inline.
    env.set_unknown_method_callback(|_state, value, method, args| {
        use minijinja::value::{from_args, ValueKind};
        use minijinja::{Error, ErrorKind, Value};
        match method {
            "get" => {
                // dict.get(key) or dict.get(key, default)
                if value.kind() != ValueKind::Map {
                    return Err(Error::from(ErrorKind::UnknownMethod));
                }
                let (key, default): (Value, Option<Value>) = from_args(args)?;
                let key_str: String = key
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| key.to_string());
                match value.get_attr(&key_str) {
                    Ok(v) if !v.is_undefined() => Ok(v),
                    _ => Ok(default.unwrap_or(Value::from(()))),
                }
            }
            "split" => {
                // str.split(sep) — sep is required in HF templates we've
                // seen (no zero-arg whitespace split path needed yet).
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "split on non-string")
                })?;
                let (sep,): (String,) = from_args(args)?;
                let parts: Vec<Value> = s.split(&sep).map(|p| Value::from(p.to_string())).collect();
                Ok(Value::from(parts))
            }
            "startswith" => {
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "startswith on non-string")
                })?;
                let (prefix,): (String,) = from_args(args)?;
                Ok(Value::from(s.starts_with(&prefix)))
            }
            "endswith" => {
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "endswith on non-string")
                })?;
                let (suffix,): (String,) = from_args(args)?;
                Ok(Value::from(s.ends_with(&suffix)))
            }
            "upper" => {
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "upper on non-string")
                })?;
                let _: () = from_args(args)?;
                Ok(Value::from(s.to_uppercase()))
            }
            "lower" => {
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "lower on non-string")
                })?;
                let _: () = from_args(args)?;
                Ok(Value::from(s.to_lowercase()))
            }
            "strip" => {
                let s = value.as_str().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidOperation, "strip on non-string")
                })?;
                let _: () = from_args(args)?;
                Ok(Value::from(s.trim().to_string()))
            }
            _ => Err(Error::from(ErrorKind::UnknownMethod)),
        }
    });
    env.add_filter("tojson", tojson);
    env.add_template("chat", template)
        .map_err(|e| Error::Inference(format!("minijinja: compile chat template: {e}")))?;
    Ok(env)
}

/// `tojson` as llama.cpp's Jinja runtime writes it (common/jinja/value.cpp
/// `tojson` + `value_to_json_internal`), not as minijinja's own `json`
/// feature does (that one HTML-escapes `<`, `>`, `&`, `'`): keys in
/// insertion order, `", "` / `": "` separators, raw UTF-8 unless
/// `ensure_ascii=true`, and floats as a C++ ostream prints them.
fn tojson(
    value: minijinja::Value,
    kwargs: minijinja::value::Kwargs,
) -> std::result::Result<String, minijinja::Error> {
    let indent: Option<i64> = kwargs.get("indent")?;
    let ensure_ascii: Option<bool> = kwargs.get("ensure_ascii")?;
    let separators: Option<Vec<String>> = kwargs.get("separators")?;
    let sort_keys: Option<bool> = kwargs.get("sort_keys")?;
    kwargs.assert_all_used()?;
    if sort_keys == Some(true) {
        return Err(minijinja::Error::new(
            minijinja::ErrorKind::InvalidOperation,
            "tojson sort_keys=true is not supported (llama.cpp refuses it too)",
        ));
    }
    let indent = indent.unwrap_or(-1);
    let separators = separators.unwrap_or_default();
    let item_sep = separators
        .first()
        .cloned()
        .unwrap_or_else(|| if indent < 0 { ", " } else { "," }.to_string());
    let key_sep = separators
        .get(1)
        .cloned()
        .unwrap_or_else(|| ": ".to_string());
    let mut out = String::new();
    write_json(&mut out, &value, 0, indent, &item_sep, &key_sep)?;
    if ensure_ascii == Some(true) {
        out = ascii_escaped(&out);
    }
    Ok(out)
}

fn write_json(
    out: &mut String,
    value: &minijinja::Value,
    level: usize,
    indent: i64,
    item_sep: &str,
    key_sep: &str,
) -> std::result::Result<(), minijinja::Error> {
    use minijinja::value::ValueKind;
    let pad = |lvl: usize| " ".repeat(if indent > 0 { lvl * indent as usize } else { 0 });
    let newline = if indent >= 0 { "\n" } else { "" };
    match value.kind() {
        ValueKind::Undefined | ValueKind::None => out.push_str("null"),
        ValueKind::Bool => out.push_str(if value.is_true() { "true" } else { "false" }),
        ValueKind::Number if value.is_integer() => out.push_str(&value.to_string()),
        ValueKind::Number => out.push_str(&cpp_ostream_float(f64::try_from(value.clone())?)),
        ValueKind::String => write_json_string(out, value.as_str().unwrap_or_default()),
        ValueKind::Seq | ValueKind::Iterable => {
            let items: Vec<minijinja::Value> = value.try_iter()?.collect();
            out.push('[');
            if !items.is_empty() {
                out.push_str(newline);
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&pad(level + 1));
                    write_json(out, item, level + 1, indent, item_sep, key_sep)?;
                    if i + 1 < items.len() {
                        out.push_str(item_sep);
                    }
                    out.push_str(newline);
                }
                out.push_str(&pad(level));
            }
            out.push(']');
        }
        ValueKind::Map => {
            let keys: Vec<minijinja::Value> = value.try_iter()?.collect();
            out.push('{');
            if !keys.is_empty() {
                out.push_str(newline);
                for (i, key) in keys.iter().enumerate() {
                    out.push_str(&pad(level + 1));
                    write_json_string(out, &key.to_string());
                    out.push_str(key_sep);
                    write_json(
                        out,
                        &value.get_item(key)?,
                        level + 1,
                        indent,
                        item_sep,
                        key_sep,
                    )?;
                    if i + 1 < keys.len() {
                        out.push_str(item_sep);
                    }
                    out.push_str(newline);
                }
                out.push_str(&pad(level));
            }
            out.push('}');
        }
        _ => out.push_str("null"),
    }
    Ok(())
}

fn write_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `\uXXXX` for every non-ASCII character (surrogate pairs above the BMP),
/// as llama.cpp's `json_ensure_ascii_preserving_format` does.
fn ascii_escaped(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

/// A float as `std::ostream << double` prints it with default flags:
/// `%g` at precision 6 (`0.95`, `1`, `1e+06`, `1.5e-07`).
fn cpp_ostream_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.into();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0" } else { "0" }.into();
    }
    let sci = format!("{f:.5e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let trim = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if (-4..6).contains(&exp) {
        let decimals = (5 - exp).max(0) as usize;
        trim(&format!("{f:.decimals$}"))
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa), exp.abs())
    }
}

#[cfg(test)]
mod llama_server_parity {
    //! Renders captured agent requests and compares them, byte for byte,
    //! with what llama-server's `/apply-template` built for the same body
    //! (pin 035e22731, Qwen3.8-27B's own template). Regenerate the expected
    //! text with llama-server, never by hand.
    use super::*;
    use std::path::PathBuf;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/chat_template/qwen3.8-27b")
    }

    fn rendered(case: &str, enable_thinking: bool) -> (String, String) {
        let dir = fixture_dir();
        let template = std::fs::read_to_string(dir.join("template.jinja")).unwrap();
        let req: Json = serde_json::from_str(
            &std::fs::read_to_string(dir.join(format!("{case}.request.json"))).unwrap(),
        )
        .unwrap();
        let label = if enable_thinking {
            "think-on"
        } else {
            "think-off"
        };
        let expected =
            std::fs::read_to_string(dir.join(format!("{case}.{label}.prompt.txt"))).unwrap();
        let messages = req["messages"].as_array().unwrap();
        let tools = req.get("tools").and_then(Json::as_array).map(Vec::as_slice);
        let got = render(
            &template,
            &ChatTemplateInput {
                messages,
                tools,
                enable_thinking,
                add_generation_prompt: true,
            },
        )
        .unwrap();
        (got, expected)
    }

    fn assert_same(case: &str, enable_thinking: bool) {
        let (got, expected) = rendered(case, enable_thinking);
        if got != expected {
            let at = got
                .bytes()
                .zip(expected.bytes())
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| got.len().min(expected.len()));
            let lo = at.saturating_sub(120);
            panic!(
                "{case} (thinking {enable_thinking}): first difference at byte {at} of {} vs {}\n\
                 ours:         {:?}\nllama-server: {:?}",
                got.len(),
                expected.len(),
                &got[lo..(at + 120).min(got.len())],
                &expected[lo..(at + 120).min(expected.len())],
            );
        }
    }

    #[test]
    fn pi_first_turn_matches_llama_server() {
        assert_same("pi-turn1", true);
        assert_same("pi-turn1", false);
    }

    #[test]
    fn pi_mid_session_with_tool_results_matches_llama_server() {
        assert_same("pi-midsession", true);
        assert_same("pi-midsession", false);
    }

    #[test]
    fn reasoning_history_matches_llama_server() {
        assert_same("pi-midsession-reasoning", true);
        assert_same("pi-midsession-reasoning", false);
    }

    /// Floats as C++ prints them (`1e+06`, `123457`, `1e-05`), non-ASCII
    /// and control characters, a client `"strict"` that must not render,
    /// and tool-call arguments with every JSON kind.
    #[test]
    fn edge_values_match_llama_server() {
        assert_same("edge-values", true);
        assert_same("edge-values", false);
    }

    /// llama-server answers a tool with no name with an error, not a prompt
    /// that offers a tool called "".
    #[test]
    fn a_tool_without_a_name_is_refused() {
        let template = std::fs::read_to_string(fixture_dir().join("template.jinja")).unwrap();
        let messages = [serde_json::json!({"role": "user", "content": "hi"})];
        let tools =
            [serde_json::json!({"type": "function", "function": {"description": "nameless"}})];
        let err = render(
            &template,
            &ChatTemplateInput {
                messages: &messages,
                tools: Some(&tools),
                enable_thinking: false,
                add_generation_prompt: true,
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::InvalidInput(ref m) if m.contains("missing function name")),
            "{err:?}"
        );
    }

    #[test]
    fn opencode_second_turn_matches_llama_server() {
        assert_same("opencode-turn2", true);
        assert_same("opencode-turn2", false);
    }
}
