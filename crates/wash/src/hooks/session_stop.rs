//! Stop hook: trigger a relayburn ingest pass so the just-ended session lands in the
//! local ledger. relayburn-sdk reads the Claude Code transcript directly (including each
//! tool_result's `_meta.replaces` / `_meta.collapsedCalls` annotations), so wash no longer
//! records its own per-call events. Failures are logged but never block the session.
//!
//! In addition to relayburn's session-level summary, this hook now drives wash's own
//! per-turn accounting ledger (see `crate::accounting`) which captures model, cost,
//! cache, and category breakdowns at message-id granularity. Both steps are best-effort:
//! errors are logged and the hook still emits `continue: true` so the session never
//! blocks on telemetry.

use anyhow::Result;
use relayburn_sdk::{IngestOptions, ingest};
use serde_json::Value;
use std::io::Write;

use super::write_continue;
use crate::accounting::ingest_transcript;

pub fn run(payload: &Value, out: &mut impl Write) -> Result<()> {
    if let Err(e) = run_ingest() {
        eprintln!("relaywash: ingest failed: {e}");
    }
    if let Err(e) = ingest_transcript(payload) {
        eprintln!("relaywash: per-turn accounting ingest failed: {e}");
    }
    write_continue(out)
}

fn run_ingest() -> Result<()> {
    // `relayburn_sdk::ingest` became synchronous in 2.8.5 — no runtime needed.
    ingest(IngestOptions::default())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn writes_continue_even_when_ingest_path_is_unreachable() {
        // Drive the hook with no transcript and no env override. ingest() may fail (no
        // ~/.claude/projects in CI) but the hook must still emit `continue:true` so the
        // session isn't blocked.
        let mut buf = Vec::new();
        run(&json!({"session_id": "test"}), &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"continue\":true"));
    }

    #[test]
    fn writes_continue_when_transcript_path_is_garbage() {
        // Per-turn accounting must tolerate a bogus transcript path silently.
        let mut buf = Vec::new();
        run(
            &json!({
                "session_id": "test",
                "transcript_path": "/this/does/not/exist.jsonl",
            }),
            &mut buf,
        )
        .unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"continue\":true"));
    }
}
