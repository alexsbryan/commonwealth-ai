//! Tool-profile registry: a named subset of tool names that filters
//! incoming `request.tools[]` before the chat-completion request
//! reaches the model.
//!
//! Why this exists. The 2026-05-08 measurement run showed that
//! opencode ships ~27 tool descriptions (~52 KB / ~13 K tokens) on
//! every chat-completion turn, while the actual workflow uses 3–5
//! of them. Tool descriptions account for ~64 % of the prompt;
//! prefill cost on a 30 B model at Apple Metal's ~300 tok/s prefill
//! is ~67 s for that share alone. Trimming the tool list to the
//! tools a workflow *actually* uses cuts the prefill time
//! proportionally — measured ~45 s saved per turn for a Rust
//! authoring loop that needs only `write`/`edit`/`bash`.
//!
//! ## Design (per ARCH_PRINCIPLES § 4 / § 5)
//!
//! `ToolProfileRegistry` is a small immutable collection: a
//! `default` profile name plus a `HashMap<String, Profile>`. Loaded
//! once per daemon process from `~/.sovereign/tool_profiles.toml`.
//! When the file is missing, the registry contains exactly one
//! profile — `permissive`, which allows all tools — so the
//! disabled-default path matches today's daemon bit-for-bit.
//!
//! `Profile::filter` is the contract. It takes
//! `&mut Option<Vec<ToolDefinition>>` and trims it in place. A
//! wildcard profile is a no-op; a named-list profile retains only
//! tools whose names are in the list. Trimming is name-based
//! (string-equal); we don't try to do prefix-matching or globbing
//! today — adding either changes the schema, not the call site.
//!
//! ## Activation surface (single layer in v1)
//!
//! v1 supports per-request selection only, via the
//! `X-Sovereign-Tool-Profile: <name>` HTTP header. The route handler
//! copies the header value into `ChatCompletionRequest::tool_profile`,
//! and the inference adapter passes it to the registry's `resolve`.
//! Unknown profile names fall back to the registry's `default` and
//! log a warning so a misspelled header is visible.
//!
//! Future extensions (admin endpoint for session-wide switch, env
//! override, opencode-config sticky binding) plug into the same
//! `resolve` call without changing the registry shape. Out of scope
//! for v1 — start with the smallest activation surface that lets us
//! measure the trim end-to-end.
//!
//! ## What this is NOT
//!
//! - It does NOT control the daemon's own MCP tool surface (the
//!   ~26 server-side tools registered for MCP clients). Those
//!   never enter the inference prompt and have a different
//!   lifecycle.
//! - It does NOT modify the model's understanding of which tools
//!   exist — the model only sees what's in the (filtered) `tools[]`
//!   field, by definition.
//! - It does NOT (yet) trim tool-name mentions from the system
//!   prompt. If opencode's prompt says "use the `read` tool to ..."
//!   and the `read` tool is filtered out, the model will see the
//!   instruction without a way to satisfy it. Empirically opencode
//!   handles this gracefully (tries other tools, retries) but it's
//!   a sibling concern documented at the call site.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use commonwealth_api::openai_types::{ChatCompletionRequest, ToolDefinition};
use serde::Deserialize;

/// One named profile. Either allows all tools (wildcard) or a
/// specific list. Stored in TOML as either:
///
///   ```toml
///   [profile.permissive]
///   allow_tools = "*"
///   ```
///
/// or:
///
///   ```toml
///   [profile.rust-edit]
///   allow_tools = ["write", "edit", "bash"]
///   ```
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub description: Option<String>,
    pub allow: AllowList,
}

#[derive(Debug, Clone)]
pub enum AllowList {
    /// Allow every tool (no-op filter).
    Wildcard,
    /// Allow only tools whose name is in this set. Empty set means
    /// "no tools" — the model will receive `tools=[]`, which
    /// effectively disables tool-calling for the request. Useful
    /// for read-only chat profiles.
    Names(HashSet<String>),
}

impl Profile {
    /// Trim a tool list in place, retaining only tools allowed by
    /// this profile. Returns the count of dropped entries (0 when
    /// wildcard or when nothing was filtered).
    pub fn filter(&self, tools: &mut Option<Vec<ToolDefinition>>) -> usize {
        let Some(list) = tools.as_mut() else {
            return 0;
        };
        match &self.allow {
            AllowList::Wildcard => 0,
            AllowList::Names(names) => {
                let before = list.len();
                list.retain(|t| names.contains(&t.function.name));
                before - list.len()
            }
        }
    }
}

