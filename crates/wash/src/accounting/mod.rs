//! Per-turn accounting ingestion.
//!
//! The Stop hook calls [`ingest_transcript`] after [`relayburn_sdk::ingest`] runs.
//! We parse the Claude Code transcript JSONL, emit one structured record per
//! assistant turn that carries usage data, and append the records to a JSONL
//! ledger under `${RELAYBURN_HOME}/turns/<session-id>.jsonl`. A small state file
//! tracks the highest message id we've seen so repeated Stop-hook runs only
//! append new turns.
//!
//! Storage choice: JSONL (no new dependency). A future migration to SQLite is
//! tracked separately — aggregation queries will be easier there, but the
//! immediate need is to *capture* the data without taking on `rusqlite`.

pub mod categorize;
pub mod pricing;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::hooks::sanitize_session_id;
use crate::profile::{current_repo_key, ledger_home};

use categorize::classify;
use pricing::{Usage, estimate};

const TURNS_SUBDIR: &str = "turns";
const STATE_SUBDIR: &str = "turns-state";
const TURN_SCHEMA_VERSION: u32 = 1;

/// A single accounting record. Stored as one JSONL line per assistant turn.
///
/// On-disk field names are stable — downstream aggregation reads these as
/// canonical keys.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnRecord {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(rename = "messageId")]
    pub message_id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "projectKey")]
    pub project_key: String,
    pub model: String,
    pub timestamp: String,
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "cacheCreationTokens")]
    pub cache_creation_tokens: u64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(rename = "estimatedCostUsd")]
    pub estimated_cost_usd: f64,
    pub tools: Vec<String>,
    pub category: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct IngestState {
    /// Set of message ids we have already emitted for this session. Provides
    /// idempotent ingestion: re-running the Stop hook over the same transcript
    /// is a no-op.
    #[serde(rename = "seenMessageIds", default)]
    seen_message_ids: Vec<String>,
}

/// Public entry point. Called from the Stop hook with the raw payload that
/// Claude Code passes to its hooks. Best-effort: any failure is logged and the
/// hook continues. The returned `Result` is for testability; callers ignore the
/// error.
pub fn ingest_transcript(payload: &Value) -> Result<usize> {
    let home = ledger_home();
    ingest_with(&home, payload)
}

/// Test-friendly variant — `home` is the equivalent of `${RELAYBURN_HOME}`.
pub fn ingest_with(home: &Path, payload: &Value) -> Result<usize> {
    let transcript_path = match payload
        .get("transcript_path")
        .or_else(|| payload.get("transcriptPath"))
        .and_then(|v| v.as_str())
    {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => return Ok(0), // No transcript -> nothing to do.
    };
    if !transcript_path.exists() {
        return Ok(0);
    }

    let raw_session = payload
        .get("session_id")
        .or_else(|| payload.get("sessionId"))
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let session_id = sanitize_session_id(raw_session);
    let project_key = current_repo_key();

    // Load high-water mark — set of message ids already emitted for this session.
    let state_dir = home.join(STATE_SUBDIR);
    let state_path = state_dir.join(format!("{session_id}.json"));
    let mut state: IngestState = fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut seen: HashSet<String> = state.seen_message_ids.iter().cloned().collect();

    let transcript = fs::read_to_string(&transcript_path)?;
    let turns = parse_turns(&transcript, &session_id, &project_key);

    let mut new_records: Vec<TurnRecord> = Vec::new();
    for t in turns {
        if seen.contains(&t.message_id) {
            continue;
        }
        seen.insert(t.message_id.clone());
        new_records.push(t);
    }

    if new_records.is_empty() {
        return Ok(0);
    }

    let turns_dir = home.join(TURNS_SUBDIR);
    fs::create_dir_all(&turns_dir)?;
    fs::create_dir_all(&state_dir)?;
    let turns_path = turns_dir.join(format!("{session_id}.jsonl"));

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&turns_path)?;
    let mut written = 0;
    for rec in &new_records {
        let line = match serde_json::to_string(rec) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("relaywash: turn serialize failed: {e}");
                continue;
            }
        };
        if let Err(e) = writeln!(file, "{line}") {
            eprintln!("relaywash: turn append failed: {e}");
            break;
        }
        written += 1;
    }

    // Persist the new high-water mark. Cap the size so the state file doesn't
    // grow unbounded over very long sessions — we only need recent ids to
    // detect duplicates, and message ids appear in transcript order.
    state.seen_message_ids = seen.into_iter().collect();
    state.seen_message_ids.sort();
    const MAX_TRACKED_IDS: usize = 50_000;
    if state.seen_message_ids.len() > MAX_TRACKED_IDS {
        let drop = state.seen_message_ids.len() - MAX_TRACKED_IDS;
        state.seen_message_ids.drain(0..drop);
    }
    if let Ok(s) = serde_json::to_string(&state) {
        if let Err(e) = fs::write(&state_path, s) {
            eprintln!("relaywash: turn state write failed: {e}");
        }
    }

    Ok(written)
}

