// SPDX-License-Identifier: AGPL-3.0-or-later
//! JSON-RPC 2.0 envelope — the transport contract MCP rides on.
//!
//! Home per operator decision 2026-08-21: layer 0 is the contract, and a
//! JSON-RPC 2.0 envelope is a wire contract by definition. It names nothing
//! above itself — `serde` and `serde_json::Value` only — so the whole
//! specification lives here and both the daemon's MCP router
//! (`sovereign_mesh::mcp_router`) and the standalone server's
//! (`sovereign_server::routes_mcp`) import it instead of re-declaring it.
//!
//! Until this module existed the two declared their own copies and had
//! already drifted: the server modelled the request id as a bare `Value`
//! defaulted to `Null`, the daemon as `Option<Value>`, and only the server
//! carried the spec's optional error `data` member. Both drifts are
//! adjudicated here — see [`JsonRpcRequest::id`] and [`JsonRpcError::data`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only protocol version this envelope speaks.
pub const JSONRPC_VERSION: &str = "2.0";

fn jsonrpc_version() -> String {
    JSONRPC_VERSION.to_string()
}

/// Inbound JSON-RPC request or notification.
///
/// `params` stays opaque: each method parses its own shape. `jsonrpc` is
/// deserialized so the envelope round-trips, but its value is not validated
/// — the spec says `"2.0"`, and we accept anything for forward-compat with
/// clients that omit it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// Protocol version as the peer sent it. Absent → [`JSONRPC_VERSION`].
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    /// Correlation id, or `None` for a notification.
    ///
    /// ADJUDICATED 2026-08-21 in favour of `Option<Value>` over the bare
    /// `Value` + `serde(default)` the standalone server used. The two are
    /// BEHAVIOURALLY EQUIVALENT on today's wire and the reason is not the one
    /// you would guess: `Option<T>` deserializes an explicit `"id": null` to
    /// `None`, exactly as it does an omitted member, so neither shape can tell
    /// a null-id call from a notification. The first draft of this comment
    /// claimed otherwise and the test below is what caught it.
    ///
    /// `Option` wins on the two things that are true:
    ///
    /// - It states the domain in the type. `Value` + `is_null()` keeps "Null
    ///   means there was no id" in a comment, where it can be forgotten; a
    ///   caller who forgets replies to a notification with `"id": null`, which
    ///   §4.1 forbids. `let Some(id) = req.id else { return None }` cannot be
    ///   forgotten — the compiler asks.
    /// - It can express the distinction later; `Value` cannot express it at
    ///   all. §4 discourages but permits a null id in a Request, and a client
    ///   that sends one should get a Response. Nothing in this workspace sends
    ///   one, so that is a documented limitation and not a fix: the day a
    ///   client does, `#[serde(default, deserialize_with = "…")]` mapping a
    ///   present `null` to `Some(Value::Null)` is the whole change.
    #[serde(default)]
    pub id: Option<Value>,
    /// Method name, e.g. `tools/call`.
    pub method: String,
    /// Method-specific parameters, opaque at this layer.
    #[serde(default)]
    pub params: Option<Value>,
}

/// Outbound JSON-RPC response. Exactly one of `result` / `error` is set.
///
/// `id` is NOT optional here, and that asymmetry with [`JsonRpcRequest`] is
/// deliberate: §5 requires every Response to carry an id, and `Null` when the
/// id of the offending request could not be determined (a parse failure, an
/// empty batch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// Always [`JSONRPC_VERSION`] on anything this crate constructs.
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    /// The id of the request being answered, or `Null` when unknown.
    pub id: Value,
    /// Present on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Present on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC error object (§5.1).
