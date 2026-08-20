// SPDX-License-Identifier: AGPL-3.0-or-later
//! The DECLARED half of a tool — identity, behavioural properties, parameter
//! schema, worked examples, required permissions — as checked-in data.
//!
//! # Why this exists
//!
//! `Recipe` is 46 TOML files declaring acquire → extract → filter → chunk →
//! embed → index, and adding a corpus touches no Rust. `Tool` was the
//! opposite: a hand-written `impl Tool for` block whose `descriptor()` body
//! was, in 80 of 82 measured cases, a literal that never reads `self` — 4,137
//! lines of data spelled as code. This module is the other half of that
//! asymmetry closed: the literal moves to `tool-manifests/*.toml` and the
//! `impl` keeps only the part that actually runs.
//!
//! # What a manifest carries
//!
//! Not just shape. A shared struct saves an author nothing and loses to
//! bespoke every time (`quality/NOUN_CONVERGENCE.md` §10.3 — adoption is
//! monotone in WORK CARRIED and in nothing else). A [`ToolManifest`] carries
//! three jobs the author would otherwise re-derive per tool:
//!
//! 1. [`ToolManifest::to_descriptor`] — descriptor construction.
//! 2. [`ToolManifest::validate_params`] — schema-driven parameter checking,
//!    so `required` and `type` are enforced from the declaration rather than
//!    restated by hand in a `validate()` body.
//! 3. [`DeclaredTool`] — a tool that has a manifest and a named handler needs
//!    no `impl Tool for` block of its own.
//!
//! # Adding a tool
//!
//! Append a `[[tool]]` block to a family file under `tool-manifests/`. That is
//! the whole change for a tool whose behaviour already exists — see
//! [`delegating_handler`]. A tool with NEW behaviour still writes the
//! executable half; the manifest is what stops it also writing 50 lines of
//! descriptor literal, a permissions list, and a validator.
//!
//! Adding a new FAMILY file is the one remaining Rust edit (one line in
//! [`FAMILIES`]) and is deliberately rare — families track source directories,
//! not tools.
//!
//! # Not `Capability`, yet
//!
//! noun-convergence rung 8 may make a tool a `Capability`. This module
//! declares the FIELDS without claiming that name, so rung 8 inherits a free
//! choice over a data file rather than a shim to unpick.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::traits::Tool;
use crate::types::{
    Effect, Idempotency, Latency, Permission, RetryConfig, Scope, StepOutput, ToolContext,
    ToolDescriptor, ToolExample,
};

/// Code-intelligence manifests — every tool under `sovereign-tools/src/code/`.
///
/// Anchored at `CARGO_MANIFEST_DIR` so the one repo-relative reference to the
/// artifact lives here, once — the same convention as
/// [`crate::recipe::registry::RECIPE_REGISTRY_TOML`].
pub const CODE_MANIFESTS_TOML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tool-manifests/code.toml"
));

/// Knowledge / document / communication manifests — every tool under
/// `sovereign-tools/src/` outside `code/`.
pub const KNOWLEDGE_MANIFESTS_TOML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tool-manifests/knowledge.toml"
));

/// Every family file, by name. A new family is one line here; a new TOOL is
/// zero lines here.
pub const FAMILIES: &[(&str, &str)] = &[
    ("code", CODE_MANIFESTS_TOML),
    ("knowledge", KNOWLEDGE_MANIFESTS_TOML),
];

