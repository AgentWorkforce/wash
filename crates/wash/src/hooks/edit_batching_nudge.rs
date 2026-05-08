//! PostToolUse on relaywash__Edit: count single-edit calls per session; nudge if >= 3 in 5 turns.
//! State lives at `${RELAYBURN_HOME}/edit-nudge/<sessionId>.json`.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{sanitize_session_id, write_continue, write_json};

const HISTORY_WINDOW: usize = 5;
const SINGLE_EDIT_THRESHOLD: usize = 3;

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    history: Vec<Entry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    turn: usize,
    #[serde(rename = "editCount")]
    edit_count: u32,
}

pub fn run(payload: &Value, out: &mut impl Write) -> Result<()> {
    run_in(&nudge_dir_default(), payload, out)
}

fn run_in(dir: &Path, payload: &Value, out: &mut impl Write) -> Result<()> {
    let raw_session = payload
        .get("session_id")
        .or_else(|| payload.get("sessionId"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let session_id = sanitize_session_id(raw_session);
    let edit_count: u32 = payload
        .get("tool_input")
        .or_else(|| payload.get("toolInput"))
        .and_then(|v| v.get("edits"))
        .and_then(|v| v.as_array())
        .map(|a| a.len() as u32)
        .unwrap_or(1);

    fs::create_dir_all(dir).ok();
    let path = dir.join(format!("{session_id}.json"));

    let mut state: State = fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    state.history.push(Entry {
        turn: state.history.len() + 1,
        edit_count,
    });
    if state.history.len() > HISTORY_WINDOW {
        let drop = state.history.len() - HISTORY_WINDOW;
        state.history.drain(0..drop);
    }
    let _ = fs::write(&path, serde_json::to_string(&state)?);

    let recent_singles = state.history.iter().filter(|e| e.edit_count == 1).count();
    if recent_singles >= SINGLE_EDIT_THRESHOLD {
        write_json(
            out,
            &json!({
                "continue": true,
                "systemMessage": "relaywash: 3+ single-edit calls in the last 5 turns. relaywash__Edit accepts an `edits[]` array — batch them next time for one round-trip.",
            }),
        )?;
        return Ok(());
    }
    write_continue(out)
}

fn nudge_dir_default() -> PathBuf {
    let home = if let Ok(s) = std::env::var("RELAYBURN_HOME") {
        PathBuf::from(s)
    } else if let Some(h) = std::env::var_os("HOME") {
        PathBuf::from(h).join(".relayburn")
    } else {
        PathBuf::from(".relayburn")
    };
    home.join("edit-nudge")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn drive(dir: &Path, payload: Value) -> String {
        let mut buf = Vec::new();
        run_in(dir, &payload, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn nudges_after_three_single_edits() {
        let tmp = TempDir::new().unwrap();
        for _ in 0..2 {
            let s = drive(tmp.path(), json!({"session_id": "a", "tool_input": {"edits": [{}]}}));
            assert!(!s.contains("systemMessage"), "should not nudge yet: {s}");
        }
        let s = drive(tmp.path(), json!({"session_id": "a", "tool_input": {"edits": [{}]}}));
        assert!(s.contains("systemMessage"), "should nudge on third: {s}");
        assert!(s.contains("relaywash__Edit"));
    }

    #[test]
    fn batched_edits_dont_count_toward_threshold() {
        let tmp = TempDir::new().unwrap();
        for _ in 0..5 {
            let s = drive(
                tmp.path(),
                json!({"session_id": "b", "tool_input": {"edits": [{}, {}, {}]}}),
            );
            assert!(!s.contains("systemMessage"), "batched should not nudge: {s}");
        }
    }
}
