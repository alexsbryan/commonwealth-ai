// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ToolRegistry half of an MCP `tools/call` (phase-b pb-code-server,
//! decision phase-b-13): validate, audit a write, build the [`ToolContext`],
//! execute, map the [`StepOutput`]. The daemon's `handle_tool_call` and the
//! code server's tool host run this one copy. What a program exposes, and
//! what it logs, stay the program's.
//!
//! It answers in `oicp_types::mcp::ToolOutcome`, the MCP wire vocabulary, and
//! links no host kit: sovereign-contracts is the contract layer the thin
//! surfaces may reach, and the kit is not (ARCH_LAYERS `[thin_surfaces]`).

use serde_json::Value;

use crate::oicp::mcp::{CallAudit, McpRequestContext, ToolOutcome};
use crate::registry::ToolRegistry;
use crate::types::{StepOutput, ToolContext};
use crate::Effect;

/// The agent session a call runs under: the `X-Agent-Session` the agent
/// sent, else `conn:<session_id>` for the server run, so clients that send no
/// header still group by connection. Only tools that read
/// `ToolContext::agent_session_token` (the work atlas) see it.
pub fn agent_session_token(header: Option<String>, session_id: &str) -> String {
    header.unwrap_or_else(|| format!("conn:{session_id}"))
}

/// Whether a connection reaches a tool of `effect`: a read-only one
/// (`x-svrn-effects: read`) reaches `Read` alone, `ReadWrite` counting as a
/// write. The one rule both `tools/list` ([`render_tool_entries`]) and
/// `tools/call` ([`call_registry_tool`]) apply, so a tool cannot be hidden
/// from the list and still answer a call.
pub fn effect_reachable(read_only: bool, effect: Effect) -> bool {
    !read_only || effect == Effect::Read
}

/// Run one tool from `tools` as an MCP call for the request `ctx`. `None`
/// when `exposed` refuses the canonical `name` (the caller answers -32601).
/// A name the registry does not hold, a tool whose effect the connection
/// does not reach, and arguments that fail validation answer without
/// executing, so their outcome carries no audit. `ctx.corpus` is taken as
/// admitted: the host judged it before calling here.
pub async fn call_registry_tool(
    tools: &ToolRegistry,
    name: &str,
    arguments: &Value,
    session_id: &str,
    ctx: &McpRequestContext,
    exposed: impl Fn(&str) -> bool,
) -> Option<ToolOutcome> {
    if !exposed(name) {
        tracing::debug!(tool = %name, "mcp: tool not exposed");
        return None;
    }

    let tool = match tools.get(name) {
        Ok(t) => t,
        Err(_) => {
            tracing::debug!(tool = %name, "mcp: tool not registered");
            return Some(ToolOutcome::answer(
                format!("`{name}` not registered. Run `sovereign project init` first."),
                None,
            ));
        }
    };

    let effect = tool.descriptor().effect;
    if !effect_reachable(ctx.read_only, effect) {
        tracing::info!(tool = %name, ?effect, "mcp: refused, the connection reads only");
        return Some(ToolOutcome::refusal(format!(
            "`{name}` is a {effect:?} tool, and this connection reaches only Read tools \
             (it sent `{}: {}`)",
            crate::oicp::mcp::MCP_EFFECTS_HEADER,
            crate::oicp::mcp::MCP_EFFECTS_READ,
        )));
    }

    if let Err(e) = tool.validate(arguments) {
        tracing::debug!(tool = %name, error = %e, "mcp: arguments refused");
        return Some(ToolOutcome::refusal(e.to_string()));
    }

    // Phase 1.5 audit gate: MCP is stdio/HTTP non-interactive, so the
    // executor's `ApprovalChannel::request_approval` path (which blocks
    // on human input) can't fire here. Instead we AUDIT every write-
    // effectful MCP call — tracing::warn!, plus a dedicated outcome
    // tag in the ring buffer — so an operator running `sovereign
    // reflect` sees every unapproved write after the fact. A future
    // interactive-MCP protocol extension can upgrade this to a hard
    // block without changing the surrounding structure.
    //
    // See ARCH_PRINCIPLES.md §7 (structural invariants) and §9
    // (glassbox). The parity gate to the executor's StepKind::Tool
    // path closes once MCP has an approval protocol; until then
    // visibility is the achievable half.
    let descriptor = tool.descriptor();
    let is_write_effectful = descriptor.effect != Effect::Read;
    if is_write_effectful {
        tracing::warn!(
            tool_id = %name,
            effect = ?descriptor.effect,
            idempotency = ?descriptor.idempotency,
            session_id = %session_id,
            "mcp: write-effectful tool invoked without approval gate \
             (MCP protocol does not support interactive approval; \
             audit-only per Phase 1.5)"
        );
    }

    // Glassbox the dispatch so operators can correlate work-atlas
    // session creation with the MCP call that triggered it (ARCH §9.1).
    // Truncate the token to 12 chars per ARCH §9.3 — redact deliberately.
    let agent_session_token = agent_session_token(ctx.agent_session.clone(), session_id);
    let token_redacted: String = agent_session_token.chars().take(12).collect();
    tracing::debug!(
        tool = %name,
        agent_session_token = %token_redacted,
        corpus_scope = ?ctx.corpus,
        "mcp:tool_call dispatched"
    );

    let tool_ctx = ToolContext {
        conversation_id: "mcp".to_string(),
        task_id: None,
        working_directory: None,
        in_reasoning_loop: false,
        agent_session_token: Some(agent_session_token),
        turn_index: 0,
        corpus_scope: ctx.corpus.clone(),
        ..Default::default()
    };

    let result = tool.execute(arguments, &tool_ctx).await;

    let effect = descriptor.effect;
    // Detect empty/null results to flag "index missing content" signals.
    let empty_result = matches!(&result, Ok(StepOutput::Json(v))
        if v.is_null() || *v == serde_json::json!({}) || *v == serde_json::json!([]));
    let mut outcome = match result {
        Ok(StepOutput::Text(text)) => ToolOutcome::answer(text, None),
        Ok(StepOutput::Json(value)) => ToolOutcome::answer(
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
            None,
        ),
        Ok(other) => ToolOutcome::answer(format!("{other:?}"), None),
        Err(e) => ToolOutcome::refusal(format!("Tool `{name}` failed: {e}")),
    };
    outcome.audit = Some(CallAudit {
        effect,
        empty_result,
    });
    Some(outcome)
}