/// The declared half of one tool.
///
/// Field-for-field the data in a [`ToolDescriptor`] plus the two policy
/// declarations that live beside it on the `Tool` trait — the permissions the
/// consent layer must hold, and the retry posture. Everything here is
/// answerable without running anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolManifest {
    /// Registry id — the key `ToolRegistry` dispatches on.
    pub id: String,
    /// Human-readable name shown in prompts and UI.
    pub name: String,
    /// What the tool does, phrased for the model choosing among tools.
    pub description: String,
    /// Read / write classification — gates approval routing.
    pub effect: Effect,
    /// Whether a duplicate call duplicates the effect — drives the retry gate.
    pub idempotency: Idempotency,
    /// Expected cost class — drives plan parallelisation and timeouts.
    pub latency: Latency,
    /// Where the effect lives (session / persistent / external).
    pub scope: Scope,
    /// JSON schema of accepted arguments. Declared as a TOML table; read as
    /// JSON, which is what the planner prompt and [`Self::validate_params`]
    /// both want.
    #[serde(default = "empty_object")]
    pub parameters: serde_json::Value,
    /// Worked invocations. Small models copy examples more reliably than they
    /// follow descriptions.
    #[serde(default)]
    pub examples: Vec<ToolExample>,
    /// Shape of the tool's output, when it has a declarable one.
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,
    /// Permissions the consent layer must hold before `execute` may run.
    #[serde(default)]
    pub permissions: Vec<Permission>,
    /// Retry posture. Absent means "do not retry" — the `Tool` trait default.
    #[serde(default)]
    pub retry: Option<RetryConfig>,
    /// For a tool that IS an existing tool under a narrower declaration: the
    /// id of the tool that does the work. Its parameters are merged under
    /// [`Self::defaults`]. A manifest with a `delegate` needs no Rust at all.
    #[serde(default)]
    pub delegate: Option<String>,
    /// Parameters merged into every call before delegation. Caller-supplied
    /// keys WIN — a default is a default, not a pin.
    #[serde(default)]
    pub defaults: Option<serde_json::Value>,
}

fn empty_object() -> serde_json::Value {
    serde_json::json!({})
}

#[derive(Debug, Deserialize)]
struct Family {
    #[serde(default)]
    tool: Vec<ToolManifest>,
}

static CATALOG: OnceLock<BTreeMap<String, ToolManifest>> = OnceLock::new();

/// Every declared manifest, keyed by tool id.
///
/// Parsed once from the embedded family files. A malformed family is a
/// PANIC, not a silent skip: the files are compile-time constants, so a parse
/// failure is a developer error that must never reach a caller as a missing
/// tool (ARCH §18.3 — absence is reported, never defaulted). The
/// `catalog_parses` test below is what makes that panic unreachable in
/// practice.
pub fn catalog() -> &'static BTreeMap<String, ToolManifest> {
    CATALOG.get_or_init(|| {
        let mut out = BTreeMap::new();
        for (family, toml_src) in FAMILIES {
            let parsed: Family = toml::from_str(toml_src)
                .unwrap_or_else(|e| panic!("tool-manifests/{family}.toml is malformed: {e}"));
            for manifest in parsed.tool {
                if let Some(prev) = out.insert(manifest.id.clone(), manifest) {
                    panic!(
                        "tool id `{}` is declared twice; the second was in \
                         tool-manifests/{family}.toml",
                        prev.id
                    );
                }
            }
        }
        tracing::debug!(
            tools = out.len(),
            families = FAMILIES.len(),
            "tool manifest catalog loaded"
        );
        out
    })
}

/// Parse a family file's worth of `[[tool]]` blocks.
///
/// The same shape the embedded catalog files use, exposed so a host can carry
/// a local declaration — and so a test can prove the whole path from TOML text
/// to a dispatchable tool without a checked-in row.
pub fn parse_family(toml_src: &str) -> Result<Vec<ToolManifest>> {
    let parsed: Family = toml::from_str(toml_src)
        .map_err(|e| Error::InvalidInput(format!("malformed tool manifest: {e}")))?;
    Ok(parsed.tool)
}

/// The manifest for `id`, or `None` when nothing declares it.
pub fn get(id: &str) -> Option<&'static ToolManifest> {
    catalog().get(id)
}

/// The manifest for `id`, or a panic naming the id.
///
/// The caller is a `Tool::descriptor` implementation, which cannot return an
/// error — so the only honest options are the right answer or a loud stop. A
/// missing manifest means an id typo, it fails the first time the tool is
/// constructed, and `every_declared_id_resolves` catches it in CI before that.
pub fn require(id: &str) -> &'static ToolManifest {
    get(id).unwrap_or_else(|| {
        panic!(
            "no manifest declared for tool id `{id}` — add a [[tool]] block to a \
             file under sovereign-contracts/tool-manifests/"
        )
    })
}

