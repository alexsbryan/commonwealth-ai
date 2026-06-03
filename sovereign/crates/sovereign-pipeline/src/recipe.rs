//! Recipe TOML — the per-corpus surface of the pipeline tool.
//!
//! A recipe declares **what** the worklist contains and **how** each
//! unit is enriched. Everything else (claim/ack, retry, dashboard,
//! pause/resume) is recipe-agnostic and lives in the driver.
//!
//! ## Shape
//!
//! ```toml
//! [recipe]
//! id      = "sep-core-v1"   # primary key for the worklist DB
//! version = 1
//!
//! [source]
//! type = "slug_list"
//! path = "sep_slugs.txt"   # operator-supplied slug list, relative to CWD
//!
//! [enrich]
//! # `{key}` is replaced with the work-unit key. The command is
//! # executed via shell; non-zero exit means failure.
//! command       = "sovereign enrich sep-ingest {key}"
//! timeout_secs  = 1800
//!
//! [[enrich.failure_classifier]]
//! bucket          = "vram_thrash"
//! stderr_pattern  = "out of memory|VRAM|cudaMalloc"
//!
//! [[enrich.failure_classifier]]
//! bucket          = "refused"
//! stderr_pattern  = "Connection refused|connect failed"
//!
//! [dispatch]
//! max_attempts = 3
//! lease_secs   = 1800
//! concurrency  = 6
//!
//! [schedule]
//! active_hours = "22:00-06:00"   # optional; 24/7 if omitted
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RecipeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("recipe `{0}` is missing the required `{1}` field")]
    Missing(String, &'static str),
    #[error("recipe `{0}` is invalid: {1}")]
    Invalid(String, String),
}

