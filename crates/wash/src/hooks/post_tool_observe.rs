//! PostToolUse on every `mcp__relaywash__*` — wash#13 layer 1 (observation, always-on).
//!
//! Captures arg/outcome data into a wash-local JSONL log so a future aggregator can
//! derive per-repo tuning without observation requiring its own behavior change.
//! Sensitive fields (paths, file contents, search needles, edit text, PR bodies) are
//! NOT logged — only tuning-relevant args (counts, modes, runner/builder selectors)
//! and the result's byte size + a few derived signals.
//!
//! These events are wash's own observability surface; relayburn-sdk does not read them.
//! They live under `${RELAYBURN_HOME}/observe/<sessionId>.jsonl`.

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{sanitize_session_id, write_continue};
use crate::profile::{ledger_home, pick_fields};
use crate::tokens::estimate_tokens;

const STATE_SUBDIR: &str = "observe-state";
const EVENTS_SUBDIR: &str = "observe";
const METRICS_SCHEMA_VERSION: u32 = 1;

/// Allowlist of per-tool args that matter for tuning. Anything else is dropped to
/// keep the log free of user content.
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

/// Per-call savings event used by the adaptive learning layer. Schema versioned
/// from the start so future fields (compression-ratio buckets, follow-up flags,
/// etc.) can be added without breaking downstream consumers.
#[derive(Serialize)]
struct MetricsLine<'a> {
    ts: u128,
    kind: &'a str,
    tool: &'a str,
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    #[serde(rename = "resultBytes")]
    result_bytes: u64,
    #[serde(rename = "resultTokens")]
    result_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none", rename = "baselineBytes")]
    baseline_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "baselineTokens")]
    baseline_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "estimatedSavedTokens")]
    estimated_saved_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "hitCap")]
    hit_cap: Option<bool>,
}

#[derive(serde::Deserialize, serde::Serialize, Default)]
struct SessionState {
    #[serde(rename = "lastTool", default, skip_serializing_if = "Option::is_none")]
    last_tool: Option<String>,
    #[serde(rename = "lastArgs", default, skip_serializing_if = "Option::is_none")]
    last_args: Option<Value>,
}

pub fn run(payload: &Value, out: &mut impl Write) -> Result<()> {
    let home = ledger_home();
    run_with(&home, payload, out)
}

fn run_with(home: &Path, payload: &Value, out: &mut impl Write) -> Result<()> {
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
    // Strip the `mcp__relaywash__` prefix if present so the log uses bare tool names.
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
    let state_dir = home.join(STATE_SUBDIR);
    if let Err(e) = fs::create_dir_all(&state_dir) {
        eprintln!("relaywash: observe state dir create failed ({}): {e}", state_dir.display());
    }
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
    match serde_json::to_string(&new_state) {
        Ok(state_json) => {
            if let Err(e) = fs::write(&state_path, state_json) {
                eprintln!(
                    "relaywash: observe state write failed (session={session_id}, path={}): {e}",
                    state_path.display()
                );
            }
        }
        Err(e) => eprintln!(
            "relaywash: observe state serialize failed (session={session_id}): {e}"
        ),
    }

    // Best-effort from here down: the observe hook is telemetry, not user-visible work.
    // Any write/serialize failure is logged and dropped so the user's tool call still
    // returns `continue:true` — the hook must never block the session.
    let events_dir = home.join(EVENTS_SUBDIR);
    if let Err(e) = fs::create_dir_all(&events_dir) {
        eprintln!(
            "relaywash: observe events dir create failed ({}): {e}",
            events_dir.display()
        );
    }
    let events_path = events_dir.join(format!("{session_id}.jsonl"));

    let baseline_bytes = response
        .get("_meta")
        .and_then(|m| m.get("baselineBytes"))
        .and_then(|v| v.as_u64());
    let result_bytes_u64 = result_bytes as u64;
    let result_tokens = estimate_tokens(result_bytes_u64);
    let baseline_tokens = baseline_bytes.map(estimate_tokens);
    let estimated_saved_tokens = baseline_tokens.map(|b| b.saturating_sub(result_tokens));

    let ts = now_ms();
    let outcome = OutcomeLine {
        ts,
        kind: "tool_outcome",
        tool,
        args: &safe_args,
        result_bytes,
        hit_cap,
        prev_tool,
        prev_same_args,
    };
    let metrics = MetricsLine {
        ts,
        kind: "tool_metrics",
        tool,
        schema_version: METRICS_SCHEMA_VERSION,
        result_bytes: result_bytes_u64,
        result_tokens,
        baseline_bytes,
        baseline_tokens,
        estimated_saved_tokens,
        hit_cap,
    };
    append_event(&events_path, &outcome);
    append_event(&events_path, &metrics);

    write_continue(out)
}