/// In-memory registry. Either built from a TOML file or constructed
/// directly from defaults. Kept immutable after construction so the
/// shared singleton needs only `OnceLock`.
#[derive(Debug, Clone)]
pub struct ToolProfileRegistry {
    default_name: String,
    profiles: HashMap<String, Profile>,
}

impl ToolProfileRegistry {
    /// Registry containing only the `permissive` profile. Used as
    /// the fallback when no `tool_profiles.toml` is present so
    /// daemons that haven't been configured behave exactly as
    /// today's (no filtering).
    pub fn allow_all() -> Self {
        let permissive = Profile {
            name: "permissive".into(),
            description: Some("All tools allowed (default)".into()),
            allow: AllowList::Wildcard,
        };
        let mut profiles = HashMap::new();
        profiles.insert("permissive".into(), permissive);
        Self {
            default_name: "permissive".into(),
            profiles,
        }
    }

    /// Build from a parsed TOML body. Any profile that names
    /// `allow_tools = "*"` becomes Wildcard; arrays become Names.
    /// The `default` field must reference a profile that exists in
    /// the file; if it doesn't, fall back to `permissive` and log.
    /// `permissive` is always available even if the file doesn't
    /// declare it.
    pub fn from_toml(body: &str) -> Result<Self, ToolProfileError> {
        let parsed: TomlRoot = toml::from_str(body)
            .map_err(|e| ToolProfileError::Toml(e.to_string()))?;
        let mut profiles = HashMap::new();
        // Always synthesise a permissive entry first so a
        // misconfigured file still has a safe fallback.
        profiles.insert(
            "permissive".into(),
            Profile {
                name: "permissive".into(),
                description: Some("All tools allowed (synthesised default)".into()),
                allow: AllowList::Wildcard,
            },
        );
        if let Some(map) = parsed.profile {
            for (name, raw) in map {
                let allow = match raw.allow_tools {
                    AllowTomlValue::Wildcard(s) if s == "*" => AllowList::Wildcard,
                    AllowTomlValue::Wildcard(other) => {
                        return Err(ToolProfileError::BadAllow(format!(
                            "profile {name}: allow_tools string must be \"*\", got {other:?}"
                        )));
                    }
                    AllowTomlValue::List(v) => AllowList::Names(v.into_iter().collect()),
                };
                profiles.insert(
                    name.clone(),
                    Profile {
                        name,
                        description: raw.description,
                        allow,
                    },
                );
            }
        }
        let default_name = parsed.default.unwrap_or_else(|| "permissive".into());
        if !profiles.contains_key(&default_name) {
            tracing::warn!(
                bad_default = %default_name,
                "tool_profile: configured default name does not match any profile; \
                 falling back to permissive"
            );
            return Ok(Self {
                default_name: "permissive".into(),
                profiles,
            });
        }
        Ok(Self {
            default_name,
            profiles,
        })
    }

    /// Convenience: read TOML from a path. Missing file returns
    /// `allow_all`. Read errors propagate.
    pub fn from_path(path: &Path) -> Result<Self, ToolProfileError> {
        match std::fs::read_to_string(path) {
            Ok(body) => Self::from_toml(&body),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::allow_all())
            }
            Err(e) => Err(ToolProfileError::Io(e.to_string())),
        }
    }

    /// Resolve a profile by name. `None` falls back to the registry
    /// default. Unknown names log and fall back to default — visible
    /// in logs as a misconfiguration signal.
    pub fn resolve(&self, requested: Option<&str>) -> &Profile {
        let want = requested.unwrap_or(self.default_name.as_str());
        match self.profiles.get(want) {
            Some(p) => p,
            None => {
                tracing::warn!(
                    requested_profile = %want,
                    "tool_profile: unknown profile name, using default"
                );
                self.profiles
                    .get(self.default_name.as_str())
                    .expect("default profile always present in registry")
            }
        }
    }

    pub fn default_name(&self) -> &str {
        &self.default_name
    }

    pub fn profile_names(&self) -> impl Iterator<Item = &str> {
        self.profiles.keys().map(String::as_str)
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ToolProfileError {
    #[error("toml parse error: {0}")]
    Toml(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("invalid allow_tools: {0}")]
    BadAllow(String),
}

#[derive(Debug, Deserialize)]
struct TomlRoot {
    default: Option<String>,
    profile: Option<HashMap<String, TomlProfile>>,
}

#[derive(Debug, Deserialize)]
struct TomlProfile {
    description: Option<String>,
    allow_tools: AllowTomlValue,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AllowTomlValue {
    Wildcard(String),
    List(Vec<String>),
}

/// Process-global registry singleton. Initialised lazily on first
/// `global()` call: tries `~/.sovereign/tool_profiles.toml`, falls
/// back to `allow_all`. Subsequent calls return the cached value.
///
/// Clearing-and-reloading the singleton at runtime would require an
/// explicit reload mechanism; v1 expects a daemon restart for TOML
/// changes. This matches how `setup_config.toml` is consumed.
static REGISTRY: OnceLock<ToolProfileRegistry> = OnceLock::new();

pub fn global() -> &'static ToolProfileRegistry {
    REGISTRY.get_or_init(|| {
        let path = default_config_path();
        match ToolProfileRegistry::from_path(&path) {
            Ok(r) => {
                tracing::info!(
                    path = %path.display(),
                    profile_count = r.profiles.len(),
                    default = %r.default_name(),
                    "tool_profile: registry loaded"
                );
                r
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "tool_profile: failed to load registry, falling back to allow_all"
                );
                ToolProfileRegistry::allow_all()
            }
        }
    })
}

