//! Hook subcommands. Each entry reads a JSON payload from stdin, emits a JSON response on
//! stdout, and exits 0. Wired into `hooks/hooks.json` via the `wash hook <kind>` subcommand.

mod builtin_block;
mod edit_batching_nudge;
mod post_tool_observe;
mod session_start;
mod session_stop;
mod tool_redirect;

use anyhow::{Result, anyhow};
use std::io::{Read, Write};

pub fn run(kind: &str) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut buf = String::new();
    let _ = stdin.lock().read_to_string(&mut buf);
    let mut out = stdout.lock();
    let payload: serde_json::Value = if buf.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(&buf).unwrap_or(serde_json::Value::Null)
    };
    dispatch(kind, &payload, &mut out)
}

pub fn dispatch(kind: &str, payload: &serde_json::Value, out: &mut impl Write) -> Result<()> {
    match kind {
        "builtin-block" => builtin_block::run(payload, out),
        "tool-redirect" => tool_redirect::run(payload, out),
        "edit-batching-nudge" => edit_batching_nudge::run(payload, out),
        "post-tool-observe" => post_tool_observe::run(payload, out),
        "session-start" => session_start::run(payload, out),
        "session-stop" => session_stop::run(payload, out),
        other => Err(anyhow!("unknown hook kind: {other}")),
    }
}

pub(crate) fn write_continue(out: &mut impl Write) -> Result<()> {
    writeln!(out, "{}", serde_json::json!({"continue": true}))?;
    Ok(())
}

pub(crate) fn write_json(out: &mut impl Write, value: &serde_json::Value) -> Result<()> {
    writeln!(out, "{value}")?;
    Ok(())
}
