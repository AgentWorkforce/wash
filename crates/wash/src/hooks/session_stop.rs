//! Stop hook: ingest the just-ended session into the local relayburn ledger by appending
//! `session_end` and (when a transcript is available) a `session_summary` event with
//! per-turn token totals. Any failure is logged but does not block the session.

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::write_continue;
use crate::burn::Ledger;

pub fn run(payload: &Value, out: &mut impl Write) -> Result<()> {
    run_with(&Ledger::default(), payload, out)
}

fn run_with(ledger: &Ledger, payload: &Value, out: &mut impl Write) -> Result<()> {
    let session_id = resolve_session_id(payload);
    let transcript_path = payload
        .get("transcript_path")
        .or_else(|| payload.get("transcriptPath"))
        .and_then(|v| v.as_str());

    if let Err(e) = ledger.record_session_end(&session_id, transcript_path) {
        eprintln!("relaywash: ingest failed: {e}");
    }

    // Best-effort transcript summarization. The schema follows what wash#13 calls for:
    // cache hit-rate inputs (cacheReadTokens / cacheCreationTokens) and uncached IO.
    if let Some(path) = transcript_path {
        if let Some(summary) = summarize_transcript(Path::new(path)) {
            if let Err(e) = append_session_summary(ledger, &session_id, summary) {
                eprintln!("relaywash: session_summary append failed: {e}");
            }
        }
    }

    write_continue(out)
}

fn resolve_session_id(payload: &Value) -> String {
    payload
        .get("session_id")
        .or_else(|| payload.get("sessionId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            let ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            format!("session-{ms}")
        })
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub(crate) struct TranscriptSummary {
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(rename = "cacheCreationTokens")]
    pub cache_creation_tokens: u64,
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    pub turns: u32,
}

/// Walk the transcript JSONL; sum the four token fields wash#13's A/B comparison cares
/// about. Returns None if the file can't be read or has no usage info.
pub(crate) fn summarize_transcript(path: &Path) -> Option<TranscriptSummary> {
    let raw = std::fs::read_to_string(path).ok()?;
    let mut s = TranscriptSummary::default();
    for line in raw.lines() {
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(usage) = find_usage(&v) {
            s.cache_read_tokens += usage
                .get("cache_read_input_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            s.cache_creation_tokens += usage
                .get("cache_creation_input_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            s.input_tokens += usage.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
            s.output_tokens += usage.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
            s.turns += 1;
        }
    }
    if s.turns == 0 {
        return None;
    }
    Some(s)
}

/// Locate a `usage` object inside an arbitrary turn record. Top-level or under common
/// nests (`message.usage`, `response.usage`).
fn find_usage(turn: &Value) -> Option<&Value> {
    if let Some(u) = turn.get("usage") {
        return Some(u);
    }
    for nest in ["message", "response", "result"] {
        if let Some(u) = turn.get(nest).and_then(|v| v.get("usage")) {
            return Some(u);
        }
    }
    None
}

fn append_session_summary(
    ledger: &Ledger,
    session_id: &str,
    summary: TranscriptSummary,
) -> Result<()> {
    let dir = ledger.home().join("sessions");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{session_id}.jsonl"));
    let line = serde_json::json!({
        "ts": SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0),
        "kind": "session_summary",
        "cacheReadTokens": summary.cache_read_tokens,
        "cacheCreationTokens": summary.cache_creation_tokens,
        "inputTokens": summary.input_tokens,
        "outputTokens": summary.output_tokens,
        "turns": summary.turns,
    });
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn writes_session_end_event() {
        let tmp = TempDir::new().unwrap();
        let l = Ledger::new(tmp.path());
        let mut buf = Vec::new();
        run_with(
            &l,
            &json!({"session_id": "abc", "transcript_path": "/nonexistent.jsonl"}),
            &mut buf,
        )
        .unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"continue\":true"));
        let events = l.read_session("abc");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "session_end");
    }

    #[test]
    fn missing_session_id_synthesizes_one() {
        let tmp = TempDir::new().unwrap();
        let l = Ledger::new(tmp.path());
        let mut buf = Vec::new();
        run_with(&l, &json!({}), &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"continue\":true"));
    }

    #[test]
    fn summarize_transcript_sums_token_fields() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("transcript.jsonl");
        let body = [
            json!({"usage": {"cache_read_input_tokens": 100, "cache_creation_input_tokens": 50, "input_tokens": 10, "output_tokens": 5}}),
            json!({"message": {"usage": {"cache_read_input_tokens": 200, "input_tokens": 0, "output_tokens": 8}}}),
            json!({"role": "user"}),
        ]
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        std::fs::write(&path, body).unwrap();
        let s = summarize_transcript(&path).unwrap();
        assert_eq!(s.cache_read_tokens, 300);
        assert_eq!(s.cache_creation_tokens, 50);
        assert_eq!(s.input_tokens, 10);
        assert_eq!(s.output_tokens, 13);
        assert_eq!(s.turns, 2);
    }

    #[test]
    fn appends_session_summary_when_transcript_available() {
        let tmp = TempDir::new().unwrap();
        let transcript = tmp.path().join("t.jsonl");
        let body = json!({"usage": {"cache_read_input_tokens": 100, "input_tokens": 5, "output_tokens": 5}}).to_string();
        std::fs::write(&transcript, body).unwrap();
        let l = Ledger::new(tmp.path());
        let mut buf = Vec::new();
        run_with(
            &l,
            &json!({
                "session_id": "abc",
                "transcript_path": transcript.to_string_lossy()
            }),
            &mut buf,
        )
        .unwrap();
        let events = l.read_session("abc");
        assert_eq!(events.len(), 2);
        let summary = events.iter().find(|e| e["kind"] == "session_summary").unwrap();
        assert_eq!(summary["cacheReadTokens"], 100);
        assert_eq!(summary["turns"], 1);
    }

    #[test]
    fn no_session_summary_when_transcript_missing() {
        let tmp = TempDir::new().unwrap();
        let l = Ledger::new(tmp.path());
        let mut buf = Vec::new();
        run_with(&l, &json!({"session_id": "no-trans"}), &mut buf).unwrap();
        let events = l.read_session("no-trans");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "session_end");
    }
}
