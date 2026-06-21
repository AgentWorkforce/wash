//! Reading line-delimited JSON: the Claude Code transcript and wash's own session
//! ledgers. Both the per-turn accounting ingest (`crate::accounting`) and the
//! compaction hook (`crate::hooks::compaction`) consume the same JSONL shape, so the
//! bytes -> rows step lives here once rather than being re-implemented per consumer.

use serde_json::Value;
use std::path::Path;

/// Parse JSONL text into one `Value` per non-blank line. Malformed lines are skipped
/// rather than aborting the parse — the transcript is a streaming format that may carry
/// a partial trailing line. `on_bad_line` receives each parse error so the caller picks
/// its own policy (log, count, or stay silent).
pub fn parse_lines(text: &str, mut on_bad_line: impl FnMut(&serde_json::Error)) -> Vec<Value> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(v) => out.push(v),
            Err(e) => on_bad_line(&e),
        }
    }
    out
}

/// Read a JSONL file into one `Value` per line, logging and skipping malformed lines.
/// Returns `Err` only when the file itself cannot be read.
pub fn read_file(path: &Path) -> std::io::Result<Vec<Value>> {
    let raw = std::fs::read_to_string(path)?;
    Ok(parse_lines(&raw, |e| {
        eprintln!(
            "relaywash: skipped malformed JSONL line in {}: {e}",
            path.display()
        );
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lines_skips_blank_and_malformed() {
        let mut bad = 0;
        let rows = parse_lines("{\"a\":1}\n\n  \nnot json\n{\"b\":2}\n", |_| bad += 1);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["a"], 1);
        assert_eq!(rows[1]["b"], 2);
        assert_eq!(bad, 1, "exactly one malformed line reported");
    }

    #[test]
    fn read_file_missing_path_is_err() {
        assert!(read_file(Path::new("/no/such/wash-transcript.jsonl")).is_err());
    }
}
