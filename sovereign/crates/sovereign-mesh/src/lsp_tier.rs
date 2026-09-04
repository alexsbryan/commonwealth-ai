// SPDX-License-Identifier: AGPL-3.0-or-later
//! Compiler-resolved call edges for changed files, asked of a rust-analyzer
//! that is ALREADY warm.
//!
//! ## Why this exists
//!
//! The reindexer has had two tiers and a gap between them. The tree-sitter
//! overlay ([`super::reindexer::run_overlay_merge`]) refreshes symbol
//! DEFINITIONS on every save in milliseconds, but it parses one file at a
//! time and can never see that `a()` calls `b()` in another crate. The full
//! `rust-analyzer scip` export sees everything and costs a cold analysis of
//! all 54 crates — 3m42s and 15.2 GB measured on this workspace 2026-09-04 —
//! so it is gated behind a 900 s cooldown and a quiescence window. Between
//! the two, a save that adds a call is invisible to `callers` until the next
//! export.
//!
//! With a shared analyzer resident on the box (order `lspmux-shared-analyzer`)
//! the expensive half of that export — building the analysis database — has
//! already been paid by somebody else. This module asks that warm server the
//! two questions the overlay cannot answer, for the changed files only.
//!
//! ## Three decisions, and the reason each one is the cheap one
//!
//! **We reach the analyzer through `tool_path::resolve("rust-analyzer")`, the
//! same one decider the SCIP exporter uses.** Not a TCP port, not an lspmux
//! config file. If the shim is on the daemon's PATH this resolves to lspmux
//! and the server is shared; if it is not, it resolves to the real binary and
//! we get a private one. Either way this module is unchanged, and the choice
//! of "shared or not" is made in exactly one place for the whole daemon
//! (ARCH §10.6).
//!
//! **No symbol id is minted here.** rust-analyzer 1.95.0 advertises
//! `monikerProvider: null` (verified against the running server 2026-09-04),
//! so LSP cannot hand us the `rust-analyzer cargo <crate> <ver> …` descriptor
//! the exporter assigns. It does not need to: `ScipGraph::find_callers`
//! queries `refs.callee_symbol` and `resolve_symbol` matches `symbols.name` —
//! both are the BARE name, which is exactly what a `CallHierarchyItem`
//! carries. So the edges written here fill the bare-name columns and leave
//! `caller_qualified` / `callee_qualified` empty, which is the fallback
//! [`ScipSymbolRecord::qualified_name`] already documents and the tree-sitter
//! overlay already writes. A second id scheme would have been the alternative
//! and it is not needed (ARCH §7.5).
//!
//! (`lsp-to-scip` 2.0.0 was measured as a candidate client and rejected for
//! precisely that: its `symbol_string` mints `lsp . . . src.main.rs#foo` — a
//! scheme with no crate, no version, and a collision for any two same-named
//! items in one file. Its own unit test pins that shape.)
//!
//! **Files are announced, not opened.** A `workspace/didChangeWatchedFiles`
//! notification tells the server the file changed on disk. `didOpen` would
//! claim document ownership, and through lspmux the server is shared with
//! editors that may already own it.
//!
//! ## Failure is a degradation, never a wipe
//!
//! Every call here returns `Result`. The caller's contract (ARCH §18.3) is to
//! log at warn and fall back to the tree-sitter overlay's symbols-only merge,
//! which leaves existing edges alone. Nothing in this module writes to the
//! graph; it returns rows and the reindexer decides.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use corpus_engine_scip::{ScipGraph, ScipRefRecord};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// Budget for one LSP request against a WARM server. Generous enough for a
/// cold-ish server still settling, short enough that a wedged one degrades to
/// the overlay inside a single debounce window rather than stalling the
/// watcher.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Budget for the `initialize` handshake, which on a cold shared server
/// includes spawning rust-analyzer itself.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(90);

/// Errors are strings on purpose: every one of them ends up in a single
/// `tracing::warn!` and a fall back to the overlay. There is no caller that
/// branches on the variant, so an enum would be inventory (ARCH §19).
pub type Result<T> = std::result::Result<T, String>;

/// A live LSP session against the analyzer that serves this workspace.
pub struct LspTier {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
    root: PathBuf,
}