///
/// Reserved codes: `-32700` parse error, `-32600` invalid request, `-32601`
/// method not found, `-32602` invalid params, `-32603` internal error.
///
/// MCP tool-call failures that the agent should SEE rather than break on do
/// not travel here — they ride inside a successful `result` as
/// `CallToolResult { isError: true }`. That is MCP's distinction between
/// "transport failed" and "the tool said no", and erasing it turns a
/// recoverable tool error into a dead client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// Spec or application error code.
    pub code: i32,
    /// Human-readable, single-sentence description.
    pub message: String,
    /// Optional structured detail.
    ///
    /// ADJUDICATED 2026-08-21: kept. §5.1 defines `data` as an OPTIONAL
    /// member, the server's copy carried it and the daemon's did not.
    /// `skip_serializing_if` means a `None` serializes to exactly the daemon's
    /// old two-field object, so adopting the superset is byte-identical on the
    /// wire for every error either router emits today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    /// A success response carrying `value` as the result.
    pub fn result(id: Value, value: Value) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id,
            result: Some(value),
            error: None,
        }
    }

    /// An error response with no structured `data`.
    pub fn error(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self::error_with_data(id, code, message, None)
    }

    /// An error response carrying the spec's optional `data` member.
    pub fn error_with_data(
        id: Value,
        code: i32,
        message: impl Into<String>,
        data: Option<Value>,
    ) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_elides_the_absent_half() {
        let ok = JsonRpcResponse::result(Value::from(1), Value::Null);
        let s = serde_json::to_string(&ok).unwrap();
        assert!(s.contains("\"result\""));
        assert!(!s.contains("\"error\""));

        let err = JsonRpcResponse::error(Value::from(2), -32601, "method not found");
        let s = serde_json::to_string(&err).unwrap();
        assert!(s.contains("\"error\""));
        assert!(!s.contains("\"result\""));
        // The daemon's copy had no `data` field at all. Adopting the server's
        // superset must not add a member to the wire when it is None.
        assert!(!s.contains("\"data\""), "None data must not serialize: {s}");
    }

    /// Pins what `Option<Value>` ACTUALLY does with an id, including the part
    /// that is a limitation rather than a feature.
    ///
    /// An omitted `id` is a notification and deserializes to `None` — that is
    /// the case both routers dispatch on and it is correct. An explicit
    /// `"id": null` ALSO deserializes to `None`, because that is how serde
    /// treats `Option`, so a null-id call is dispatched as a notification and
    /// receives no reply. §4 discourages null ids and no client in this
    /// workspace sends one; if one ever does, this test is where the
    /// expectation changes and `deserialize_with` on the field is the fix.
    #[test]
    fn an_omitted_id_is_a_notification_and_an_explicit_null_reads_as_one_too() {
        let notification: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap();
        assert!(notification.id.is_none(), "no id member -> no reply");

        let explicit_null: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#).unwrap();
        assert!(
            explicit_null.id.is_none(),
            "serde folds a present null into None; the routers cannot tell it \
             from an omitted id, and this is the documented limitation"
        );

        let call: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#).unwrap();
        assert_eq!(call.id, Some(Value::from(7)), "a real id must survive");
    }

    #[test]
    fn request_defaults_the_version_and_params() {
        let req: JsonRpcRequest = serde_json::from_str(r#"{"id":7,"method":"ping"}"#).unwrap();
        assert_eq!(req.jsonrpc, JSONRPC_VERSION);
        assert!(req.params.is_none());
    }

    /// The envelope is a contract, so it has to travel in both directions: a
    /// client builds a request and parses a response using the same types the
    /// routers serve them with.
    #[test]
    fn envelope_round_trips_both_directions() {
        let req = JsonRpcRequest {
            jsonrpc: jsonrpc_version(),
            id: Some(Value::from(3)),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({ "name": "symbols" })),
        };
        let wire = serde_json::to_value(&req).unwrap();
        assert_eq!(wire["jsonrpc"], "2.0");
        let back: JsonRpcRequest = serde_json::from_value(wire).unwrap();
        assert_eq!(back.method, "tools/call");

        let resp = JsonRpcResponse::error_with_data(
            Value::from(3),
            -32602,
            "invalid params",
            Some(serde_json::json!({ "field": "name" })),
        );
        let wire = serde_json::to_value(&resp).unwrap();
        let back: JsonRpcResponse = serde_json::from_value(wire).unwrap();
        let err = back.error.expect("error half survives the round trip");
        assert_eq!(err.code, -32602);
        assert_eq!(err.data.unwrap()["field"], "name");
    }
}
