//! Stop hook: trigger a relayburn ingest pass so the just-ended session lands in the
//! local ledger. relayburn-sdk reads the Claude Code transcript directly (including each
//! tool_result's `_meta.replaces` / `_meta.collapsedCalls` annotations), so wash no longer
//! records its own per-call events. Failures are logged but never block the session.

use anyhow::Result;
use relayburn_sdk::{IngestOptions, ingest};
use serde_json::Value;
use std::io::Write;

use super::write_continue;

pub fn run(payload: &Value, out: &mut impl Write) -> Result<()> {
    if let Err(e) = run_ingest() {
        eprintln!("relaywash: ingest failed: {e}");
    }
    let _ = payload; // payload not needed: ingest scans the whole roots dir.
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
}
