//! Minimal MCP stdio server. Implements the subset of the protocol Claude Code exercises:
//! `initialize`, `tools/list`, `tools/call`, plus standard JSON-RPC plumbing. Wire format is
//! LSP-style: each JSON-RPC message framed by `Content-Length` / blank line / JSON body.

mod server;

use anyhow::Result;

pub use server::{McpServer, Tool, ToolContext, ToolResult};

use crate::tools;

const SERVER_NAME: &str = "relaywash";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn serve() -> Result<()> {
    let mut server = McpServer::new(SERVER_NAME, SERVER_VERSION);
    for tool in tools::all() {
        server.register(tool);
    }
    server.run()
}
