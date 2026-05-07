//! Native ledger writer matching the JS stub format (one JSONL file per session under
//! `${ledgerHome}/sessions/<sessionId>.jsonl`). Format is byte-stable with the JS stub so
//! existing burn ingest paths and dev tooling keep reading the same shape.

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_HOME: &str = ".relayburn";

#[derive(Debug, Clone)]
pub struct Ledger {
    home: PathBuf,
}

impl Default for Ledger {
    fn default() -> Self {
        Self::new(resolve_home())
    }
}

impl Ledger {
    pub fn new<P: Into<PathBuf>>(home: P) -> Self {
        Self { home: home.into() }
    }

    pub fn home(&self) -> &PathBuf {
        &self.home
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.home.join("sessions").join(format!("{session_id}.jsonl"))
    }

    pub fn record_tool_use(&self, session_id: &str, ev: ToolUseEvent) -> Result<()> {
        self.append(
            session_id,
            &serde_json::to_string(&ToolUseLine {
                ts: now_ms(),
                kind: "tool_use",
                tool: &ev.tool,
                replaces: &ev.replaces,
                collapsed_calls: ev.collapsed_calls,
            })?,
        )
    }

    /// Append a `session_end` event. Mirrors the JS `ingest()` surface. Transcript path is
    /// recorded for later, fuller attribution; we don't parse the transcript today.
    pub fn record_session_end(&self, session_id: &str, transcript_path: Option<&str>) -> Result<()> {
        let value = serde_json::json!({
            "ts": now_ms(),
            "kind": "session_end",
            "transcriptPath": transcript_path,
        });
        self.append(session_id, &value.to_string())
    }

    fn append(&self, session_id: &str, line: &str) -> Result<()> {
        let dir = self.home.join("sessions");
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let path = self.session_path(session_id);
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        writeln!(f, "{line}")?;
        Ok(())
    }

    /// Read all events for a single session.
    pub fn read_session(&self, session_id: &str) -> Vec<Value> {
        let path = self.session_path(session_id);
        read_jsonl(&path)
    }

    /// Aggregate per-tool counts across one session, or all sessions if `session` is None.
    pub fn summary(&self, session: Option<&str>) -> SummaryOut {
        let events = match session {
            Some(s) => self.read_session(s),
            None => self.read_all_sessions(),
        };
        aggregate(&events)
    }

    fn read_all_sessions(&self) -> Vec<Value> {
        let dir = self.home.join("sessions");
        let mut all = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return all,
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            all.extend(read_jsonl(&p));
        }
        all
    }
}

#[derive(Debug, Clone)]
pub struct ToolUseEvent {
    pub tool: String,
    pub replaces: Vec<String>,
    pub collapsed_calls: u32,
}

#[derive(Serialize)]
struct ToolUseLine<'a> {
    ts: u128,
    kind: &'a str,
    tool: &'a str,
    replaces: &'a [String],
    #[serde(rename = "collapsedCalls")]
    collapsed_calls: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SummaryOut {
    #[serde(rename = "byTool")]
    pub by_tool: BTreeMap<String, ToolStats>,
    #[serde(rename = "totalCalls")]
    pub total_calls: u32,
    #[serde(rename = "collapsedCalls")]
    pub collapsed_calls: u32,
    #[serde(rename = "replacedTools")]
    pub replaced_tools: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ToolStats {
    pub calls: u32,
    #[serde(rename = "collapsedCalls")]
    pub collapsed_calls: u32,
}

fn aggregate(events: &[Value]) -> SummaryOut {
    let mut out = SummaryOut::default();
    let mut replaced: BTreeSet<String> = BTreeSet::new();
    for ev in events {
        if ev.get("kind").and_then(|v| v.as_str()) != Some("tool_use") {
            continue;
        }
        let tool = ev
            .get("tool")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let collapsed = ev.get("collapsedCalls").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let entry = out.by_tool.entry(tool).or_default();
        entry.calls += 1;
        entry.collapsed_calls += collapsed;
        out.total_calls += 1;
        out.collapsed_calls += collapsed;
        if let Some(arr) = ev.get("replaces").and_then(|v| v.as_array()) {
            for r in arr {
                if let Some(s) = r.as_str() {
                    replaced.insert(s.to_string());
                }
            }
        }
    }
    out.replaced_tools = replaced.into_iter().collect();
    out
}

fn read_jsonl(path: &std::path::Path) -> Vec<Value> {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    raw.lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn resolve_home() -> PathBuf {
    if let Ok(s) = std::env::var("RELAYBURN_HOME") {
        return PathBuf::from(s);
    }
    if let Some(home) = home_dir() {
        return home.join(DEFAULT_HOME);
    }
    PathBuf::from(DEFAULT_HOME)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
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
    use tempfile::TempDir;

    #[test]
    fn record_and_read_round_trip() {
        let dir = TempDir::new().unwrap();
        let l = Ledger::new(dir.path());
        l.record_tool_use(
            "s1",
            ToolUseEvent {
                tool: "relaywash__Search".into(),
                replaces: vec!["Glob".into(), "Grep".into(), "Read".into()],
                collapsed_calls: 9,
            },
        )
        .unwrap();
        l.record_tool_use(
            "s1",
            ToolUseEvent {
                tool: "relaywash__Read".into(),
                replaces: vec!["Read".into()],
                collapsed_calls: 1,
            },
        )
        .unwrap();
        let events = l.read_session("s1");
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn session_end_appended() {
        let dir = TempDir::new().unwrap();
        let l = Ledger::new(dir.path());
        l.record_session_end("s2", Some("/tmp/transcript.jsonl")).unwrap();
        let events = l.read_session("s2");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "session_end");
        assert_eq!(events[0]["transcriptPath"], "/tmp/transcript.jsonl");
    }

    #[test]
    fn summary_aggregates_per_tool() {
        let dir = TempDir::new().unwrap();
        let l = Ledger::new(dir.path());
        for _ in 0..3 {
            l.record_tool_use(
                "s",
                ToolUseEvent {
                    tool: "relaywash__Search".into(),
                    replaces: vec!["Glob".into(), "Grep".into()],
                    collapsed_calls: 2,
                },
            )
            .unwrap();
        }
        l.record_tool_use(
            "s",
            ToolUseEvent {
                tool: "relaywash__Read".into(),
                replaces: vec!["Read".into()],
                collapsed_calls: 1,
            },
        )
        .unwrap();
        let s = l.summary(Some("s"));
        assert_eq!(s.total_calls, 4);
        assert_eq!(s.collapsed_calls, 7);
        assert_eq!(s.by_tool["relaywash__Search"].calls, 3);
        assert_eq!(s.by_tool["relaywash__Search"].collapsed_calls, 6);
        assert_eq!(s.by_tool["relaywash__Read"].calls, 1);
        assert_eq!(s.replaced_tools, vec!["Glob", "Grep", "Read"]);
    }
}
