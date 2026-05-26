//! PreCompact / PostCompact hooks for compaction-aware attribution (wash#33).
//!
//! Claude Code's compactor rewrites a session's transcript JSONL in place.
//! `PreCompact` fires before the rewrite, `PostCompact` fires after — both
//! receive the same `transcript_path` field on stdin. Neither payload contains
//! the transcript inline, so we have to read the file from disk.
//!
//! Strategy:
//!
//! 1. On `pre-compact`, copy the transcript file to
//!    `${RELAYBURN_HOME}/compaction/<session>-pre.jsonl` so the post-compact
//!    hook has a snapshot to diff against.
//! 2. On `post-compact`, read the snapshot (pre) and current transcript (post),
//!    intersect by `uuid` to compute which messages survived, and attribute
//!    surviving `tool_result` blocks back to the tool that produced them.
//!    Append a `kind: "compaction"` event to the session ledger at
//!    `${RELAYBURN_HOME}/sessions/<session>.jsonl`.
//!
//! The hook never blocks the harness: parse failures, missing snapshots, and
//! I/O errors are logged via `eprintln!` and the hook still emits
//! `continue:true` with a best-effort event when possible.

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{sanitize_session_id, write_continue};
use crate::profile::ledger_home;
use crate::tokens::estimate_tokens_usize;

const SNAPSHOT_SUBDIR: &str = "compaction";
const SESSIONS_SUBDIR: &str = "sessions";
const SYNTHETIC_SUMMARY_TOOL: &str = "compacted-summary";
const SCHEMA_VERSION: u32 = 1;

pub fn run_pre(payload: &Value, out: &mut impl Write) -> Result<()> {
    let home = ledger_home();
    run_pre_with(&home, payload, out)
}

pub fn run_post(payload: &Value, out: &mut impl Write) -> Result<()> {
    let home = ledger_home();
    run_post_with(&home, payload, out)
}

fn run_pre_with(home: &Path, payload: &Value, out: &mut impl Write) -> Result<()> {
    let session_id = extract_session_id(payload);
    if let Some(transcript) = extract_transcript_path(payload) {
        let snapshot_dir = home.join(SNAPSHOT_SUBDIR);
        if let Err(e) = fs::create_dir_all(&snapshot_dir) {
            eprintln!(
                "relaywash: compaction snapshot dir create failed ({}): {e}",
                snapshot_dir.display()
            );
            return write_continue(out);
        }
        let snapshot_path = snapshot_dir.join(format!("{session_id}-pre.jsonl"));
        if let Err(e) = fs::copy(&transcript, &snapshot_path) {
            eprintln!(
                "relaywash: pre-compact snapshot copy failed (src={}, dst={}): {e}",
                transcript.display(),
                snapshot_path.display()
            );
        }
    } else {
        eprintln!("relaywash: pre-compact missing transcript_path; nothing to snapshot");
    }
    write_continue(out)
}

fn run_post_with(home: &Path, payload: &Value, out: &mut impl Write) -> Result<()> {
    let session_id = extract_session_id(payload);
    let trigger = payload
        .get("trigger")
        .and_then(|v| v.as_str())
        .unwrap_or("auto")
        .to_string();
    let snapshot_path = home
        .join(SNAPSHOT_SUBDIR)
        .join(format!("{session_id}-pre.jsonl"));
    let transcript_path = extract_transcript_path(payload);

    let pre_entries = match read_jsonl(&snapshot_path) {
        Ok(rows) => rows,
        Err(e) => {
            // Missing snapshot is expected the first time around (e.g. if the
            // pre-compact hook didn't run). Emit a best-effort event with empty
            // pre-state so consumers still see *something* happened.
            eprintln!(
                "relaywash: post-compact could not read snapshot {}: {e}",
                snapshot_path.display()
            );
            Vec::new()
        }
    };

    let post_entries = match transcript_path.as_deref() {
        Some(p) => match read_jsonl(p) {
            Ok(rows) => rows,
            Err(e) => {
                eprintln!(
                    "relaywash: post-compact could not read transcript {}: {e}",
                    p.display()
                );
                Vec::new()
            }
        },
        None => {
            eprintln!("relaywash: post-compact missing transcript_path; assuming empty post-state");
            Vec::new()
        }
    };

    let event = build_event(&trigger, &pre_entries, &post_entries);

    if let Err(e) = append_session_event(home, &session_id, &event) {
        eprintln!(
            "relaywash: post-compact ledger append failed (session={session_id}): {e}"
        );
    }

    // Snapshot is consumed: best-effort cleanup so we don't accumulate stale
    // pre-files for long-running sessions. Ignored on error.
    let _ = fs::remove_file(&snapshot_path);

    write_continue(out)
}