pub type Result<T> = std::result::Result<T, RecipeError>;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Recipe {
    pub recipe: RecipeMeta,
    pub source: Source,
    pub enrich: Enrich,
    #[serde(default)]
    pub dispatch: Dispatch,
    #[serde(default)]
    pub schedule: Option<Schedule>,
    /// The directory the recipe was loaded from. Used to resolve
    /// relative `source.path` values. `None` if the recipe was
    /// parsed from a string (tests).
    #[serde(skip)]
    pub base_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RecipeMeta {
    pub id: String,
    #[serde(default = "default_version")]
    pub version: u32,
}

fn default_version() -> u32 {
    1
}

/// Where the keys come from.
///
/// The `command` variant is the "batteries-included" default for
/// recipes that target a known corpus — the command's job is to
/// enumerate every available key (one per line on stdout), so the
/// recipe works out of the box without the user shipping a slug list.
/// Failure is treated as a hard error at seed time with a clear
/// message; the operator can supply `--slugs <file>` or `--key
/// <slug>` to bypass entirely.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    /// Path to a newline-separated list of keys. Blank lines and
    /// `#`-prefixed comments are ignored.
    SlugList { path: PathBuf },
    /// Shell command whose stdout is treated as a newline-separated
    /// list of keys. Run via `/bin/sh -c` from the recipe's dir.
    Command { command: String },
    /// Inline key list (handy for tests and tiny pipelines).
    Inline { keys: Vec<String> },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Enrich {
    /// Shell command to run per unit. `{key}` is replaced with the
    /// work-unit key before exec. Run via `/bin/sh -c`.
    pub command: String,
    /// Hard timeout per attempt. Zero means "no timeout".
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Custom failure-bucket rules. The driver also applies built-in
    /// rules (exit 124 → `timeout`, etc.) — these are additive.
    #[serde(default)]
    pub failure_classifier: Vec<ClassifierRule>,
}

fn default_timeout() -> u64 {
    1800
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClassifierRule {
    pub bucket: String,
    /// Regex matched against the **combined** stdout+stderr of the
    /// failed command. First matching rule wins.
    pub stderr_pattern: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Dispatch {
    /// Max attempts per unit before it lands in `failed`.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// Lease duration. If the driver dies, another driver run will
    /// sweep the abandoned claim after this many seconds.
    #[serde(default = "default_lease_secs")]
    pub lease_secs: u32,
    /// Parallel in-flight units per driver.
    #[serde(default = "default_concurrency")]
    pub concurrency: u32,
}

impl Default for Dispatch {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            lease_secs: default_lease_secs(),
            concurrency: default_concurrency(),
        }
    }
}

fn default_max_attempts() -> u32 {
    3
}
fn default_lease_secs() -> u32 {
    1800
}
fn default_concurrency() -> u32 {
    4
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Schedule {
    /// Active window in `HH:MM-HH:MM` (local time). When the current
    /// time is outside this window, the driver stops claiming new
    /// work, lets in-flight drain, and sleeps until the next active
    /// window opens.
    pub active_hours: String,
}

impl Recipe {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)?;
        let mut recipe: Recipe = toml::from_str(&text)?;
        recipe.base_dir = path.parent().map(|p| p.to_path_buf());
        recipe.validate()?;
        Ok(recipe)
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        let recipe: Recipe = toml::from_str(text)?;
        recipe.validate()?;
        Ok(recipe)
    }

    fn validate(&self) -> Result<()> {
        if self.recipe.id.is_empty() {
            return Err(RecipeError::Missing(self.recipe.id.clone(), "recipe.id"));
        }
        if !self.enrich.command.contains("{key}") {
            return Err(RecipeError::Invalid(
                self.recipe.id.clone(),
                "enrich.command must contain `{key}`".into(),
            ));
        }
        if let Some(sched) = &self.schedule {
            parse_window(&sched.active_hours).map_err(|e| {
                RecipeError::Invalid(
                    self.recipe.id.clone(),
                    format!("schedule.active_hours: {e}"),
                )
            })?;
        }
        for rule in &self.enrich.failure_classifier {
            regex::Regex::new(&rule.stderr_pattern).map_err(|e| {
                RecipeError::Invalid(
                    self.recipe.id.clone(),
                    format!(
                        "enrich.failure_classifier `{}` has bad regex: {e}",
                        rule.bucket
                    ),
                )
            })?;
        }
        Ok(())
    }

    /// Materialize the key list described by `source`.
    ///
    /// Relative paths are resolved against `base_dir` (the directory
    /// the recipe was loaded from), so `sovereign pipeline run
    /// /abs/path/recipe.toml` from any cwd produces the same result.
    pub fn load_keys(&self) -> Result<Vec<String>> {
        match &self.source {
            Source::SlugList { path } => {
                let resolved = self.resolve_path(path);
                let text = std::fs::read_to_string(&resolved).map_err(|e| {
                    RecipeError::Invalid(
                        self.recipe.id.clone(),
                        format!("could not read slug list `{}`: {e}", resolved.display()),
                    )
                })?;
                Ok(parse_key_lines(&text))
            }
            Source::Command { command } => {
                let mut cmd = std::process::Command::new("/bin/sh");
                cmd.arg("-c").arg(command);
                if let Some(base) = &self.base_dir {
                    cmd.current_dir(base);
                }
                let out = cmd.output().map_err(|e| {
                    RecipeError::Invalid(
                        self.recipe.id.clone(),
                        format!("source command spawn failed (`{command}`): {e}"),
                    )
                })?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Err(RecipeError::Invalid(
                        self.recipe.id.clone(),
                        format!(
                            "source command failed (`{command}`, exit {:?}):\n{stderr}\n\
                             hint: pass `--slugs <file>` or `--key <slug>` to bypass.",
                            out.status.code()
                        ),
                    ));
                }
                let stdout = String::from_utf8_lossy(&out.stdout);
                Ok(parse_key_lines(&stdout))
            }
            Source::Inline { keys } => Ok(keys.clone()),
        }
    }

    fn resolve_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else if let Some(base) = &self.base_dir {
            base.join(path)
        } else {
            path.to_path_buf()
        }
    }
}

/// One half-open hour-minute window. End is exclusive; if end ≤ start
/// the window wraps midnight (e.g. `22:00-06:00`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start_minutes: u16,
    pub end_minutes: u16,
}

impl Window {
    pub fn contains(&self, hour: u8, minute: u8) -> bool {
        let m = hour as u16 * 60 + minute as u16;
        if self.start_minutes <= self.end_minutes {
            m >= self.start_minutes && m < self.end_minutes
        } else {
            // wraps midnight
            m >= self.start_minutes || m < self.end_minutes
        }
    }
}

/// Parse newline-separated keys. Blanks and `#`-prefixed lines are
/// dropped, leading/trailing whitespace is trimmed.
pub fn parse_key_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect()
}

