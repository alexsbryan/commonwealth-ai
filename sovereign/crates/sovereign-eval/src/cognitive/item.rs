//! On-disk TOML schema for a cognitive unit-test item.
//!
//! Each item is a single TOML file under
//! `sovereign/inquiries/cognitive/<category>/<id>.toml`. The item
//! probes one bounded competency of the Fast slot in isolation; the
//! harness loads it, renders the prompt with `[[context_blocks]]`
//! substitution, POSTs to `/v1/chat/completions`, and scores the
//! response mechanically per `[scoring].kind`.
//!
//! No judge call. Same property that makes the existing
//! `principle_*.toml` inquiry bank fast.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One TOML item loaded from disk.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Item {
    pub item: ItemMeta,
    pub prompt: Prompt,
    #[serde(default, rename = "context_blocks")]
    pub context_blocks: Vec<ContextBlock>,
    pub scoring: Scoring,
    /// Absolute path the item was loaded from, populated by the loader.
    #[serde(skip)]
    pub source_path: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ItemMeta {
    pub id: String,
    pub title: String,
    pub category: Category,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    SituatingJudgment,
    DecisionQuality,
    HonestyCalibration,
    CodeReasoning,
    CharterSatisfaction,
    ToolUse,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::SituatingJudgment => "situating_judgment",
            Category::DecisionQuality => "decision_quality",
            Category::HonestyCalibration => "honesty_calibration",
            Category::CodeReasoning => "code_reasoning",
            Category::CharterSatisfaction => "charter_satisfaction",
            Category::ToolUse => "tool_use",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "situating_judgment" => Ok(Category::SituatingJudgment),
            "decision_quality" => Ok(Category::DecisionQuality),
            "honesty_calibration" => Ok(Category::HonestyCalibration),
            "code_reasoning" => Ok(Category::CodeReasoning),
            "charter_satisfaction" => Ok(Category::CharterSatisfaction),
            "tool_use" => Ok(Category::ToolUse),
            other => bail!(
                "unknown category `{other}` (valid: situating_judgment, decision_quality, honesty_calibration, code_reasoning, charter_satisfaction, tool_use)"
            ),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Prompt {
    /// Optional system prompt. If absent, the runner supplies a stable
    /// generic system prompt that asks for one JSON object.
    #[serde(default)]
    pub system: Option<String>,
    /// The user-turn prompt template. `{{block_name}}` references
    /// resolve to the matching `[[context_blocks]]` entry; unmatched
    /// references error at render time.
    pub user: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContextBlock {
    pub name: String,
    /// Inline content. Mutually exclusive with `file`.
    #[serde(default)]
    pub content: Option<String>,
    /// File-backed content. Path resolved relative to the workspace
    /// root the harness was invoked from. Mutually exclusive with
    /// `content`.
    #[serde(default)]
    pub file: Option<PathBuf>,
    /// Optional `"start-end"` line range (1-indexed, inclusive).
    /// Ignored when `content` is set.
    #[serde(default)]
    pub lines: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scoring {
    /// Pass if the response (raw text) contains `expected_substring`.
    ExactMatch {
        expected_substring: String,
        #[serde(default)]
        case_sensitive: bool,
    },
    /// Pass if the JSON response's `choice_field` equals
    /// `expected_choice` (string-compared, case-sensitive).
    MultiChoice {
        expected_choice: String,
        #[serde(default = "default_choice_field")]
        choice_field: String,
    },
    /// Pass when the model's stated confidence is well-calibrated
    /// against the claim's known truth value. Reads `confidence_field`
    /// as a number 1-5.
    ///
    /// `expected_truth = true`  → pass when confidence >= `pass_high`
    /// `expected_truth = false` → pass when confidence <= `pass_low`
    Calibration {
        expected_truth: bool,
        #[serde(default = "default_confidence_field")]
        confidence_field: String,
        #[serde(default = "default_pass_high")]
        pass_high: u32,
        #[serde(default = "default_pass_low")]
        pass_low: u32,
    },
    /// Pass when the model picks the right tool (or chain of tools)
    /// from a presented menu. Three sub-modes — exactly ONE must be
    /// set:
    /// - `expected_tool` (+ optional `expected_args`): single-tool call.
    /// - `expected_tool = "none"`: model must signal "no tool needed".
    /// - `expected_sequence`: ordered list of tool names; matches
    ///   the model's `"tools": [...]` array exactly.
    ///
    /// `alternates_ok` lists additional acceptable tool names for
    /// the single-tool mode (e.g., `code_search` instead of `symbols`
    /// for genuinely-ambiguous tasks).
    ToolUse {
        #[serde(default)]
        expected_tool: Option<String>,
        #[serde(default)]
        expected_args: Option<std::collections::BTreeMap<String, String>>,
        /// Fuzzy alternative to `expected_args`. Every listed string
        /// must appear (case-insensitive substring) in the JSON-serialized
        /// `args` object. Use for shell commands and natural-language
        /// queries where flag ordering / phrasing varies but the
        /// load-bearing tokens are stable.
        ///
        /// When set, takes precedence over `expected_args`.
        #[serde(default)]
        must_contain: Option<Vec<String>>,
        #[serde(default)]
        expected_sequence: Option<Vec<String>>,
        #[serde(default)]
        alternates_ok: Option<Vec<String>>,
    },
}

fn default_choice_field() -> String {
    "choice".to_string()
}

fn default_confidence_field() -> String {
    "confidence".to_string()
}

fn default_pass_high() -> u32 {
    4
}

fn default_pass_low() -> u32 {
    2
}

/// Render the user prompt by substituting `{{block_name}}` against
/// the loaded context blocks. Returns the rendered string plus the
/// fully-rendered system prompt (or `None` if no system prompt set).
pub fn render(item: &Item, workspace_root: &Path) -> Result<RenderedPrompt> {
    let mut blocks: BTreeMap<String, String> = BTreeMap::new();
    for b in &item.context_blocks {
        let content = resolve_block(b, workspace_root)
            .with_context(|| format!("resolving context block `{}`", b.name))?;
        blocks.insert(b.name.clone(), content);
    }
    let user = substitute(&item.prompt.user, &blocks)
        .with_context(|| format!("rendering user prompt for item `{}`", item.item.id))?;
    let system = match &item.prompt.system {
        Some(s) => Some(
            substitute(s, &blocks)
                .with_context(|| format!("rendering system prompt for item `{}`", item.item.id))?,
        ),
        None => None,
    };
    Ok(RenderedPrompt { user, system })
}

pub struct RenderedPrompt {
    pub user: String,
    pub system: Option<String>,
}

fn resolve_block(b: &ContextBlock, workspace_root: &Path) -> Result<String> {
    match (&b.content, &b.file) {
        (Some(_), Some(_)) => bail!(
            "context block `{}` declares both `content` and `file`; pick one",
            b.name
        ),
        (Some(c), None) => Ok(c.clone()),
        (None, Some(p)) => {
            let abs = if p.is_absolute() {
                p.clone()
            } else {
                workspace_root.join(p)
            };
            let raw = fs::read_to_string(&abs).with_context(|| {
                format!("reading file block `{}` from {}", b.name, abs.display())
            })?;
            match &b.lines {
                None => Ok(raw),
                Some(range) => slice_lines(&raw, range)
                    .with_context(|| format!("slicing lines for block `{}` ({range})", b.name)),
            }
        }
        (None, None) => bail!(
            "context block `{}` declares neither `content` nor `file`",
            b.name
        ),
    }
}

fn slice_lines(raw: &str, range: &str) -> Result<String> {
    let (start_s, end_s) = range
        .split_once('-')
        .with_context(|| format!("range `{range}` is not `start-end`"))?;
    let start: usize = start_s.trim().parse().context("parsing range start")?;
    let end: usize = end_s.trim().parse().context("parsing range end")?;
    if start == 0 || end < start {
        bail!("invalid range `{range}` (1-indexed inclusive)");
    }
    Ok(raw
        .lines()
        .skip(start - 1)
        .take(end - start + 1)
        .collect::<Vec<_>>()
        .join("\n"))
}

fn substitute(template: &str, blocks: &BTreeMap<String, String>) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(idx) = rest.find("{{") {
        out.push_str(&rest[..idx]);
        rest = &rest[idx + 2..];
        let end = rest
            .find("}}")
            .with_context(|| "unterminated `{{...}}` in template")?;
        let name = rest[..end].trim();
        let value = blocks
            .get(name)
            .with_context(|| format!("template references unknown context block `{name}`"))?;
        out.push_str(value);
        rest = &rest[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Load all `*.toml` items beneath `root`, recursively. Files that
/// fail to parse are surfaced as load errors (loud rather than
/// silent — a malformed item is the same kind of bug as a broken
/// test fixture).
pub fn load_all(root: &Path) -> Result<Vec<Item>> {
    if !root.exists() {
        bail!("cognitive bank root not found: {}", root.display());
    }
    let mut out = Vec::new();
    walk_toml(root, &mut out)?;
    out.sort_by(|a, b| a.item.id.cmp(&b.item.id));
    Ok(out)
}

fn walk_toml(dir: &Path, out: &mut Vec<Item>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk_toml(&path, out)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("toml") {
            let raw =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            let mut item: Item =
                toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
            item.source_path = path;
            out.push(item);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn substitute_replaces_named_blocks() {
        let mut blocks = BTreeMap::new();
        blocks.insert("foo".to_string(), "BAR".to_string());
        let got = substitute("before {{ foo }} after", &blocks).unwrap();
        assert_eq!(got, "before BAR after");
    }

    #[test]
    fn substitute_errors_on_unknown_block() {
        let blocks = BTreeMap::new();
        let err = substitute("{{missing}}", &blocks).unwrap_err();
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn slice_lines_inclusive_range() {
        let raw = "one\ntwo\nthree\nfour";
        let got = slice_lines(raw, "2-3").unwrap();
        assert_eq!(got, "two\nthree");
    }

    #[test]
    fn category_round_trip() {
        for c in [
            Category::SituatingJudgment,
            Category::DecisionQuality,
            Category::HonestyCalibration,
            Category::CodeReasoning,
            Category::CharterSatisfaction,
        ] {
            assert_eq!(Category::parse(c.as_str()).unwrap(), c);
        }
    }
}
