//! PostToolUse on every `mcp__relaywash__*` — wash#13 layer 1 (observation, always-on).
//!
//! Captures arg/outcome data into the burn ledger so a future aggregator can derive
//! per-repo tuning without observation requiring its own behavior change. Sensitive
//! fields (paths, file contents, search needles, edit text, PR bodies) are NOT logged —
//! only tuning-relevant args (counts, modes, runner/builder selectors) and the result's
//! byte size + a few derived signals.

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{sanitize_session_id, write_continue};
use crate::burn::Ledger;
use crate::profile::{ledger_home, pick_fields};

const SESSION_STATE_DIR: &str = "observe";

/// Allowlist of per-tool args that matter for tuning. Anything else is dropped to
/// keep the ledger free of user content.
fn allowed_args(tool: &str) -> &'static [&'static str] {
    match tool {
        "Search" => &["maxResults", "contextLines", "rank"],
        "Read" => &["mode"],
        "Edit" => &["fuzzy"],
        "GitState" => &["op", "maxFiles", "maxLines", "withBody"],
        "TestRun" => &["runner", "failuresOnly", "maxFailures"],
        "Build" => &["builder", "errorTailLines"],
        "GhPR" => &["op", "fields", "maxComments", "maxDiffLines"],
        _ => &[],
    }
}

#[derive(Serialize)]
struct OutcomeLine<'a> {
    ts: u128,
    kind: &'a str,
    tool: &'a str,
    args: &'a Value,
    #[serde(rename = "resultBytes")]
    result_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none", rename = "hitCap")]
    hit_cap: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "prevTool")]
    prev_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "prevSameArgs")]
    prev_same_args: Option<bool>,
}

#[derive(serde::Deserialize, serde::Serialize, Default)]
struct SessionState {
    #[serde(rename = "lastTool", default, skip_serializing_if = "Option::is_none")]
    last_tool: Option<String>,
    #[serde(rename = "lastArgs", default, skip_serializing_if = "Option::is_none")]
    last_args: Option<Value>,
}

pub fn run(payload: &Value, out: &mut impl Write) -> Result<()> {
    run_with(&Ledger::default(), &observe_dir(), payload, out)
}