#[derive(Serialize)]
struct ToolSurvival {
    #[serde(rename = "callsBefore")]
    calls_before: u64,
    #[serde(rename = "callsAfter")]
    calls_after: u64,
    #[serde(rename = "estimatedTokensBefore")]
    estimated_tokens_before: u64,
    #[serde(rename = "estimatedTokensAfter")]
    estimated_tokens_after: u64,
}

#[derive(Serialize)]
struct CompactionEvent {
    kind: &'static str,
    trigger: String,
    #[serde(rename = "preMessageCount")]
    pre_message_count: u64,
    #[serde(rename = "postMessageCount")]
    post_message_count: u64,
    #[serde(rename = "perToolSurvival")]
    per_tool_survival: indexmap::IndexMap<String, ToolSurvival>,
    #[serde(rename = "syntheticSummaries")]
    synthetic_summaries: u64,
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
}

fn build_event(trigger: &str, pre: &[Value], post: &[Value]) -> CompactionEvent {
    // Map uuid -> (tool_use_id -> tool_name) for pre-entries so we can resolve
    // surviving tool_result blocks back to their producing tool. Independent of
    // survival: a `tool_use` block carries the tool name even if its result
    // ended up in a different message uuid.
    let pre_tool_use_to_name: HashMap<String, String> = pre
        .iter()
        .flat_map(|row| extract_tool_uses(row))
        .collect();

    let post_uuids: HashSet<String> = post
        .iter()
        .filter_map(|row| row.get("uuid").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect();

    let mut pre_counts: indexmap::IndexMap<String, ToolSurvival> = indexmap::IndexMap::new();
    let mut post_counts: indexmap::IndexMap<String, ToolSurvival> = indexmap::IndexMap::new();

    // Tally tool_result blocks in the pre-set, attributing to the tool name
    // resolved via tool_use_id. tool_use blocks themselves also count towards
    // the tool's "calls before" so a same-message tool_use+tool_result pair
    // doesn't go missing.
    for row in pre {
        for block in extract_tool_result_blocks(row) {
            let tool = pre_tool_use_to_name
                .get(&block.tool_use_id)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            let entry = pre_counts.entry(tool).or_insert(ToolSurvival {
                calls_before: 0,
                calls_after: 0,
                estimated_tokens_before: 0,
                estimated_tokens_after: 0,
            });
            entry.calls_before += 1;
            entry.estimated_tokens_before += estimate_tokens_usize(block.bytes);
        }
    }

    // Same pass for post entries, but a surviving tool_result must come from a
    // message uuid present in `post_uuids`. The pre's tool_use_id -> tool_name
    // map still applies because the harness preserves block ids across the
    // rewrite (it drops or summarises whole messages, not individual blocks).
    let mut synthetic_summaries: u64 = 0;
    for row in post {
        let uuid = row.get("uuid").and_then(|v| v.as_str()).unwrap_or("");
        if !post_uuids.contains(uuid) {
            continue;
        }
        if is_synthetic_summary(row) {
            synthetic_summaries += 1;
            let entry = post_counts
                .entry(SYNTHETIC_SUMMARY_TOOL.to_string())
                .or_insert(ToolSurvival {
                    calls_before: 0,
                    calls_after: 0,
                    estimated_tokens_before: 0,
                    estimated_tokens_after: 0,
                });
            entry.calls_after += 1;
            entry.estimated_tokens_after += estimate_tokens_usize(message_bytes(row));
            continue;
        }
        for block in extract_tool_result_blocks(row) {
            let tool = pre_tool_use_to_name
                .get(&block.tool_use_id)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            let entry = post_counts.entry(tool).or_insert(ToolSurvival {
                calls_before: 0,
                calls_after: 0,
                estimated_tokens_before: 0,
                estimated_tokens_after: 0,
            });
            entry.calls_after += 1;
            entry.estimated_tokens_after += estimate_tokens_usize(block.bytes);
        }
    }

    // Merge pre and post into a single map keyed by tool name with both halves.
    let mut merged: indexmap::IndexMap<String, ToolSurvival> = indexmap::IndexMap::new();
    for (tool, pre_entry) in pre_counts {
        merged.insert(tool, pre_entry);
    }
    for (tool, post_entry) in post_counts {
        let m = merged.entry(tool).or_insert(ToolSurvival {
            calls_before: 0,
            calls_after: 0,
            estimated_tokens_before: 0,
            estimated_tokens_after: 0,
        });
        m.calls_after += post_entry.calls_after;
        m.estimated_tokens_after += post_entry.estimated_tokens_after;
    }

    CompactionEvent {
        kind: "compaction",
        trigger: trigger.to_string(),
        pre_message_count: pre.len() as u64,
        post_message_count: post.len() as u64,
        per_tool_survival: merged,
        synthetic_summaries,
        schema_version: SCHEMA_VERSION,
    }
}

/// Pull every `tool_use` block's `(id, name)` pair out of a transcript row.
fn extract_tool_uses(row: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let content = match row.get("message").and_then(|m| m.get("content")) {
        Some(v) => v,
        None => return out,
    };
    let arr = match content.as_array() {
        Some(a) => a,
        None => return out,
    };
    for block in arr {
        if block.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
            continue;
        }
        let id = block.get("id").and_then(|v| v.as_str());
        let name = block.get("name").and_then(|v| v.as_str());
        if let (Some(id), Some(name)) = (id, name) {
            out.push((id.to_string(), name.to_string()));
        }
    }
    out
}