impl ToolManifest {
    /// Render this declaration as the descriptor the router, planner and MCP
    /// surface read.
    pub fn to_descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: self.id.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
            examples: self.examples.clone(),
            effect: self.effect,
            idempotency: self.idempotency,
            latency: self.latency,
            scope: self.scope,
            output_schema: self.output_schema.clone(),
        }
    }

    /// Check `params` against the declared schema: every `required` key is
    /// present, and every present key whose schema declares a `type` matches
    /// it.
    ///
    /// This is the work the declaration carries that a bare data struct would
    /// not — the reason to reach for a manifest rather than mint a descriptor
    /// literal. Deliberately NOT a full JSON Schema validator: it enforces the
    /// two constraints every hand-written `validate()` body in the tree was
    /// already restating, and leaves genuinely tool-specific checks (a symbol
    /// name's character set, a path's shape) to the tool, which is where they
    /// belong.
    pub fn validate_params(&self, params: &serde_json::Value) -> Result<()> {
        let Some(schema) = self.parameters.as_object() else {
            return Ok(());
        };
        let supplied = params.as_object();
        if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
            for key in required.iter().filter_map(|k| k.as_str()) {
                let present = supplied
                    .map(|o| o.get(key).is_some_and(|v| !v.is_null()))
                    .unwrap_or(false);
                if !present {
                    return Err(Error::InvalidInput(format!("{} requires `{key}`", self.id)));
                }
            }
        }
        let (Some(props), Some(supplied)) = (
            schema.get("properties").and_then(|p| p.as_object()),
            supplied,
        ) else {
            return Ok(());
        };
        for (key, value) in supplied {
            if value.is_null() {
                continue;
            }
            let Some(declared) = props
                .get(key)
                .and_then(|s| s.get("type"))
                .and_then(|t| t.as_str())
            else {
                continue;
            };
            if !json_type_matches(declared, value) {
                return Err(Error::InvalidInput(format!(
                    "{}: `{key}` must be {declared}, got {}",
                    self.id,
                    json_type_name(value)
                )));
            }
        }
        Ok(())
    }
}

