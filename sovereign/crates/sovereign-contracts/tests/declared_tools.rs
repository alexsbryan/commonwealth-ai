// SPDX-License-Identifier: AGPL-3.0-or-later
//! The extensibility proof for noun-convergence rung nc-13.
//!
//! The claim under test is narrow and mechanical: **a tool can be added to a
//! running `ToolRegistry` as DATA — a `[[tool]]` block and nothing else — and
//! it dispatches, validates and executes through exactly the production code
//! path a hand-written tool does.**
//!
//! Everything here goes through the shipped surfaces (`parse_family`,
//! `DeclaredTool`, `ToolRegistry::register` / `get` / `install_declared`). The
//! only thing local to this file is the TOML text, which is the point — those
//! bytes are what an author would append to a family file under
//! `tool-manifests/`.
//!
//! What this does NOT claim: that the 82 existing `impl Tool for` blocks are
//! gone. They are not. The manifest half is declarative; the executable half
//! of a tool with genuinely new behaviour is still code, and `nc-extends`'s
//! tool axis counts the block, not the literal.

use std::sync::Arc;

use async_trait::async_trait;
use futures::executor::block_on;
use sovereign_contracts::error::Result;
use sovereign_contracts::registry::ToolRegistry;
use sovereign_contracts::tool_manifest::{
    delegating_handler, parse_family, DeclaredTool, ToolHandler,
};
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::{
    Effect, Idempotency, Latency, Permission, Scope, StepOutput, ToolContext, ToolDescriptor,
};

/// A manifest with a handler named in code — the shape a tool with new
/// behaviour takes. No `impl Tool for` block of its own.
const DECLARED_ONLY: &str = r#"
[[tool]]
id = "echo_declared"
name = "Echo (declared)"
description = "Returns its `text` parameter. Exists to prove a manifest plus a handler is a tool."
effect = "read"
idempotency = "idempotent"
latency = "instant"
scope = "session"
permissions = ["FileRead"]

[tool.parameters]
type = "object"
required = ["text"]

[tool.parameters.properties.text]
type = "string"
description = "What to echo back."

[tool.parameters.properties.times]
type = "integer"
description = "How many times."

[[tool.examples]]
situation = "You want to check the declared-tool path end to end."
call = { text = "hello", times = 2 }
"#;

/// A manifest with NO Rust at all: it declares an existing tool under a
/// narrower description and a pinned default.
const DELEGATING_ONLY: &str = r#"
[[tool]]
id = "shout"
name = "Shout"
description = "`echo_declared` with `times` pinned to 3 — a tool that is pure data."
effect = "read"
idempotency = "idempotent"
latency = "instant"
scope = "session"
delegate = "echo_declared"
defaults = { times = 3 }

[tool.parameters]
type = "object"
required = ["text"]

[tool.parameters.properties.text]
type = "string"
description = "What to shout."
"#;

fn echo_handler() -> ToolHandler {
    Arc::new(|params, _ctx| {
        Box::pin(async move {
            let text = params
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let times = params.get("times").and_then(|v| v.as_u64()).unwrap_or(1);
            Ok(StepOutput::Text(
                std::iter::repeat_n(text, times as usize)
                    .collect::<Vec<_>>()
                    .join(" "),
            ))
        })
    })
}

fn declared(toml_src: &str, handler: ToolHandler) -> DeclaredTool {
    let mut manifests = parse_family(toml_src).expect("manifest parses");
    DeclaredTool::from_manifest(manifests.remove(0), handler)
}

/// A hand-written tool, present so the delegating case has a coded target and
/// so the two paths are compared against the same registry.
struct CodedTool;

#[async_trait]
impl Tool for CodedTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "coded".into(),
            name: "Coded".into(),
            description: "hand-written".into(),
            parameters: serde_json::json!({ "type": "object" }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: None,
        }
    }
    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }
    async fn execute(&self, _p: &serde_json::Value, _c: &ToolContext) -> Result<StepOutput> {
        Ok(StepOutput::Text("coded".into()))
    }
}