struct ToolResultRef {
    tool_use_id: String,
    /// Byte size of the result block — fed to the shared token estimator.
    bytes: usize,
}

fn extract_tool_result_blocks(row: &Value) -> Vec<ToolResultRef> {
    let mut out = Vec::new();
    let content = match row.get("message").and_then(|m| m.get("content")) {
        Some(v) => v,
        None => return out,
    };
    let arr = match content.as_array() {
        Some(a) => a,
        None => return out,
    };
    for block in arr {
        if block.get("type").and_then(|v| v.as_str()) != Some("tool_result") {
            continue;
        }
        let tool_use_id = block
            .get("tool_use_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let bytes = serde_json::to_string(block).map(|s| s.len()).unwrap_or(0);
        out.push(ToolResultRef { tool_use_id, bytes });
    }
    out
}

/// A "synthetic summary" is the compacted-summary stub the harness inserts in
/// place of dropped messages. The harness marks these with
/// `isCompactSummary: true` (newer builds) or `type == "summary"` on the row;
/// we accept either, plus a defensive check for messages whose content is a
/// single text block tagged as a summary.
fn is_synthetic_summary(row: &Value) -> bool {
    if row.get("isCompactSummary").and_then(|v| v.as_bool()) == Some(true) {
        return true;
    }
    if row.get("type").and_then(|v| v.as_str()) == Some("summary") {
        return true;
    }
    if let Some(role) = row.get("message").and_then(|m| m.get("role")).and_then(|v| v.as_str())
        && role == "system"
        && row
            .get("subtype")
            .and_then(|v| v.as_str())
            .map(|s| s.contains("compact"))
            .unwrap_or(false)
    {
        return true;
    }
    false
}

fn message_bytes(row: &Value) -> usize {
    row.get("message")
        .and_then(|m| serde_json::to_string(m).ok())
        .map(|s| s.len())
        .unwrap_or(0)
}

fn append_session_event(home: &Path, session_id: &str, event: &CompactionEvent) -> Result<()> {
    let dir = home.join(SESSIONS_SUBDIR);
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{session_id}.jsonl"));
    let line = serde_json::to_string(event)?;
    let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

fn read_jsonl(path: &Path) -> std::io::Result<Vec<Value>> {
    let raw = fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(v) => out.push(v),
            Err(e) => {
                // Single malformed line shouldn't abort the whole parse — log
                // and continue so the rest of the transcript still attributes.
                eprintln!(
                    "relaywash: compaction parse skipped malformed line in {}: {e}",
                    path.display()
                );
            }
        }
    }
    Ok(out)
}

fn extract_session_id(payload: &Value) -> String {
    let raw = payload
        .get("session_id")
        .or_else(|| payload.get("sessionId"))
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    sanitize_session_id(raw)
}