fn append_event<T: Serialize>(events_path: &Path, event: &T) {
    let line = match serde_json::to_string(event) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("relaywash: observe serialize failed: {e}");
            return;
        }
    };
    match fs::OpenOptions::new().create(true).append(true).open(events_path) {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "{line}") {
                eprintln!("relaywash: observe append failed: {e}");
            }
        }
        Err(e) => eprintln!("relaywash: observe open failed: {e}"),
    }
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

    fn drive(payload: Value, home: &Path) -> String {
        let mut buf = Vec::new();
        run_with(home, &payload, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn read_events(home: &Path, session_id: &str) -> Vec<Value> {
        let path = home
            .join(EVENTS_SUBDIR)
            .join(format!("{session_id}.jsonl"));
        let raw = std::fs::read_to_string(&path).unwrap_or_default();
        raw.lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                serde_json::from_str(l)
                    .unwrap_or_else(|e| panic!("invalid observe event JSON in {path:?}: {e}\nline: {l}"))
            })
            .collect()
    }

    fn outcomes(events: &[Value]) -> Vec<&Value> {
        events.iter().filter(|e| e["kind"] == "tool_outcome").collect()
    }

    fn metrics(events: &[Value]) -> Vec<&Value> {
        events.iter().filter(|e| e["kind"] == "tool_metrics").collect()
    }

    #[test]
    fn writes_tool_outcome_event_with_redacted_args() {
        let tmp = TempDir::new().unwrap();
        let s = drive(
            json!({
                "session_id": "s1",
                "tool_name": "mcp__relaywash__Search",
                "tool_input": {
                    "content": "API_KEY",
                    "paths": ["src/secrets.ts"],
                    "maxResults": 50,
                    "contextLines": 2,
                    "rank": "matches"
                },
                "tool_response": {"results": [{},{},{},{},{}]}
            }),
            tmp.path(),
        );
        assert!(s.contains("\"continue\":true"));
        let events = read_events(tmp.path(), "s1");
        let outcomes = outcomes(&events);
        assert_eq!(outcomes.len(), 1);
        let ev = outcomes[0];
        assert_eq!(ev["tool"], "Search");
        assert_eq!(ev["args"]["maxResults"], 50);
        assert!(ev["args"].get("content").is_none(), "content must be redacted");
        assert!(ev["args"].get("paths").is_none(), "paths must be redacted");
        assert!(ev["resultBytes"].as_u64().unwrap() > 0);
    }

    #[test]
    fn hit_cap_true_when_results_meet_max() {
        let tmp = TempDir::new().unwrap();
        drive(
            json!({
                "session_id": "s2",
                "tool_name": "relaywash__Search",
                "tool_input": {"maxResults": 3, "rank": "matches"},
                "tool_response": {"results": [{},{},{}]}
            }),
            tmp.path(),
        );
        let events = read_events(tmp.path(), "s2");
        assert_eq!(outcomes(&events)[0]["hitCap"], true);
        assert_eq!(metrics(&events)[0]["hitCap"], true);
    }

    #[test]
    fn hit_cap_absent_for_non_search() {
        let tmp = TempDir::new().unwrap();
        drive(
            json!({
                "session_id": "s3",
                "tool_name": "mcp__relaywash__Read",
                "tool_input": {"path": "/foo", "mode": "signatures"},
                "tool_response": {"content": "..."}
            }),
            tmp.path(),
        );
        let events = read_events(tmp.path(), "s3");
        assert!(outcomes(&events)[0].get("hitCap").map(|v| v.is_null()).unwrap_or(true));
    }

    #[test]
    fn prev_same_args_detected() {
        let tmp = TempDir::new().unwrap();
        let payload = json!({
            "session_id": "s4",
            "tool_name": "mcp__relaywash__Search",
            "tool_input": {"maxResults": 10, "rank": "matches", "contextLines": 2},
            "tool_response": {"results": []}
        });
        drive(payload.clone(), tmp.path());
        drive(payload, tmp.path());
        let events = read_events(tmp.path(), "s4");
        let outs = outcomes(&events);
        assert_eq!(outs.len(), 2);
        assert!(outs[0].get("prevTool").map(|v| v.is_null()).unwrap_or(true));
        assert_eq!(outs[1]["prevTool"], "Search");
        assert_eq!(outs[1]["prevSameArgs"], true);
    }

    #[test]
    fn tool_metrics_event_shape_and_token_estimate() {
        let tmp = TempDir::new().unwrap();
        drive(
            json!({
                "session_id": "m1",
                "tool_name": "mcp__relaywash__Search",
                "tool_input": {"maxResults": 50, "rank": "matches"},
                "tool_response": {"results": [{"path":"a.ts","snippet":"x"}]}
            }),
            tmp.path(),
        );
        let events = read_events(tmp.path(), "m1");
        let m = metrics(&events);
        assert_eq!(m.len(), 1, "exactly one tool_metrics per call");
        let ev = m[0];
        assert_eq!(ev["kind"], "tool_metrics");
        assert_eq!(ev["tool"], "Search");
        assert_eq!(ev["schemaVersion"], 1);
        let rb = ev["resultBytes"].as_u64().unwrap();
        let rt = ev["resultTokens"].as_u64().unwrap();
        assert!(rb > 0);
        assert_eq!(rt, rb.div_ceil(4));
        assert!(ev.get("baselineBytes").map(|v| v.is_null()).unwrap_or(true));
        assert!(ev.get("estimatedSavedTokens").map(|v| v.is_null()).unwrap_or(true));
    }

    #[test]
    fn tool_metrics_includes_baseline_when_meta_provides_it() {
        let tmp = TempDir::new().unwrap();
        drive(
            json!({
                "session_id": "m2",
                "tool_name": "mcp__relaywash__Read",
                "tool_input": {"mode": "signatures"},
                "tool_response": {
                    "content": "sig only",
                    "_meta": {"replaces": ["Read"], "collapsedCalls": 1, "baselineBytes": 8000}
                }
            }),
            tmp.path(),
        );
        let events = read_events(tmp.path(), "m2");
        let ev = metrics(&events)[0];
        assert_eq!(ev["baselineBytes"], 8000);
        assert_eq!(ev["baselineTokens"], 2000);
        let rt = ev["resultTokens"].as_u64().unwrap();
        let saved = ev["estimatedSavedTokens"].as_u64().unwrap();
        assert_eq!(saved, 2000u64.saturating_sub(rt));
    }

    #[test]
    fn tool_metrics_baseline_for_build() {
        let tmp = TempDir::new().unwrap();
        drive(
            json!({
                "session_id": "m3",
                "tool_name": "mcp__relaywash__Build",
                "tool_input": {"builder": "cargo"},
                "tool_response": {
                    "builder": "cargo",
                    "success": true,
                    "_meta": {"replaces": ["Bash:build"], "collapsedCalls": 1, "baselineBytes": 4096}
                }
            }),
            tmp.path(),
        );
        let events = read_events(tmp.path(), "m3");
        let ev = metrics(&events)[0];
        assert_eq!(ev["baselineBytes"], 4096);
        assert_eq!(ev["baselineTokens"], 1024);
    }

    #[test]
    fn tool_metrics_baseline_for_testrun() {
        let tmp = TempDir::new().unwrap();
        drive(
            json!({
                "session_id": "m4",
                "tool_name": "mcp__relaywash__TestRun",
                "tool_input": {"runner": "cargo"},
                "tool_response": {
                    "runner": "cargo",
                    "passed": 12,
                    "failed": 0,
                    "_meta": {"replaces": ["Bash:test"], "collapsedCalls": 1, "baselineBytes": 12000}
                }
            }),
            tmp.path(),
        );
        let events = read_events(tmp.path(), "m4");
        let ev = metrics(&events)[0];
        assert_eq!(ev["baselineBytes"], 12000);
        assert_eq!(ev["baselineTokens"], 3000);
    }

    #[test]
    fn tool_metrics_redacts_user_content() {
        // The metrics event must never carry args, content, paths, etc. — same
        // confidentiality contract as tool_outcome.
        let tmp = TempDir::new().unwrap();
        drive(
            json!({
                "session_id": "m5",
                "tool_name": "mcp__relaywash__Search",
                "tool_input": {
                    "content": "SECRET_TOKEN_abc123",
                    "paths": ["/etc/passwd"],
                    "maxResults": 5
                },
                "tool_response": {"results": []}
            }),
            tmp.path(),
        );
        let events = read_events(tmp.path(), "m5");
        let m = metrics(&events);
        let raw = serde_json::to_string(&m[0]).unwrap();
        assert!(!raw.contains("SECRET_TOKEN_abc123"));
        assert!(!raw.contains("/etc/passwd"));
        assert!(m[0].get("args").is_none(), "metrics event has no args field");
    }
}
