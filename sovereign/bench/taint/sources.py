#!/usr/bin/env python3
"""The input layer: a structural taxonomy of untrusted ingress.

Precision principle: a parameter is NOT a source by default. It is a source
ONLY when a structural signal says untrusted data enters there. Framework-
injected params (State/Extension/AppHandle/Window/DI) are explicitly excluded
even on a handler — the server controls them, the user does not.

Kinds (highest precision first):
  http_extractor    param typed Json<>/Query<>/Path<>/Form<>/Bytes/Multipart   [high]
  tauri_command_arg non-framework param of a #[tauri::command] fn (frontend)   [high]
  peer_bytes        &[u8]/Bytes param in the mesh transport/gossip layer        [high]
  deserialize_ext   from_slice/from_reader(&x) over external bytes              [med]
  cli_arg           std::env::args()/clap parse                                 [low]
"""
import re

# Framework-injected / server-controlled — NEVER a source, even on a handler.
FRAMEWORK_TYPE = re.compile(
    r"\bState\s*<|\bExtension\s*<|\bAppHandle\b|\bWindow\b|\btauri::(State|Window|AppHandle|Manager)"
    r"|ConnectInfo\s*<|\bAppState\b|\bDbConn\b|\bPgPool\b|\bSqlitePool\b|\bArc<\s*AppState")

# axum / http extractors — untrusted network input by construction.
HTTP_EXTRACTOR = re.compile(
    r"\bJson\s*<|\bQuery\s*<|\bPath\s*<|\bForm\s*<|\bBytes\b|\bBytesMut\b|\bMultipart\b"
    r"|\bRawBody\b|\bWebSocketUpgrade\b|\bTypedHeader\s*<|\bRawQuery\b")

# byte/stream deserialization of *external* data (gated: not from_str on consts).
EXTERNAL_DESERIALIZE = re.compile(
    r"from_slice|from_reader|serde_json::from_slice|serde_json::from_reader|"
    r"rmp_serde::from|bincode::deserialize|ciborium::from_reader")

# mesh transport crates where inbound bytes are peer-controlled.
PEER_MODULE = re.compile(r"commonwealth-transport|commonwealth-discovery|/gossip|/relay")

def is_framework_param(type_str):
    return bool(FRAMEWORK_TYPE.search(type_str))

def classify_param(param_name, type_str, *, is_tauri_cmd, rel):
    """Return (kind, confidence) if this parameter is an untrusted source, else None."""
    if is_framework_param(type_str):
        return None
    if HTTP_EXTRACTOR.search(type_str):
        return ("http_extractor", "high")
    if is_tauri_cmd:
        # a non-framework param of a tauri command is frontend-deserialized input;
        # exclude zero-info unit/marker types.
        if type_str.strip() in ("", "()"):
            return None
        return ("tauri_command_arg", "high")
    if PEER_MODULE.search(rel) and re.search(r"&\s*\[\s*u8\s*\]|\bBytes\b|\bVec<\s*u8", type_str):
        return ("peer_bytes", "high")
    return None

def source_call_kind(expr_text):
    """Return (kind, confidence) if an initializer expr is an external-data source call."""
    if EXTERNAL_DESERIALIZE.search(expr_text):
        return ("deserialize_ext", "med")
    return None
