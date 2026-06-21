//! Hook subcommands. Each entry reads a JSON payload from stdin, emits a JSON response on
//! stdout, and exits 0. Wired into `hooks/hooks.json` via the `wash hook <kind>` subcommand.

mod builtin_block;
mod compaction;
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
        "pre-compact" => compaction::run_pre(payload, out),
        "post-compact" => compaction::run_post(payload, out),
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

/// Look up a hook-payload field by its `snake_case` name, falling back to the
/// `camelCase` spelling Claude Code sometimes emits. Returns the raw `Value` so each
/// caller keeps its own typing, default, and post-processing — centralizing only the
/// two-spelling fallback so the spellings can't drift apart as fields are added.
pub(crate) fn payload_field<'a>(
    payload: &'a serde_json::Value,
    snake: &str,
    camel: &str,
) -> Option<&'a serde_json::Value> {
    payload.get(snake).or_else(|| payload.get(camel))
}

/// Map a tool name as Claude Code reports it to its bare relaywash name. The harness
/// may surface a relaywash tool as either `mcp__relaywash__Read` or `relaywash__Read`
/// depending on context; observation-side code (the categorizer, the observe log) wants
/// the bare `Read`. Names with no relaywash prefix pass through unchanged.
///
/// Deliberately strips ONLY relaywash prefixes — the categorizer's own `canonical()`
/// additionally folds `mcp__github__`, which changes categorization output and is a
/// product decision, not a mechanical rename. Keep that separate.
pub(crate) fn bare_relaywash_name(name: &str) -> &str {
    name.strip_prefix("mcp__relaywash__")
        .or_else(|| name.strip_prefix("relaywash__"))
        .unwrap_or(name)
}

/// Map a session id to a filename-safe slug. Hooks compose paths like
/// `${RELAYBURN_HOME}/observe/<session>.json`; without sanitization a crafted id like
/// `../../etc/passwd` would let the harness write outside the intended directory.
pub(crate) fn sanitize_session_id(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_path_separators() {
        assert_eq!(sanitize_session_id("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitize_session_id("session-abc_123"), "session-abc_123");
        assert_eq!(sanitize_session_id(""), "unknown");
        assert_eq!(sanitize_session_id("//"), "__");
    }
}