pub fn parse_window(s: &str) -> std::result::Result<Window, String> {
    let (start, end) = s
        .split_once('-')
        .ok_or_else(|| format!("expected `HH:MM-HH:MM`, got `{s}`"))?;
    let (sh, sm) = parse_hm(start)?;
    let (eh, em) = parse_hm(end)?;
    Ok(Window {
        start_minutes: sh as u16 * 60 + sm as u16,
        end_minutes: eh as u16 * 60 + em as u16,
    })
}

fn parse_hm(s: &str) -> std::result::Result<(u8, u8), String> {
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| format!("expected `HH:MM`, got `{s}`"))?;
    let h: u8 = h.parse().map_err(|_| format!("bad hour `{h}`"))?;
    let m: u8 = m.parse().map_err(|_| format!("bad minute `{m}`"))?;
    if h > 23 || m > 59 {
        return Err(format!("out-of-range time `{s}`"));
    }
    Ok((h, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
[recipe]
id = "test-v1"

[source]
type = "inline"
keys = ["a", "b", "c"]

[enrich]
command = "echo {key}"
"#;

    #[test]
    fn minimal_recipe_parses() {
        let r = Recipe::from_toml(MINIMAL).unwrap();
        assert_eq!(r.recipe.id, "test-v1");
        assert_eq!(r.dispatch.concurrency, default_concurrency());
        let keys = r.load_keys().unwrap();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn missing_key_marker_is_rejected() {
        let bad = MINIMAL.replace("echo {key}", "echo hello");
        let err = Recipe::from_toml(&bad).unwrap_err();
        assert!(format!("{err}").contains("{key}"));
    }

    #[test]
    fn bad_regex_rejected() {
        let bad = format!(
            "{MINIMAL}\n[[enrich.failure_classifier]]\nbucket = \"x\"\nstderr_pattern = \"(unclosed\""
        );
        let err = Recipe::from_toml(&bad).unwrap_err();
        assert!(format!("{err}").contains("bad regex"));
    }

    #[test]
    fn schedule_parses_and_validates() {
        let s = format!("{MINIMAL}\n[schedule]\nactive_hours = \"22:00-06:00\"");
        let r = Recipe::from_toml(&s).unwrap();
        let w = parse_window(&r.schedule.unwrap().active_hours).unwrap();
        assert!(w.contains(23, 30));
        assert!(w.contains(2, 0));
        assert!(!w.contains(12, 0));
    }

    #[test]
    fn schedule_non_wrapping_window() {
        let w = parse_window("09:00-17:00").unwrap();
        assert!(w.contains(9, 0));
        assert!(w.contains(16, 59));
        assert!(!w.contains(17, 0));
        assert!(!w.contains(8, 59));
    }

    #[test]
    fn invalid_schedule_rejected() {
        let s = format!("{MINIMAL}\n[schedule]\nactive_hours = \"25:00-08:00\"");
        let err = Recipe::from_toml(&s).unwrap_err();
        assert!(format!("{err}").contains("schedule.active_hours"));
    }

    #[test]
    fn command_source_runs_and_parses_lines() {
        let toml = r#"
[recipe]
id = "cmd-test"

[source]
type = "command"
command = "printf 'alpha\nbeta\n\n# skip me\ngamma\n'"

[enrich]
command = "echo {key}"
"#;
        let r = Recipe::from_toml(toml).unwrap();
        assert_eq!(r.load_keys().unwrap(), vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn command_source_failure_surfaces_clearly() {
        let toml = r#"
[recipe]
id = "cmd-fail-test"

[source]
type = "command"
command = "echo nope >&2; exit 7"

[enrich]
command = "echo {key}"
"#;
        let r = Recipe::from_toml(toml).unwrap();
        let err = r.load_keys().unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("exit 7") || msg.contains("Some(7)"),
            "got: {msg}"
        );
        assert!(
            msg.contains("--slugs") || msg.contains("--key"),
            "got: {msg}"
        );
    }

    #[test]
    fn slug_list_source_strips_comments_and_blanks() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("slugs.txt");
        std::fs::write(&p, "alpha\n\n# beta\ngamma\n  delta  \n").unwrap();
        let toml = format!(
            "[recipe]\nid = \"t\"\n\n[source]\ntype = \"slug_list\"\npath = \"{}\"\n\n[enrich]\ncommand = \"echo {{key}}\"\n",
            p.display()
        );
        let r = Recipe::from_toml(&toml).unwrap();
        assert_eq!(r.load_keys().unwrap(), vec!["alpha", "gamma", "delta"]);
    }
}