/// The tool_call_log outcome tag for a call, or `None` when no tool executed
/// (nothing is logged). Write-effectful successes get their own tag so
/// `sovereign reflect` can surface them as a reviewable bucket separate from
/// ordinary reads.
pub fn call_log_tag(outcome: &ToolOutcome) -> Option<&'static str> {
    let audit = outcome.audit?;
    Some(match (outcome.is_error, audit.empty_result, audit.effect) {
        (true, _, _) => "error",
        (false, true, _) => "empty_result",
        (false, false, Effect::Write) => "unapproved_write",
        (false, false, Effect::ReadWrite) => "unapproved_readwrite",
        (false, false, Effect::Read) => "success",
    })
}

/// The `tools/list` entries for the `descriptors` that `keep` admits and a
/// connection that is `read_only` or not reaches ([`effect_reachable`]), as
/// they go on the wire. Each program decides what it keeps (its exposure
/// list, a spec gate); this is the one shape of an entry.
pub fn render_tool_entries(
    descriptors: &[crate::types::ToolDescriptor],
    read_only: bool,
    keep: impl Fn(&str) -> bool,
) -> Vec<Value> {
    descriptors
        .iter()
        .filter(|desc| effect_reachable(read_only, desc.effect) && keep(&desc.id))
        .map(|desc| {
            serde_json::json!({
                "name": desc.id,
                "description": desc.description,
                "inputSchema": desc.parameters,
            })
        })
        .collect()
}

/// The `GET /mcp/stats` body: tool call counts since the registry was built.
pub fn call_stats(tools: &ToolRegistry) -> Value {
    let counts = tools.call_counts();
    let total: u64 = counts.iter().map(|(_, n)| n).sum();
    let tools_json: Vec<Value> = counts
        .into_iter()
        .map(|(name, count)| serde_json::json!({ "tool": name, "calls": count }))
        .collect();
    serde_json::json!({ "total_calls": total, "tools": tools_json })
}