/// Parse a Claude Code transcript JSONL into per-assistant-turn accounting records.
///
/// Lines without `type == "assistant"` are skipped. Lines without usage data
/// (e.g. cache-hit-only synthetic entries) are skipped. Lines that cannot be
/// parsed as JSON are skipped silently — the transcript is a streaming format
/// that may have partial trailing lines.
pub fn parse_turns(transcript: &str, session_id: &str, project_key: &str) -> Vec<TurnRecord> {
    let mut out = Vec::new();
    for line in transcript.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        let Some(rec) = turn_from_entry(&entry, session_id, project_key) else {
            continue;
        };
        out.push(rec);
    }
    out
}

fn turn_from_entry(entry: &Value, session_id: &str, project_key: &str) -> Option<TurnRecord> {
    let msg = entry.get("message")?;
    let message_id = msg.get("id").and_then(|v| v.as_str())?.to_string();
    let model = msg
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let timestamp = entry
        .get("timestamp")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let usage_v = msg.get("usage")?;
    let usage = Usage {
        input_tokens: usage_v
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        output_tokens: usage_v
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cache_creation_tokens: usage_v
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cache_read_tokens: usage_v
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    };

    // Drop turns with no token activity at all — they're typically synthetic
    // (e.g. tool-result echoes) and add noise without adding signal.
    if usage.input_tokens == 0
        && usage.output_tokens == 0
        && usage.cache_creation_tokens == 0
        && usage.cache_read_tokens == 0
    {
        return None;
    }

    let (tools, bash_commands) = extract_tools(msg.get("content"));
    let category = classify(&tools, &bash_commands).as_str().to_string();
    let cost = estimate(&model, &usage);

    Some(TurnRecord {
        schema_version: TURN_SCHEMA_VERSION,
        message_id,
        session_id: session_id.to_string(),
        project_key: project_key.to_string(),
        model,
        timestamp,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_tokens: usage.cache_creation_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        estimated_cost_usd: cost,
        tools,
        category,
    })
}

