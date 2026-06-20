//! Shared on-disk log store for the process-backed tools.
//!
//! Build and TestRun persist the full subprocess output to a temp file and return
//! only a `fullLogPath` plus a tail/summary, so the model can fetch detail on demand
//! (TestRun's `getFailureLog`) without paying for the whole log inline. Both the
//! writer and the reader live here so neither tool has to reach into the other.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Directory holding persisted tool logs, named `<prefix>-<millis>.log`.
pub(crate) fn dir() -> PathBuf {
    std::env::temp_dir().join("relaywash-logs")
}

/// Write `body` to a fresh timestamped log file and return its path.
pub(crate) fn write(prefix: &str, body: &str) -> std::io::Result<PathBuf> {
    let dir = dir();
    std::fs::create_dir_all(&dir)?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{prefix}-{ts}.log"));
    std::fs::write(&path, body)?;
    Ok(path)
}

/// The most recent `<prefix>-*.log` file, or `None` if none exist. Restricting to a
/// single prefix is what makes "most recent" well-defined: filenames sort lexically,
/// and for a *fixed* prefix the fixed-width millisecond suffix makes lexical order
/// chronological. Mixing prefixes would not — `testrun-` always outsorts `build-`
/// regardless of time — so callers ask for the log family they actually want.
pub(crate) fn latest(prefix: &str) -> std::io::Result<Option<PathBuf>> {
    let dir = dir();
    if !dir.exists() {
        return Ok(None);
    }
    let want = format!("{prefix}-");
    let mut entries: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|r| r.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with(&want) && name.ends_with(".log")
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());
    Ok(entries.last().map(|e| e.path()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_picks_newest_of_its_prefix_and_ignores_others() {
        // Unique prefixes keep this test isolated from build-/testrun- logs and from
        // parallel test threads sharing the temp dir.
        write("washcorefirst", "first").unwrap();
        let second = write("washcorefirst", "second").unwrap();
        // A lexically-later prefix must NOT shadow ours — this is the bug the prefix
        // filter fixes (sorting all logs together would return this instead).
        write("washzzz", "wrong-family").unwrap();

        let found = latest("washcorefirst").unwrap().expect("a washcorefirst log exists");
        let body = std::fs::read_to_string(&found).unwrap();
        assert_eq!(body, "second", "latest must return the newest log of the asked prefix");
        let _ = second;

        // An unused prefix yields nothing rather than someone else's log.
        assert!(latest("washneverwritten").unwrap().is_none());
    }
}