fn extract_transcript_path(payload: &Value) -> Option<PathBuf> {
    payload
        .get("transcript_path")
        .or_else(|| payload.get("transcriptPath"))
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn write_jsonl(path: &Path, rows: &[Value]) {
        let mut s = String::new();
        for r in rows {
            s.push_str(&serde_json::to_string(r).unwrap());
            s.push('\n');
        }
        fs::write(path, s).unwrap();
    }

    fn tool_use(uuid: &str, tool_use_id: &str, name: &str) -> Value {
        json!({
            "uuid": uuid,
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": tool_use_id, "name": name, "input": {}}
                ]
            }
        })
    }

    fn tool_result(uuid: &str, tool_use_id: &str, content: &str) -> Value {
        json!({
            "uuid": uuid,
            "message": {
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": tool_use_id, "content": content}
                ]
            }
        })
    }

    fn read_event(home: &Path, session: &str) -> Value {
        let path = home.join(SESSIONS_SUBDIR).join(format!("{session}.jsonl"));
        let raw = fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("session ledger missing at {}: {e}", path.display())
        });
        let last = raw.lines().filter(|l| !l.is_empty()).next_back().unwrap();
        serde_json::from_str(last).unwrap()
    }

    fn drive_pre(home: &Path, payload: Value) -> String {
        let mut buf = Vec::new();
        run_pre_with(home, &payload, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn drive_post(home: &Path, payload: Value) -> String {
        let mut buf = Vec::new();
        run_post_with(home, &payload, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn exact_survival_when_nothing_drops() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let transcript = tmp.path().join("session.jsonl");
        let rows = vec![
            tool_use("u-1", "tu-search-1", "Search"),
            tool_result("u-2", "tu-search-1", "result body"),
        ];
        write_jsonl(&transcript, &rows);

        let payload = json!({
            "session_id": "s-exact",
            "transcript_path": transcript.to_str().unwrap(),
            "trigger": "manual"
        });
        let pre = drive_pre(home, payload.clone());
        assert!(pre.contains("\"continue\":true"));
        // Transcript untouched — post sees the same rows.
        let post = drive_post(home, payload);
        assert!(post.contains("\"continue\":true"));

        let ev = read_event(home, "s-exact");
        assert_eq!(ev["kind"], "compaction");
        assert_eq!(ev["trigger"], "manual");
        assert_eq!(ev["preMessageCount"], 2);
        assert_eq!(ev["postMessageCount"], 2);
        assert_eq!(ev["schemaVersion"], 1);
        assert_eq!(ev["syntheticSummaries"], 0);
        let survival = &ev["perToolSurvival"]["Search"];
        assert_eq!(survival["callsBefore"], 1);
        assert_eq!(survival["callsAfter"], 1);
        assert_eq!(
            survival["estimatedTokensBefore"],
            survival["estimatedTokensAfter"]
        );
    }

    #[test]
    fn dropped_messages_attributed_per_tool() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let transcript = tmp.path().join("session.jsonl");
        let pre_rows = vec![
            tool_use("u-1", "tu-search-1", "Search"),
            tool_result("u-2", "tu-search-1", "search result alpha beta gamma"),
            tool_use("u-3", "tu-search-2", "Search"),
            tool_result("u-4", "tu-search-2", "search result two delta epsilon zeta"),
            tool_use("u-5", "tu-read-1", "Read"),
            tool_result("u-6", "tu-read-1", "file contents survived"),
        ];
        write_jsonl(&transcript, &pre_rows);
        let payload = json!({
            "session_id": "s-drop",
            "transcript_path": transcript.to_str().unwrap(),
            "trigger": "auto"
        });
        drive_pre(home, payload.clone());

        // Now the harness "compacts": drop both Search results, keep the Read.
        let post_rows = vec![
            tool_use("u-5", "tu-read-1", "Read"),
            tool_result("u-6", "tu-read-1", "file contents survived"),
        ];
        write_jsonl(&transcript, &post_rows);

        drive_post(home, payload);

        let ev = read_event(home, "s-drop");
        assert_eq!(ev["preMessageCount"], 6);
        assert_eq!(ev["postMessageCount"], 2);

        let search = &ev["perToolSurvival"]["Search"];
        assert_eq!(search["callsBefore"], 2);
        assert_eq!(search["callsAfter"], 0);
        assert!(search["estimatedTokensBefore"].as_u64().unwrap() > 0);
        assert_eq!(search["estimatedTokensAfter"], 0);

        let read = &ev["perToolSurvival"]["Read"];
        assert_eq!(read["callsBefore"], 1);
        assert_eq!(read["callsAfter"], 1);
        assert_eq!(
            read["estimatedTokensBefore"],
            read["estimatedTokensAfter"]
        );
    }

    #[test]
    fn missing_snapshot_emits_best_effort_event() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let transcript = tmp.path().join("session.jsonl");
        let post_rows = vec![tool_use("u-1", "tu-1", "Read")];
        write_jsonl(&transcript, &post_rows);

        let payload = json!({
            "session_id": "s-nopre",
            "transcript_path": transcript.to_str().unwrap(),
            "trigger": "auto"
        });
        // No pre-snapshot exists.
        let s = drive_post(home, payload);
        assert!(s.contains("\"continue\":true"));
        let ev = read_event(home, "s-nopre");
        assert_eq!(ev["preMessageCount"], 0);
        assert_eq!(ev["postMessageCount"], 1);
        assert_eq!(ev["kind"], "compaction");
    }

    #[test]
    fn synthetic_summary_bucketed() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let transcript = tmp.path().join("session.jsonl");
        let pre_rows = vec![
            tool_use("u-1", "tu-1", "Search"),
            tool_result("u-2", "tu-1", "lots of result text here"),
        ];
        write_jsonl(&transcript, &pre_rows);
        let payload = json!({
            "session_id": "s-summary",
            "transcript_path": transcript.to_str().unwrap(),
            "trigger": "auto"
        });
        drive_pre(home, payload.clone());

        // Compactor replaced both rows with a single synthetic summary.
        let post_rows = vec![json!({
            "uuid": "u-99",
            "isCompactSummary": true,
            "message": {
                "role": "system",
                "content": [{"type": "text", "text": "Earlier search summarized."}]
            }
        })];
        write_jsonl(&transcript, &post_rows);

        drive_post(home, payload);

        let ev = read_event(home, "s-summary");
        assert_eq!(ev["syntheticSummaries"], 1);
        let synth = &ev["perToolSurvival"][SYNTHETIC_SUMMARY_TOOL];
        assert_eq!(synth["callsAfter"], 1);
        // Original Search call still listed as "callsBefore: 1, callsAfter: 0".
        let search = &ev["perToolSurvival"]["Search"];
        assert_eq!(search["callsBefore"], 1);
        assert_eq!(search["callsAfter"], 0);
    }

    #[test]
    fn pre_hook_copies_transcript_to_snapshot() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let transcript = tmp.path().join("t.jsonl");
        fs::write(&transcript, "{\"uuid\":\"u-1\"}\n").unwrap();
        drive_pre(
            home,
            json!({
                "session_id": "s-cp",
                "transcript_path": transcript.to_str().unwrap()
            }),
        );
        let snap = home.join(SNAPSHOT_SUBDIR).join("s-cp-pre.jsonl");
        assert!(snap.exists());
        assert!(fs::read_to_string(snap).unwrap().contains("u-1"));
    }

    #[test]
    fn post_hook_cleans_up_snapshot() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let transcript = tmp.path().join("t.jsonl");
        fs::write(&transcript, "{\"uuid\":\"u-1\"}\n").unwrap();
        let payload = json!({
            "session_id": "s-clean",
            "transcript_path": transcript.to_str().unwrap()
        });
        drive_pre(home, payload.clone());
        let snap = home.join(SNAPSHOT_SUBDIR).join("s-clean-pre.jsonl");
        assert!(snap.exists());
        drive_post(home, payload);
        assert!(!snap.exists(), "snapshot should be cleaned up after post-compact");
    }

    #[test]
    fn malformed_jsonl_line_does_not_abort_parse() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let transcript = tmp.path().join("t.jsonl");
        // First line is junk, second is valid.
        fs::write(&transcript, "this is not json\n{\"uuid\":\"ok\"}\n").unwrap();
        let payload = json!({
            "session_id": "s-bad",
            "transcript_path": transcript.to_str().unwrap()
        });
        drive_pre(home, payload.clone());
        drive_post(home, payload);
        let ev = read_event(home, "s-bad");
        assert_eq!(ev["preMessageCount"], 1);
        assert_eq!(ev["postMessageCount"], 1);
    }
}