/// `~/.sovereign/tool_profiles.toml` for the user that started the
/// daemon. Falls back to a placeholder path if `HOME` isn't set
/// (which would only happen in a stripped CI shell — Read::Error in
/// from_path then silently degrades to allow_all).
fn default_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".sovereign").join("tool_profiles.toml")
}

/// Apply the requested profile (or registry default) to a request
/// in place. Logs the count of dropped tools so the operator sees
/// the trim land. Returns the profile that was applied.
///
/// The single integration point used by the inference adapter.
/// Keeps the call site one line and centralises every observability
/// concern (profile name, drop count, before/after sizes).
pub fn apply<'a>(
    registry: &'a ToolProfileRegistry,
    request: &mut ChatCompletionRequest,
) -> &'a Profile {
    let profile = registry.resolve(request.tool_profile.as_deref());
    let dropped = profile.filter(&mut request.tools);
    let kept = request.tools.as_ref().map(|v| v.len()).unwrap_or(0);
    if dropped > 0 {
        tracing::info!(
            profile = %profile.name,
            dropped,
            kept,
            "tool_profile: filtered request.tools[]"
        );
    } else {
        tracing::debug!(
            profile = %profile.name,
            kept,
            "tool_profile: no-op (wildcard or no matches)"
        );
    }
    profile
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_api::openai_types::{ChatCompletionRequest, ToolFunction};

    fn td(name: &str) -> ToolDefinition {
        ToolDefinition {
            kind: "function".into(),
            function: ToolFunction {
                name: name.into(),
                description: None,
                parameters: serde_json::json!({"type": "object"}),
            },
        }
    }

    fn req(tools: Vec<&str>, profile: Option<&str>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: None,
            messages: vec![],
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools: Some(tools.into_iter().map(td).collect()),
            tool_choice: None,
            response_format: None,
            oicp: None,
            chat_template_kwargs: None,
            think_budget: None,
            tool_profile: profile.map(String::from),
        }
    }

    // ---- registry construction ----

    #[test]
    fn allow_all_has_only_permissive() {
        let r = ToolProfileRegistry::allow_all();
        assert_eq!(r.default_name(), "permissive");
        let p = r.resolve(None);
        assert_eq!(p.name, "permissive");
        assert!(matches!(p.allow, AllowList::Wildcard));
    }

    #[test]
    fn from_toml_parses_minimal() {
        let body = r#"
default = "rust-edit"

[profile.rust-edit]
description = "Rust authoring loop"
allow_tools = ["write", "edit", "bash"]

[profile.full]
allow_tools = "*"
"#;
        let r = ToolProfileRegistry::from_toml(body).unwrap();
        assert_eq!(r.default_name(), "rust-edit");
        let p = r.resolve(Some("rust-edit"));
        assert_eq!(p.name, "rust-edit");
        match &p.allow {
            AllowList::Names(s) => {
                assert!(s.contains("write"));
                assert!(s.contains("edit"));
                assert!(s.contains("bash"));
                assert_eq!(s.len(), 3);
            }
            _ => panic!("expected Names"),
        }
        assert!(matches!(r.resolve(Some("full")).allow, AllowList::Wildcard));
        // Synthesised permissive is always present.
        assert!(matches!(
            r.resolve(Some("permissive")).allow,
            AllowList::Wildcard
        ));
    }

    #[test]
    fn from_toml_unknown_default_falls_back_to_permissive() {
        let body = r#"
default = "ghost"
"#;
        let r = ToolProfileRegistry::from_toml(body).unwrap();
        // Default is bogus; registry falls back to permissive.
        assert_eq!(r.default_name(), "permissive");
    }

    #[test]
    fn from_toml_rejects_non_wildcard_string() {
        let body = r#"
[profile.bad]
allow_tools = "not-a-wildcard"
"#;
        let err = ToolProfileRegistry::from_toml(body).unwrap_err();
        match err {
            ToolProfileError::BadAllow(msg) => {
                assert!(msg.contains("must be \"*\""));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn from_toml_empty_body_yields_only_permissive() {
        let r = ToolProfileRegistry::from_toml("").unwrap();
        assert_eq!(r.default_name(), "permissive");
        let names: Vec<_> = r.profile_names().collect();
        assert_eq!(names.len(), 1);
        assert_eq!(names[0], "permissive");
    }

    // ---- Profile::filter ----

    #[test]
    fn wildcard_profile_keeps_everything() {
        let r = ToolProfileRegistry::allow_all();
        let mut req = req(vec!["write", "edit", "bash", "read", "grep"], None);
        let _p = apply(&r, &mut req);
        assert_eq!(req.tools.as_ref().unwrap().len(), 5);
    }

    #[test]
    fn named_profile_drops_everything_else() {
        let body = r#"
default = "rust-edit"
[profile.rust-edit]
allow_tools = ["write", "edit", "bash"]
"#;
        let r = ToolProfileRegistry::from_toml(body).unwrap();
        let mut req = req(vec!["write", "edit", "bash", "read", "grep", "glob"], None);
        apply(&r, &mut req);
        let names: Vec<_> = req.tools.as_ref().unwrap()
            .iter().map(|t| t.function.name.as_str()).collect();
        assert_eq!(names, vec!["write", "edit", "bash"]);
    }

    #[test]
    fn empty_allow_list_yields_zero_tools() {
        let body = r#"
default = "no-tools"
[profile.no-tools]
allow_tools = []
"#;
        let r = ToolProfileRegistry::from_toml(body).unwrap();
        let mut req = req(vec!["write", "edit"], None);
        apply(&r, &mut req);
        assert_eq!(req.tools.as_ref().unwrap().len(), 0);
    }

    #[test]
    fn per_request_profile_overrides_default() {
        let body = r#"
default = "permissive"
[profile.locked]
allow_tools = []
"#;
        let r = ToolProfileRegistry::from_toml(body).unwrap();
        let mut req = req(vec!["write", "edit"], Some("locked"));
        apply(&r, &mut req);
        assert_eq!(req.tools.as_ref().unwrap().len(), 0);
    }

    #[test]
    fn unknown_per_request_profile_falls_back_to_default() {
        let body = r#"
default = "rust-edit"
[profile.rust-edit]
allow_tools = ["write"]
"#;
        let r = ToolProfileRegistry::from_toml(body).unwrap();
        let mut req = req(vec!["write", "edit"], Some("typo-profile-name"));
        apply(&r, &mut req);
        // Falls back to default which is rust-edit (allow=write).
        let names: Vec<_> = req.tools.as_ref().unwrap()
            .iter().map(|t| t.function.name.as_str()).collect();
        assert_eq!(names, vec!["write"]);
    }

    #[test]
    fn no_tools_field_is_no_op() {
        let r = ToolProfileRegistry::allow_all();
        let mut req = ChatCompletionRequest {
            model: None,
            messages: vec![],
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            oicp: None,
            chat_template_kwargs: None,
            think_budget: None,
            tool_profile: Some("rust-edit".into()),
        };
        apply(&r, &mut req);
        assert!(req.tools.is_none());
    }

    #[test]
    fn from_path_missing_file_yields_allow_all() {
        let p = std::path::PathBuf::from("/tmp/sovereign_no_such_profile_file_xyz123.toml");
        let r = ToolProfileRegistry::from_path(&p).unwrap();
        assert_eq!(r.default_name(), "permissive");
    }
}