fn run_with(
    ledger: &Ledger,
    state_dir: &Path,
    payload: &Value,
    out: &mut impl Write,
) -> Result<()> {
    let raw_session = payload
        .get("session_id")
        .or_else(|| payload.get("sessionId"))
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let session_id = sanitize_session_id(raw_session);
    let tool_name_full = payload
        .get("tool_name")
        .or_else(|| payload.get("toolName"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // Strip the `mcp__relaywash__` prefix if present so the ledger uses bare tool names.
    let tool = tool_name_full
        .strip_prefix("mcp__relaywash__")
        .or_else(|| tool_name_full.strip_prefix("relaywash__"))
        .unwrap_or(tool_name_full);
    if tool.is_empty() {
        return write_continue(out);
    }

    let raw_args = payload
        .get("tool_input")
        .or_else(|| payload.get("toolInput"))
        .cloned()
        .unwrap_or(Value::Null);
    let safe_args = if raw_args.is_object() {
        pick_fields(&raw_args, allowed_args(tool))
    } else {
        Value::Object(serde_json::Map::new())
    };

    let response = payload
        .get("tool_response")
        .or_else(|| payload.get("toolResponse"))
        .cloned()
        .unwrap_or(Value::Null);
    let result_bytes = serde_json::to_string(&response)
        .map(|s| s.len())
        .unwrap_or(0);

    let hit_cap = compute_hit_cap(tool, &raw_args, &response);

    // Read previous tool/args for this session, then update.
    fs::create_dir_all(state_dir).ok();
    let state_path = state_dir.join(format!("{session_id}.json"));
    let prev: SessionState = fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let prev_tool = prev.last_tool.clone();
    let prev_same_args = prev_tool.as_deref().map(|prev_t| {
        prev_t == tool
            && prev
                .last_args
                .as_ref()
                .map(|a| a == &safe_args)
                .unwrap_or(false)
    });

    let new_state = SessionState {
        last_tool: Some(tool.to_string()),
        last_args: Some(safe_args.clone()),
    };
    let _ = fs::write(
        &state_path,
        serde_json::to_string(&new_state).unwrap_or_default(),
    );

    let line = serde_json::to_string(&OutcomeLine {
        ts: now_ms(),
        kind: "tool_outcome",
        tool,
        args: &safe_args,
        result_bytes,
        hit_cap,
        prev_tool,
        prev_same_args,
    })?;
    let session_path = ledger
        .home()
        .join("sessions")
        .join(format!("{session_id}.jsonl"));
    let _ = fs::create_dir_all(session_path.parent().unwrap());
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&session_path)?;
    writeln!(f, "{line}")?;

    write_continue(out)
}

fn compute_hit_cap(tool: &str, args: &Value, response: &Value) -> Option<bool> {
    if tool != "Search" {
        return None;
    }
    let cap = args
        .get("maxResults")
        .and_then(|v| v.as_u64())
        .unwrap_or(50);
    let n = response
        .get("results")
        .and_then(|v| v.as_array())
        .map(|a| a.len() as u64);
    n.map(|count| count >= cap)
}

fn observe_dir() -> PathBuf {
    ledger_home().join(SESSION_STATE_DIR)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn drive(payload: Value, ledger: &Ledger, state_dir: &Path) -> String {
        let mut buf = Vec::new();
        run_with(ledger, state_dir, &payload, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn writes_tool_outcome_event_with_redacted_args() {
        let tmp = TempDir::new().unwrap();
        let l = Ledger::new(tmp.path());
        let state_dir = tmp.path().join("observe");
        let s = drive(
            json!({
                "session_id": "s1",
                "tool_name": "mcp__relaywash__Search",
                "tool_input": {
                    "content": "API_KEY",  // sensitive — must be dropped
                    "paths": ["src/secrets.ts"],  // sensitive — dropped
                    "maxResults": 50,
                    "contextLines": 2,
                    "rank": "matches"
                },
                "tool_response": {"results": [{},{},{},{},{}]}
            }),
            &l,
            &state_dir,
        );
        assert!(s.contains("\"continue\":true"));
        let events = l.read_session("s1");
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev["kind"], "tool_outcome");
        assert_eq!(ev["tool"], "Search");
        assert_eq!(ev["args"]["maxResults"], 50);
        assert!(ev["args"].get("content").is_none(), "content must be redacted");
        assert!(ev["args"].get("paths").is_none(), "paths must be redacted");
        assert!(ev["resultBytes"].as_u64().unwrap() > 0);
    }

    #[test]
    fn hit_cap_true_when_results_meet_max() {
        let tmp = TempDir::new().unwrap();
        let l = Ledger::new(tmp.path());
        let state_dir = tmp.path().join("observe");
        drive(
            json!({
                "session_id": "s2",
                "tool_name": "relaywash__Search",
                "tool_input": {"maxResults": 3, "rank": "matches"},
                "tool_response": {"results": [{},{},{}]}
            }),
            &l,
            &state_dir,
        );
        let events = l.read_session("s2");
        assert_eq!(events[0]["hitCap"], true);
    }

    #[test]
    fn hit_cap_absent_for_non_search() {
        let tmp = TempDir::new().unwrap();
        let l = Ledger::new(tmp.path());
        let state_dir = tmp.path().join("observe");
        drive(
            json!({
                "session_id": "s3",
                "tool_name": "mcp__relaywash__Read",
                "tool_input": {"path": "/foo", "mode": "signatures"},
                "tool_response": {"content": "..."}
            }),
            &l,
            &state_dir,
        );
        let events = l.read_session("s3");
        assert!(events[0].get("hitCap").map(|v| v.is_null()).unwrap_or(true));
    }

    #[test]
    fn prev_same_args_detected() {
        let tmp = TempDir::new().unwrap();
        let l = Ledger::new(tmp.path());
        let state_dir = tmp.path().join("observe");
        let payload = json!({
            "session_id": "s4",
            "tool_name": "mcp__relaywash__Search",
            "tool_input": {"maxResults": 10, "rank": "matches", "contextLines": 2},
            "tool_response": {"results": []}
        });
        drive(payload.clone(), &l, &state_dir);
        drive(payload, &l, &state_dir);
        let events = l.read_session("s4");
        assert_eq!(events.len(), 2);
        assert!(events[0].get("prevTool").map(|v| v.is_null()).unwrap_or(true));
        assert_eq!(events[1]["prevTool"], "Search");
        assert_eq!(events[1]["prevSameArgs"], true);
    }
}
