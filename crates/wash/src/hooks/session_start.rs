//! SessionStart hook: ensure the relayburn ledger directory exists.

use anyhow::Result;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::write_continue;

pub fn run(_payload: &Value, out: &mut impl Write) -> Result<()> {
    ensure_dir(&ledger_home_default());
    write_continue(out)
}

fn ensure_dir(dir: &Path) {
    if let Err(e) = fs::create_dir_all(dir) {
        eprintln!("relaywash: cannot create ledger dir {}: {e}", dir.display());
    }
}

fn ledger_home_default() -> PathBuf {
    if let Ok(s) = std::env::var("RELAYBURN_HOME") {
        return PathBuf::from(s);
    }
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h).join(".relayburn");
    }
    PathBuf::from(".relayburn")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ensure_dir_creates_nested_path() {
        let tmp = TempDir::new().unwrap();
        let nested = tmp.path().join("a/b/c");
        ensure_dir(&nested);
        assert!(nested.exists());
    }
}
