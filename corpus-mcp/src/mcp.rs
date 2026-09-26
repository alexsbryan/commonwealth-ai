// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP over stdio, through the host kit's dispatcher (phase-b pb-mcp): the
//! protocol, the framing and the envelope are the kit's, and [`Server`] is its
//! tool-host port.
//!
//! stdout carries ONLY responses. Everything else — degradations, tracing —
//! goes to stderr, or a client's parser breaks on the first log line.

use anyhow::Result;
use host_kit::mcp::{McpDispatcher, McpToolHost, ToolOutcome};
use serde_json::Value;

use crate::tools::Server;

impl McpToolHost for Server {
    fn instructions(&self) -> Option<String> {
        Some(Server::instructions(self))
    }

    fn list(&self) -> Value {
        self.tool_list()
    }

    async fn call(&self, name: &str, args: &Value) -> Option<ToolOutcome> {
        Server::call(self, name, args).await
    }
}

pub async fn serve_stdio(server: Server) -> Result<()> {
    eprintln!("corpus-mcp: ready on stdio");
    McpDispatcher::new("corpus-mcp", env!("CARGO_PKG_VERSION"), server, ())
        .serve_stdio()
        .await?;
    Ok(())
}
