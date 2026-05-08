//! Minimal MCP stdio server. Implements the subset of the protocol Claude Code exercises:
//! `initialize`, `tools/list`, `tools/call`, plus standard JSON-RPC plumbing. Wire format is
//! LSP-style: each JSON-RPC message framed by `Content-Length` / blank line / JSON body.

mod server;

use anyhow::Result;

pub use server::{McpServer, Tool, ToolContext, ToolResult};

use crate::burn::{Ledger, ToolUseEvent};
use crate::tools;

const SERVER_NAME: &str = "relaywash";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn serve() -> Result<()> {
    let ledger = Ledger::default();
    let session_id = std::env::var("CLAUDE_SESSION_ID").unwrap_or_else(|_| "default".into());

    let mut server = McpServer::new(SERVER_NAME, SERVER_VERSION);
    for tool in tools::all() {
        server.register(tool);
    }

    server.set_post_call(Box::new(move |result: &ToolResult| {
        if let Some(meta) = &result.meta {
            let _ = ledger.record_tool_use(
                &session_id,
                ToolUseEvent {
                    tool: result.tool_name.clone(),
                    replaces: meta.replaces.clone(),
                    collapsed_calls: meta.collapsed_calls,
                },
            );
        }
    }));

    server.run()
}