impl LspTier {
    /// Resolve, spawn and handshake. `Err` when there is no analyzer, it
    /// cannot be started, or it does not answer `initialize` in time.
    pub async fn connect(root: &Path) -> Result<Self> {
        let resolved = corpus_engine_scip::tool_path::resolve("rust-analyzer")
            .ok_or_else(|| "rust-analyzer not found on PATH or in any toolchain dir".to_string())?;

        // No arguments. That is what makes this LSP stdio mode, and — when the
        // shim is on PATH — what routes it to the shared server rather than a
        // private one.
        let mut child = Command::new(&resolved.path)
            .current_dir(root)
            .env("PATH", corpus_engine_scip::tool_path::augmented_path_env())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("spawning {}: {e}", resolved.path.display()))?;

        let stdin = child.stdin.take().ok_or("no stdin on the analyzer")?;
        let stdout = child.stdout.take().ok_or("no stdout on the analyzer")?;

        let mut tier = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 0,
            root: root.to_path_buf(),
        };

        tracing::debug!(
            target: "reindexer.lsp",
            server = %resolved.path.display(),
            via = ?resolved.via,
            root = %root.display(),
            "lsp tier: handshaking"
        );

        let params = json!({
            "processId": std::process::id(),
            "rootUri": path_to_uri(root),
            "workspaceFolders": [{
                "uri": path_to_uri(root),
                "name": root.file_name().and_then(|n| n.to_str()).unwrap_or("workspace"),
            }],
            "capabilities": {
                "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": false } },
                "textDocument": {
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                    "callHierarchy": {},
                },
            },
        });
        let result = tier
            .request_with_timeout("initialize", params, HANDSHAKE_TIMEOUT)
            .await?;

        // The one capability this tier cannot work without. Refusing here
        // means the reindexer degrades to the overlay with a named reason,
        // rather than making 2N requests that all come back empty.
        if result
            .get("capabilities")
            .and_then(|c| c.get("callHierarchyProvider"))
            .is_none_or(|v| v == &Value::Bool(false) || v.is_null())
        {
            return Err("the analyzer does not provide callHierarchy".to_string());
        }

        tier.notify("initialized", json!({})).await?;
        Ok(tier)
    }

    /// Compiler-resolved call edges ORIGINATING in `files`.
    ///
    /// Returns `(files_queried, edges)`. The files are returned so the caller
    /// can hand exactly that list to `replace_files` — the rows deleted must
    /// be the rows re-derived, or a file whose query failed would lose its
    /// edges.
    pub async fn edges_for_files(
        &mut self,
        files: &[PathBuf],
    ) -> Result<(Vec<String>, Vec<ScipRefRecord>)> {
        // Tell the server what changed on disk. One notification for the whole
        // batch; the server applies it to its vfs before answering anything
        // that follows on the same connection.
        let changes: Vec<Value> = files
            .iter()
            .map(|rel| json!({ "uri": path_to_uri(&self.root.join(rel)), "type": 2 }))
            .collect();
        self.notify(
            "workspace/didChangeWatchedFiles",
            json!({ "changes": changes }),
        )
        .await?;

        let mut queried = Vec::new();
        let mut edges = Vec::new();
        for rel in files {
            let rel_str = rel.to_string_lossy().to_string();
            let uri = path_to_uri(&self.root.join(rel));
            let file_edges = self.edges_in_file(&uri, &rel_str).await?;
            tracing::debug!(
                target: "reindexer.lsp",
                file = %rel_str,
                edges = file_edges.len(),
                "lsp tier: edges derived"
            );
            queried.push(rel_str);
            edges.extend(file_edges);
        }
        Ok((queried, edges))
    }

    async fn edges_in_file(&mut self, uri: &str, rel: &str) -> Result<Vec<ScipRefRecord>> {
        let symbols = self
            .request(
                "textDocument/documentSymbol",
                json!({ "textDocument": { "uri": uri } }),
            )
            .await?;

        // Read the source so a flat `SymbolInformation` response can still be
        // turned into identifier positions — see [`collect_definitions`].
        //
        // A file we cannot read contributes no edges rather than wrong ones,
        // and SAYS SO (ARCH §18.3). Empty content is not a neutral default
        // here: it silently costs every edge in the file when the response is
        // the flat shape, and "the analyzer had nothing to say" and "we could
        // not open the file the operator just saved" are different facts.
        let text = match std::fs::read_to_string(self.root.join(rel)) {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!(
                    target: "reindexer.lsp",
                    file = %rel,
                    error = %e,
                    "lsp tier: cannot read the saved file; deriving no edges for it"
                );
                String::new()
            }
        };
        let lines: Vec<&str> = text.lines().collect();

        let mut defs = Vec::new();
        collect_definitions(&symbols, &lines, &mut defs);

        let mut edges = Vec::new();
        for def in defs {
            let items = self
                .request(
                    "textDocument/prepareCallHierarchy",
                    json!({
                        "textDocument": { "uri": uri },
                        "position": { "line": def.line, "character": def.character },
                    }),
                )
                .await?;
            let Some(item) = items.as_array().and_then(|a| a.first()) else {
                continue;
            };
            let outgoing = self
                .request("callHierarchy/outgoingCalls", json!({ "item": item }))
                .await?;
            let Some(calls) = outgoing.as_array() else {
                continue;
            };
            for call in calls {
                let Some(callee) = call
                    .get("to")
                    .and_then(|t| t.get("name"))
                    .and_then(|n| n.as_str())
                else {
                    continue;
                };
                // `fromRanges` are the call SITES, in the caller's file — which
                // is the file we are re-deriving, so every row we write belongs
                // to the file whose rows we are about to delete.
                let sites = call.get("fromRanges").and_then(|r| r.as_array());
                for site in sites.into_iter().flatten() {
                    let (line, start_col, end_line, end_col) = range_fields(site);
                    edges.push(ScipRefRecord {
                        caller_symbol: def.name.clone(),
                        callee_symbol: callee.to_string(),
                        // Empty on purpose — see the module docs. The exporter
                        // owns SCIP descriptors; this tier never mints one.
                        caller_qualified: String::new(),
                        callee_qualified: String::new(),
                        file_path: rel.to_string(),
                        line,
                        start_col,
                        end_line,
                        end_col,
                        ref_kind: "direct".to_string(),
                    });
                }
            }
        }
        Ok(edges)
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_with_timeout(method, params, REQUEST_TIMEOUT)
            .await
    }

    async fn request_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        budget: Duration,
    ) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.write_message(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }))
        .await?;

        tokio::time::timeout(budget, self.read_response(id))
            .await
            .map_err(|_| format!("{method} timed out after {budget:?}"))?
    }

    /// Read until the response with `id` arrives.
    ///
    /// Notifications are dropped. Server-INITIATED requests are answered with
    /// a null result rather than ignored: a private analyzer will send
    /// `client/registerCapability` and `window/workDoneProgress/create` during
    /// startup, and a server waiting on a reply that never comes is a hang,
    /// not a warning. (Through lspmux they never arrive — it drops them — so
    /// this arm only runs on the private path.)
    async fn read_response(&mut self, id: i64) -> Result<Value> {
        loop {
            let msg = self.read_message().await?;
            match msg.get("id").and_then(|v| v.as_i64()) {
                Some(got) if got == id => {
                    if let Some(err) = msg.get("error") {
                        return Err(format!("server error: {err}"));
                    }
                    return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
                }
                Some(other) if msg.get("method").is_some() => {
                    self.write_message(&json!({
                        "jsonrpc": "2.0", "id": other, "result": Value::Null
                    }))
                    .await?;
                }
                _ => {}
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write_message(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    async fn write_message(&mut self, msg: &Value) -> Result<()> {
        let body = serde_json::to_vec(msg).map_err(|e| format!("encoding a request: {e}"))?;
        self.stdin
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await
            .map_err(|e| format!("writing a header: {e}"))?;
        self.stdin
            .write_all(&body)
            .await
            .map_err(|e| format!("writing a body: {e}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("flushing: {e}"))
    }

    async fn read_message(&mut self) -> Result<Value> {
        let mut length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let read = self
                .stdout
                .read_line(&mut line)
                .await
                .map_err(|e| format!("reading a header: {e}"))?;
            if read == 0 {
                return Err("the analyzer closed the connection".to_string());
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length:") {
                length = v.trim().parse().ok();
            }
        }
        let length = length.ok_or("a message arrived with no Content-Length")?;
        let mut buf = vec![0u8; length];
        self.stdout
            .read_exact(&mut buf)
            .await
            .map_err(|e| format!("reading a body: {e}"))?;
        serde_json::from_slice(&buf).map_err(|e| format!("decoding a message: {e}"))
    }

    /// Politely end the session. Best-effort: `kill_on_drop` is the backstop.
    pub async fn shutdown(mut self) {
        let _ = self.request("shutdown", Value::Null).await;
        let _ = self.notify("exit", Value::Null).await;
        let _ = self.child.wait().await;
    }
}

/// One definition found in a document, with the position to ask
/// `prepareCallHierarchy` about.
struct Definition {
    name: String,
    line: i64,
    character: i64,
}

/// Walk a `documentSymbol` response into the definitions worth asking about.
///
/// Both response shapes the protocol allows have to be handled, and NOT as
/// defensive coding — which one arrives is not ours to choose. Through lspmux
/// the language server is configured by whichever client completed the
/// handshake FIRST, and every later client is handed a replay of that
/// server's `initialize` result. Declaring
/// `hierarchicalDocumentSymbolSupport` therefore buys nothing when an editor
/// got there first: measured against the shared analyzer on 2026-09-04, a
/// client declaring it received the FLAT `SymbolInformation[]` shape anyway.
///
/// The shapes differ in the one field that matters here:
///
/// - `DocumentSymbol[]` carries `selectionRange` — the identifier itself, and
///   exactly where `prepareCallHierarchy` expects the cursor — plus `children`,
///   which is where the methods inside an `impl` live.
/// - `SymbolInformation[]` carries only `location.range`, the item's WHOLE
///   span. Its start is `pub`, or a `#[derive(...)]` attribute above the item,
///   and `prepareCallHierarchy` at that position returns nothing at all.
///
/// So when the identifier position is missing, find it: scan the item's own
/// line range for the name as a whole word. `src` is the file's lines.
fn collect_definitions(node: &Value, src: &[&str], out: &mut Vec<Definition>) {
    match node {
        Value::Array(items) => {
            for item in items {
                collect_definitions(item, src, out);
            }
        }
        Value::Object(map) => {
            let name = map.get("name").and_then(|n| n.as_str()).unwrap_or_default();
            if !name.is_empty() {
                if let Some(pos) = map.get("selectionRange").and_then(|r| r.get("start")) {
                    out.push(Definition {
                        name: name.to_string(),
                        line: pos.get("line").and_then(|v| v.as_i64()).unwrap_or(0),
                        character: pos.get("character").and_then(|v| v.as_i64()).unwrap_or(0),
                    });
                } else if let Some(range) = map
                    .get("location")
                    .and_then(|l| l.get("range"))
                    .or_else(|| map.get("range"))
                {
                    let (start, end) = (
                        range
                            .get("start")
                            .and_then(|p| p.get("line"))
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0),
                        range
                            .get("end")
                            .and_then(|p| p.get("line"))
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0),
                    );
                    if let Some((line, character)) = find_identifier(src, name, start, end) {
                        out.push(Definition {
                            name: name.to_string(),
                            line,
                            character,
                        });
                    }
                }
            }
            if let Some(children) = map.get("children") {
                collect_definitions(children, src, out);
            }
        }
        _ => {}
    }
}

/// First whole-word occurrence of `name` in `src[start..=end]`, as an LSP
/// (line, character).
///
/// Whole-word, because a substring match puts the cursor inside a longer
/// identifier and `prepareCallHierarchy` resolves the wrong item — `run` would
/// find the `run` inside `run_worker` on an earlier line and derive that
/// function's callees under this function's name.
///
/// Character offsets count UTF-16 code units: the server advertises
/// `positionEncoding: utf-16`, so a line containing a non-BMP character before
/// the identifier would otherwise point past it.
fn find_identifier(src: &[&str], name: &str, start: i64, end: i64) -> Option<(i64, i64)> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let lo = start.max(0) as usize;
    let hi = (end.max(start) as usize).min(src.len().saturating_sub(1));
    for (offset, line) in src.get(lo..=hi)?.iter().enumerate() {
        let mut from = 0usize;
        while let Some(at) = line[from..].find(name) {
            let at = from + at;
            let before_ok = line[..at].chars().next_back().is_none_or(|c| !is_word(c));
            let after_ok = line[at + name.len()..]
                .chars()
                .next()
                .is_none_or(|c| !is_word(c));
            if before_ok && after_ok {
                let character = line[..at].encode_utf16().count() as i64;
                return Some(((lo + offset) as i64, character));
            }
            from = at + name.len();
            if from >= line.len() {
                break;
            }
        }
    }
    None
}

/// `(line, start_col, end_line, end_col)` from an LSP range, 0-based, matching
/// the columns [`ScipRefRecord`] records.
fn range_fields(range: &Value) -> (i32, i32, i32, i32) {
    let get = |end: &str, field: &str| -> i32 {
        range
            .get(end)
            .and_then(|p| p.get(field))
            .and_then(|v| v.as_i64())
            .unwrap_or(-1) as i32
    };
    (
        get("start", "line"),
        get("start", "character"),
        get("end", "line"),
        get("end", "character"),
    )
}

/// A `file://` URI for an absolute path.
///
/// Deliberately minimal rather than a `url` dependency: the only paths this
/// module builds URIs for are absolute workspace paths that already came from
/// the filesystem.
fn path_to_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Ask the warm analyzer for the CALL edges rooted in `changed`, and add them.
///
/// ## The one property that matters
///
/// This pass cannot remove an edge. Not "does not" — cannot: its only write is
/// [`ScipGraph::add_call_edges_for`], which executes no `DELETE`. Every failure
/// mode therefore degrades to exactly the graph that existed a moment ago:
///
/// - no analyzer, or a handshake that times out  -> nothing written, warn
/// - a request that errors or times out mid-batch -> nothing written, warn
/// - an analyzer that answers `[]` because it is still loading the workspace,
///   or because the file is outside it                 -> nothing written
/// - a file that genuinely contains no calls           -> nothing written
///
/// The last two are indistinguishable over LSP, which is the reason the write
/// is additive rather than a per-file replace: a replace keyed on "the server
/// said nothing" would empty the file's edges, and the file that gets emptied
/// is the one the operator is editing right now (ARCH §18.3).
///
/// The session is borrowed mutably and cleared on failure, so a dead analyzer
/// costs one handshake attempt per save rather than one per file.
pub async fn run_edge_pass(
    tier: &mut Option<LspTier>,
    graph: &ScipGraph,
    corpus_id: &str,
    root: &Path,
    changed: &[PathBuf],
) {
    if changed.is_empty() {
        return;
    }
    if tier.is_none() {
        match LspTier::connect(root).await {
            Ok(t) => *tier = Some(t),
            Err(e) => {
                tracing::warn!(
                    target: "reindexer.lsp",
                    error = %e,
                    "lsp tier unavailable; symbol defs are fresh, edges lag the full export"
                );
                return;
            }
        }
    }
    let Some(session) = tier.as_mut() else {
        return;
    };

    let edges = match session.edges_for_files(changed).await {
        Ok((_queried, edges)) => edges,
        Err(e) => {
            // Drop the session: a request that failed mid-stream leaves the
            // read side out of sync with the ids we are waiting on, so every
            // later request on it would read somebody else's response.
            *tier = None;
            tracing::warn!(
                target: "reindexer.lsp",
                error = %e,
                "lsp tier request failed; existing edges preserved, full export will correct"
            );
            return;
        }
    };

    match graph.add_call_edges_for(corpus_id, &edges).await {
        Ok(inserted) => tracing::debug!(
            target: "reindexer.lsp",
            files = changed.len(),
            derived = edges.len(),
            inserted,
            "lsp tier: compiler-resolved call edges added"
        ),
        Err(e) => tracing::warn!(
            target: "reindexer.lsp",
            error = %e,
            "lsp tier edge insert failed (rolled back, graph preserved)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hierarchical_document_symbols_yield_methods_inside_impls() {
        // The shape rust-analyzer actually sends: an `impl` whose methods are
        // children. A walker that only read the top level would derive edges
        // for no method in the workspace.
        let response = json!([
            {
                "name": "Engine",
                "selectionRange": { "start": { "line": 3, "character": 11 } },
                "children": [
                    { "name": "new", "selectionRange": { "start": { "line": 4, "character": 7 } } },
                    { "name": "run", "selectionRange": { "start": { "line": 9, "character": 7 } } }
                ]
            },
            { "name": "main", "selectionRange": { "start": { "line": 20, "character": 3 } } }
        ]);
        let mut defs = Vec::new();
        collect_definitions(&response, &[], &mut defs);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["Engine", "new", "run", "main"]);
        assert_eq!((defs[2].line, defs[2].character), (9, 7));
    }

    #[test]
    fn flat_symbol_information_resolves_the_identifier_not_the_item_start() {
        // The shape the shared analyzer actually returned on 2026-09-04. Its
        // range starts on the `#[inline]` line, and prepareCallHierarchy there
        // returns nothing — so the identifier has to be found in the source.
        let src = vec![
            "// preamble",
            "#[inline]",
            "pub fn helper(x: u32) -> u32 {",
            "    x + 1",
            "}",
        ];
        let response = json!([{
            "name": "helper",
            "location": { "range": {
                "start": { "line": 1, "character": 0 },
                "end": { "line": 4, "character": 1 }
            }}
        }]);
        let mut defs = Vec::new();
        collect_definitions(&response, &src, &mut defs);
        assert_eq!(defs.len(), 1);
        assert_eq!(
            (defs[0].line, defs[0].character),
            (2, 7),
            "must land on the identifier, not the attribute above it"
        );
    }

    #[test]
    fn a_name_inside_a_longer_identifier_is_not_the_definition() {
        // `run` occurs inside `run_worker` two lines earlier. Landing there
        // would derive run_worker's callees and file them under `run`.
        let src = vec!["fn run_worker() {}", "", "pub fn run() {}"];
        let response = json!([{
            "name": "run",
            "location": { "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 2, "character": 15 }
            }}
        }]);
        let mut defs = Vec::new();
        collect_definitions(&response, &src, &mut defs);
        assert_eq!((defs[0].line, defs[0].character), (2, 7));
    }

    #[test]
    fn an_identifier_that_is_not_in_the_source_yields_no_definition() {
        // Better to derive nothing than to point the call-hierarchy request at
        // an arbitrary position and file somebody else's callees here.
        let src = vec!["fn other() {}"];
        let response = json!([{
            "name": "helper",
            "location": { "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 13 }
            }}
        }]);
        let mut defs = Vec::new();
        collect_definitions(&response, &src, &mut defs);
        assert!(defs.is_empty());
    }

    #[test]
    fn character_offsets_are_utf16_code_units() {
        // The server advertises positionEncoding: utf-16. A non-BMP char is
        // ONE Rust char and TWO UTF-16 units; counting chars would point the
        // cursor one unit short of the identifier.
        let src = vec!["// \u{1F600} marker", "fn helper() {}"];
        let response = json!([{
            "name": "helper",
            "location": { "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 1, "character": 14 }
            }}
        }]);
        let mut defs = Vec::new();
        collect_definitions(&response, &src, &mut defs);
        assert_eq!((defs[0].line, defs[0].character), (1, 3));
    }

    #[test]
    fn a_range_with_no_end_records_the_unrecorded_sentinel() {
        // ScipRefRecord documents -1 as "unrecorded", and find_callers reads
        // end_col back. Inventing a 0 would claim a span that is not there.
        let (line, start_col, end_line, end_col) =
            range_fields(&json!({ "start": { "line": 7, "character": 4 } }));
        assert_eq!((line, start_col), (7, 4));
        assert_eq!((end_line, end_col), (-1, -1));
    }

    #[test]
    fn uris_percent_encode_what_a_path_may_legally_contain() {
        assert_eq!(path_to_uri(Path::new("/a/b.rs")), "file:///a/b.rs");
        assert_eq!(path_to_uri(Path::new("/a b/c.rs")), "file:///a%20b/c.rs");
    }
}