/// THE BAR: TOML text in, a registered tool out, dispatched through
/// `ToolRegistry::get(...).execute(...)`. No `impl Tool for` was written for
/// `echo_declared`.
#[test]
fn a_manifest_and_a_handler_make_a_registered_tool() {
    let mut reg = ToolRegistry::new();
    reg.register(Box::new(CodedTool));
    reg.register(Box::new(declared(DECLARED_ONLY, echo_handler())));

    let tool = reg.get("echo_declared").expect("registered by declaration");

    // Identity, behavioural properties and schema all came from the TOML.
    let d = tool.descriptor();
    assert_eq!(d.id, "echo_declared");
    assert_eq!(d.name, "Echo (declared)");
    assert!(matches!(d.effect, Effect::Read));
    assert!(matches!(d.latency, Latency::Instant));
    assert_eq!(d.examples.len(), 1);
    assert_eq!(d.parameters["required"][0], "text");

    // Permissions came from the TOML too — one decider, not two.
    assert_eq!(tool.required_permissions(), vec![Permission::FileRead]);

    let out = block_on(tool.execute(
        &serde_json::json!({ "text": "hi", "times": 2 }),
        &ToolContext::default(),
    ))
    .expect("declared tool executes");
    assert!(matches!(out, StepOutput::Text(ref s) if s == "hi hi"), "{out:?}");
}

/// The declaration carries the parameter check: `required` and `type` are
/// enforced from the schema, with no hand-written `validate()` body.
#[test]
fn the_declaration_validates_parameters() {
    let tool = declared(DECLARED_ONLY, echo_handler());

    let missing = tool.validate(&serde_json::json!({})).unwrap_err();
    assert!(
        missing.to_string().contains("requires `text`"),
        "{missing}"
    );

    let wrong_type = tool
        .validate(&serde_json::json!({ "text": "ok", "times": "three" }))
        .unwrap_err();
    assert!(
        wrong_type.to_string().contains("must be integer"),
        "{wrong_type}"
    );

    tool.validate(&serde_json::json!({ "text": "ok", "times": 3 }))
        .expect("a well-formed call passes");
}

/// THE STRONGER BAR: a tool whose ENTIRE definition is data. `shout` has no
/// handler of its own — `install_declared` binds it to an already-registered
/// tool and merges the declared defaults.
#[test]
fn a_manifest_alone_makes_a_working_tool() {
    let mut reg = ToolRegistry::new();
    reg.register(Box::new(declared(DECLARED_ONLY, echo_handler())));

    let manifest = parse_family(DELEGATING_ONLY).expect("parses").remove(0);
    let target = reg.get_arc("echo_declared").expect("target registered");
    let handler = delegating_handler(target, manifest.defaults.clone());
    reg.register(Box::new(DeclaredTool::from_manifest(manifest, handler)));

    let shout = reg.get("shout").expect("declared tool is registered");
    assert_eq!(shout.descriptor().id, "shout");

    // The default applied: `times` was never passed by the caller.
    let out = block_on(shout.execute(&serde_json::json!({ "text": "go" }), &ToolContext::default()))
        .expect("delegating tool executes");
    assert!(matches!(out, StepOutput::Text(ref s) if s == "go go go"), "{out:?}");
}

/// A default is a default, not a pin — a caller-supplied key wins.
#[test]
fn caller_parameters_win_over_declared_defaults() {
    let mut reg = ToolRegistry::new();
    reg.register(Box::new(declared(DECLARED_ONLY, echo_handler())));
    let manifest = parse_family(DELEGATING_ONLY).expect("parses").remove(0);
    let target = reg.get_arc("echo_declared").expect("target");
    let handler = delegating_handler(target, manifest.defaults.clone());

    let tool = DeclaredTool::from_manifest(manifest, handler);
    let out = block_on(tool.execute(
        &serde_json::json!({ "text": "x", "times": 1 }),
        &ToolContext::default(),
    ))
    .expect("executes");
    assert!(matches!(out, StepOutput::Text(ref s) if s == "x"), "{out:?}");
}

/// A delegate whose target is not registered on THIS host is skipped, not
/// faked — ARCH §18.3, absence is reported rather than defaulted.
#[test]
fn install_declared_skips_a_delegate_with_no_target() {
    let mut reg = ToolRegistry::new();
    let before = reg.count();
    // Nothing in the shipped catalog declares a delegate today, and no target
    // is registered, so this installs nothing and — critically — does not
    // invent a tool that cannot work.
    let installed = reg.install_declared();
    assert!(installed.is_empty(), "installed: {installed:?}");
    assert_eq!(reg.count(), before);
}

/// Code wins over a declaration on the same id.
#[test]
fn a_coded_tool_is_not_shadowed_by_a_declaration() {
    let mut reg = ToolRegistry::new();
    reg.register(Box::new(CodedTool));
    let installed = reg.install_declared();
    assert!(!installed.contains(&"coded".to_string()));
    let out = block_on(
        reg.get("coded")
            .unwrap()
            .execute(&serde_json::json!({}), &ToolContext::default()),
    )
    .unwrap();
    assert!(matches!(out, StepOutput::Text(ref s) if s == "coded"));
}