fn json_type_matches(declared: &str, value: &serde_json::Value) -> bool {
    match declared {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        // An unrecognised or union `type` declaration constrains nothing —
        // better to accept than to invent a rule the schema did not state.
        _ => true,
    }
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(n) => {
            if n.is_f64() {
                "number"
            } else {
                "integer"
            }
        }
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// The executable half of a declared tool: parameters and context in, a step
/// output out.
///
/// A boxed closure rather than a trait on purpose. A `ToolHandler` TRAIT would
/// be `Tool` under another name — the same per-tool `impl` block moved one
/// word to the left — and this rung exists to remove that block, not rename
/// it.
pub type ToolHandler = Arc<
    dyn Fn(
            serde_json::Value,
            ToolContext,
        ) -> Pin<Box<dyn Future<Output = Result<StepOutput>> + Send>>
        + Send
        + Sync,
>;

/// A tool assembled from a manifest and a handler — the one `impl Tool for`
/// that every declared tool shares.
pub struct DeclaredTool {
    manifest: ToolManifest,
    handler: ToolHandler,
}

impl DeclaredTool {
    /// Bind the manifest declared for `id` in the catalog to `handler`.
    ///
    /// `Err` when nothing declares `id` — unlike [`require`], this caller CAN
    /// report, so it does.
    pub fn new(id: &str, handler: ToolHandler) -> Result<Self> {
        let manifest = get(id)
            .ok_or_else(|| Error::InvalidInput(format!("no manifest declared for tool id `{id}`")))?
            .clone();
        Ok(Self { manifest, handler })
    }

    /// Bind an owned manifest to `handler` — for manifests that did not come
    /// from the embedded catalog (a host-local declaration, a test).
    pub fn from_manifest(manifest: ToolManifest, handler: ToolHandler) -> Self {
        Self { manifest, handler }
    }

    /// The manifest this tool was declared from.
    pub fn manifest(&self) -> &ToolManifest {
        &self.manifest
    }
}

#[async_trait]
impl Tool for DeclaredTool {
    fn descriptor(&self) -> ToolDescriptor {
        self.manifest.to_descriptor()
    }

    fn required_permissions(&self) -> Vec<Permission> {
        self.manifest.permissions.clone()
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        self.manifest.validate_params(params)
    }

    fn retry_config(&self) -> Option<RetryConfig> {
        self.manifest.retry.clone()
    }

    async fn execute(&self, params: &serde_json::Value, ctx: &ToolContext) -> Result<StepOutput> {
        (self.handler)(params.clone(), ctx.clone()).await
    }
}

/// A handler that forwards to an already-registered tool, merging `defaults`
/// underneath the caller's parameters.
///
/// This is what makes a manifest sufficient on its own: a tool that is an
/// existing capability under a narrower description, a tighter schema and its
/// own worked examples needs no new Rust. Caller keys win over defaults.
pub fn delegating_handler(
    target: Arc<dyn Tool>,
    defaults: Option<serde_json::Value>,
) -> ToolHandler {
    Arc::new(move |params, ctx| {
        let target = Arc::clone(&target);
        let defaults = defaults.clone();
        Box::pin(async move {
            let merged = match (defaults, params) {
                (Some(serde_json::Value::Object(d)), serde_json::Value::Object(p)) => {
                    let mut out = d;
                    out.extend(p);
                    serde_json::Value::Object(out)
                }
                (_, params) => params,
            };
            target.validate(&merged)?;
            target.execute(&merged, &ctx).await
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalog parses and every row is well-formed. This test is what
    /// makes [`require`]'s panic unreachable in practice — ARCH §18.4,
    /// validate the instrument before the result.
    #[test]
    fn catalog_parses_and_every_row_is_well_formed() {
        let cat = catalog();
        assert!(
            cat.len() >= 50,
            "expected the converted sovereign-tools manifests, found {}",
            cat.len()
        );
        for (id, m) in cat {
            assert_eq!(id, &m.id, "catalog key must be the manifest id");
            assert!(!m.id.is_empty(), "empty tool id");
            assert!(!m.name.is_empty(), "{id}: empty name");
            assert!(!m.description.is_empty(), "{id}: empty description");
            assert!(
                m.parameters.is_object(),
                "{id}: parameters must be a JSON object schema"
            );
        }
    }

    /// Every id the catalog declares resolves through the accessor the
    /// `descriptor()` bodies call.
    #[test]
    fn every_declared_id_resolves() {
        for id in catalog().keys() {
            assert_eq!(require(id).id, *id);
        }
    }

    #[test]
    fn to_descriptor_carries_every_declared_field() {
        let m = require("callers");
        let d = m.to_descriptor();
        assert_eq!(d.id, "callers");
        assert!(matches!(d.effect, Effect::Read));
        assert!(matches!(d.scope, Scope::Persistent));
        assert!(
            d.parameters["properties"]["symbol"].is_object(),
            "parameter schema must survive the TOML round-trip"
        );
        assert!(!d.examples.is_empty(), "worked examples must survive");
    }

    #[test]
    fn validate_params_rejects_a_missing_required_key() {
        let m = require("callers");
        let err = m.validate_params(&serde_json::json!({})).unwrap_err();
        assert!(
            err.to_string().contains("requires `symbol`"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn validate_params_rejects_a_wrong_type() {
        let m = require("callers");
        let err = m
            .validate_params(&serde_json::json!({ "symbol": 12 }))
            .unwrap_err();
        assert!(
            err.to_string().contains("must be string"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn validate_params_accepts_a_well_formed_call() {
        let m = require("callers");
        m.validate_params(&serde_json::json!({ "symbol": "execute_step", "depth": 2 }))
            .expect("well-formed params");
    }

    /// An unconstrained schema constrains nothing — a tool declaring no
    /// `properties` must not start rejecting calls it used to accept.
    #[test]
    fn validate_params_is_permissive_where_the_schema_is_silent() {
        let m = ToolManifest {
            id: "t".into(),
            name: "t".into(),
            description: "t".into(),
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Session,
            parameters: serde_json::json!({ "type": "object" }),
            examples: vec![],
            output_schema: None,
            permissions: vec![],
            retry: None,
            delegate: None,
            defaults: None,
        };
        m.validate_params(&serde_json::json!({ "anything": [1, 2, 3] }))
            .expect("silent schema accepts anything");
    }
}