/// Returns `(tool_names, joined_bash_commands)`. Bash command strings are
/// included only to drive [`categorize::classify`]; they are NOT persisted to
/// the ledger — only the tool names are.
fn extract_tools(content: Option<&Value>) -> (Vec<String>, String) {
    let mut tools = Vec::new();
    let mut bash = String::new();
    let Some(Value::Array(blocks)) = content else {
        return (tools, bash);
    };
    for b in blocks {
        if b.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
            continue;
        }
        let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        // Bash command body powers the categorizer's text heuristics. Only the
        // command string is read; nothing is persisted.
        let canonical = name
            .strip_prefix("mcp__relaywash__")
            .or_else(|| name.strip_prefix("relaywash__"))
            .unwrap_or(&name);
        if canonical == "Bash" {
            if let Some(cmd) = b
                .get("input")
                .and_then(|i| i.get("command"))
                .and_then(|c| c.as_str())
            {
                if !bash.is_empty() {
                    bash.push('\n');
                }
                bash.push_str(cmd);
            }
        }
        tools.push(name);
    }
    (tools, bash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn assistant_entry(id: &str, model: &str, ts: &str, tools: &[&str]) -> Value {
        let content: Vec<Value> = tools
            .iter()
            .map(|name| {
                json!({
                    "type": "tool_use",
                    "name": name,
                    "input": {}
                })
            })
            .collect();
        json!({
            "type": "assistant",
            "timestamp": ts,
            "message": {
                "id": id,
                "model": model,
                "type": "message",
                "role": "assistant",
                "content": content,
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "cache_creation_input_tokens": 200,
                    "cache_read_input_tokens": 300,
                }
            }
        })
    }

    fn write_transcript(dir: &Path, name: &str, entries: &[Value]) -> PathBuf {
        let path = dir.join(name);
        let mut s = String::new();
        for e in entries {
            s.push_str(&serde_json::to_string(e).unwrap());
            s.push('\n');
        }
        fs::write(&path, s).unwrap();
        path
    }

    #[test]
    fn parse_turns_extracts_one_record_per_assistant_turn() {
        let entries = [
            json!({"type": "user", "message": {"role": "user", "content": "hi"}}),
            assistant_entry("msg_1", "claude-opus-4-7", "2026-01-01T00:00:00Z", &["Edit"]),
            assistant_entry("msg_2", "claude-opus-4-7", "2026-01-01T00:00:01Z", &["TestRun"]),
        ];
        let transcript = entries
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect::<Vec<_>>()
            .join("\n");

        let recs = parse_turns(&transcript, "sess", "proj");
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].message_id, "msg_1");
        assert_eq!(recs[0].category, "coding");
        assert_eq!(recs[1].message_id, "msg_2");
        assert_eq!(recs[1].category, "testing");
        assert_eq!(recs[0].session_id, "sess");
        assert_eq!(recs[0].project_key, "proj");
        // Cost is computed from the hardcoded opus-4 table.
        assert!(recs[0].estimated_cost_usd > 0.0);
    }

    #[test]
    fn parse_turns_skips_turns_with_no_usage() {
        let entry = json!({
            "type": "assistant",
            "timestamp": "t",
            "message": {
                "id": "msg_empty",
                "model": "claude-opus-4-7",
                "content": [],
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0,
                }
            }
        });
        let recs = parse_turns(&serde_json::to_string(&entry).unwrap(), "s", "p");
        assert!(recs.is_empty(), "zero-usage turns should be filtered out");
    }

    #[test]
    fn parse_turns_skips_unparseable_lines() {
        let mut t = String::new();
        t.push_str("not json\n");
        t.push_str(
            &serde_json::to_string(&assistant_entry(
                "msg_1",
                "claude-opus-4-7",
                "t",
                &["Edit"],
            ))
            .unwrap(),
        );
        t.push('\n');
        t.push_str("{\"partial\":\n"); // unterminated, will fail
        let recs = parse_turns(&t, "s", "p");
        assert_eq!(recs.len(), 1);
    }

    #[test]
    fn parse_turns_unknown_model_records_zero_cost() {
        let mut e = assistant_entry("msg_x", "claude-future-9999", "t", &["Read"]);
        e["message"]["usage"]["input_tokens"] = json!(10);
        let recs = parse_turns(&serde_json::to_string(&e).unwrap(), "s", "p");
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].estimated_cost_usd, 0.0);
        assert_eq!(recs[0].model, "claude-future-9999");
    }

    fn read_jsonl(path: &Path) -> Vec<Value> {
        let raw = fs::read_to_string(path).unwrap_or_default();
        raw.lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn ingest_with_writes_records_and_state() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let transcript = write_transcript(
            home,
            "t1.jsonl",
            &[assistant_entry("msg_1", "claude-opus-4-7", "t", &["Edit"])],
        );

        let payload = json!({
            "session_id": "s1",
            "transcript_path": transcript.to_string_lossy(),
        });
        let n = ingest_with(home, &payload).unwrap();
        assert_eq!(n, 1);

        let turns_path = home.join(TURNS_SUBDIR).join("s1.jsonl");
        let recs = read_jsonl(&turns_path);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0]["messageId"], "msg_1");
        assert_eq!(recs[0]["category"], "coding");
        assert_eq!(recs[0]["schemaVersion"], 1);
        assert!(recs[0]["projectKey"].as_str().is_some());

        let state_path = home.join(STATE_SUBDIR).join("s1.json");
        assert!(state_path.exists(), "state file should be written");
    }

    #[test]
    fn ingest_with_dedupes_across_runs() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let entries = vec![
            assistant_entry("msg_1", "claude-opus-4-7", "t", &["Edit"]),
            assistant_entry("msg_2", "claude-opus-4-7", "t", &["TestRun"]),
        ];
        let transcript = write_transcript(home, "t.jsonl", &entries);

        let payload = json!({
            "session_id": "s2",
            "transcript_path": transcript.to_string_lossy(),
        });

        let n1 = ingest_with(home, &payload).unwrap();
        assert_eq!(n1, 2, "first run emits both turns");

        // Re-run — nothing new should be appended.
        let n2 = ingest_with(home, &payload).unwrap();
        assert_eq!(n2, 0, "second run is a no-op");

        // Append a third turn to the transcript and ingest again — only the
        // new one should be emitted.
        let mut all = entries;
        all.push(assistant_entry("msg_3", "claude-opus-4-7", "t", &["Build"]));
        write_transcript(home, "t.jsonl", &all);
        let n3 = ingest_with(home, &payload).unwrap();
        assert_eq!(n3, 1, "third run emits the one new turn");

        let recs = read_jsonl(&home.join(TURNS_SUBDIR).join("s2.jsonl"));
        assert_eq!(recs.len(), 3);
        let ids: Vec<&str> = recs.iter().map(|r| r["messageId"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["msg_1", "msg_2", "msg_3"]);
    }

    #[test]
    fn ingest_with_handles_missing_transcript_path() {
        let tmp = TempDir::new().unwrap();
        let n = ingest_with(tmp.path(), &json!({"session_id": "s3"})).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn ingest_with_handles_nonexistent_transcript_file() {
        let tmp = TempDir::new().unwrap();
        let n = ingest_with(
            tmp.path(),
            &json!({
                "session_id": "s4",
                "transcript_path": "/nonexistent/path/to/transcript.jsonl",
            }),
        )
        .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn ingest_with_dedupes_within_a_single_run() {
        // Two transcript entries with the same message id — only one should
        // be persisted.
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let entries = vec![
            assistant_entry("dup", "claude-opus-4-7", "t", &["Edit"]),
            assistant_entry("dup", "claude-opus-4-7", "t", &["TestRun"]),
        ];
        let transcript = write_transcript(home, "t.jsonl", &entries);

        let n = ingest_with(
            home,
            &json!({
                "session_id": "s5",
                "transcript_path": transcript.to_string_lossy(),
            }),
        )
        .unwrap();
        assert_eq!(n, 1);

        let recs = read_jsonl(&home.join(TURNS_SUBDIR).join("s5.jsonl"));
        assert_eq!(recs.len(), 1);
    }

    #[test]
    fn extract_tools_picks_up_bash_command_for_categorizer() {
        let content = json!([
            {"type": "tool_use", "name": "Bash", "input": {"command": "cargo test --all"}},
            {"type": "text", "text": "running tests"}
        ]);
        let (tools, bash) = extract_tools(Some(&content));
        assert_eq!(tools, vec!["Bash"]);
        assert!(bash.contains("cargo test"));
    }
}
