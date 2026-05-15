//! Profile substrate for wash#13's adaptive layer.
//!
//! A profile is per-repo tuning derived (in a follow-up PR) from observed tool-call
//! patterns. PR 7 ships the *substrate*: the on-disk shape, the loader, and the
//! tool-side hooks that read defaults from the profile when present. No aggregator yet —
//! profiles don't get written automatically. The behavior change is invisible until a
//! profile file exists, which keeps PR 7 strictly additive.
//!
//! **Cache safety.** The profile MUST NOT influence the JSON schema the agent sees,
//! the tool descriptions, or the agent definition. Anthropic's prompt cache (5-min TTL)
//! invalidates on any of those. Profile values are *applied defaults* — used in the
//! handler when the agent omits an arg. The tool's `inputSchema` literal is byte-stable
//! across profile presence; only the runtime fallback shifts.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;

const DEFAULT_HOME: &str = ".relayburn";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub version: u32,
    #[serde(rename = "repoKey")]
    pub repo_key: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: u128,
    pub observations: u32,
    /// "low" | "medium" | "high" — set by the aggregator (PR 7+1).
    pub confidence: String,
    pub tools: ToolDefaults,
    /// Bash regex candidates surfaced by the aggregator. Never auto-applied — a follow-up
    /// `/wash-approve-patterns` slash command promotes them into the redirect list.
    #[serde(rename = "pendingBashPatterns")]
    pub pending_bash_patterns: Vec<String>,
    pub hooks: HookSettings,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolDefaults {
    pub search: SearchDefaults,
    pub read: ReadDefaults,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchDefaults {
    #[serde(rename = "maxResults", skip_serializing_if = "Option::is_none")]
    pub max_results: Option<usize>,
    #[serde(rename = "contextLines", skip_serializing_if = "Option::is_none")]
    pub context_lines: Option<u32>,
    #[serde(rename = "maxFileBytes", skip_serializing_if = "Option::is_none")]
    pub max_file_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReadDefaults {
    #[serde(rename = "smallFileLines", skip_serializing_if = "Option::is_none")]
    pub small_file_lines: Option<usize>,
    #[serde(rename = "smallFunctionLines", skip_serializing_if = "Option::is_none")]
    pub small_function_lines: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HookSettings {
    /// "off" | "warn" | "error" — controls the edit-batching nudge severity.
    #[serde(rename = "editBatchingNudge", skip_serializing_if = "Option::is_none")]
    pub edit_batching_nudge: Option<String>,
}

/// Process-cached active profile. Loaded lazily on first access.
fn cached() -> &'static OnceLock<Profile> {
    static C: OnceLock<Profile> = OnceLock::new();
    &C
}

/// Resolve the active profile for the current process. Reads `RELAYWASH_PROFILE_PATH`
/// if set (test/override seam), otherwise derives the per-repo path from CWD's git
/// remote, otherwise the global profile, otherwise empty defaults.
pub fn get() -> &'static Profile {
    cached().get_or_init(load_active)
}

fn load_active() -> Profile {
    if let Ok(path) = std::env::var("RELAYWASH_PROFILE_PATH") {
        return load_from(std::path::Path::new(&path)).unwrap_or_default();
    }
    let home = ledger_home();
    let key = current_repo_key();
    let per_repo = home.join("profiles").join(format!("{key}.json"));
    if let Some(p) = load_from(&per_repo) {
        return p;
    }
    let global = home.join("profiles").join("_global.json");
    load_from(&global).unwrap_or_default()
}

fn load_from(path: &std::path::Path) -> Option<Profile> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn ledger_home() -> PathBuf {
    if let Ok(s) = std::env::var("RELAYBURN_HOME") {
        return PathBuf::from(s);
    }
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h).join(DEFAULT_HOME);
    }
    PathBuf::from(DEFAULT_HOME)
}

/// Stable per-repo key. Uses git remote URL when available, otherwise the absolute path
/// of the current working directory. Hashed with FNV-1a so the result is filename-safe
/// and short. Falls back to "_global" if nothing resolves.
pub fn current_repo_key() -> String {
    if let Some(url) = git_remote_url() {
        return slug_with_hash(&url);
    }
    if let Ok(cwd) = std::env::current_dir() {
        return slug_with_hash(&cwd.to_string_lossy());
    }
    "_global".into()
}

fn git_remote_url() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn slug_with_hash(input: &str) -> String {
    // Slug: keep alphanumeric + a few safe punctuation chars; truncate.
    let slug: String = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(48)
        .collect();
    let h = fnv1a(input.as_bytes());
    format!("{slug}_{:016x}", h)
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Extract a top-level value from an arbitrary serde_json blob — used by the
/// observation hook to redact args before logging. Returns the requested field if it
/// exists in the source, ignoring missing keys.
pub fn pick_fields<'a>(src: &'a Value, allow: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for k in allow {
        if let Some(v) = src.get(*k) {
            if !v.is_null() {
                out.insert((*k).into(), v.clone());
            }
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn default_profile_loads_when_file_missing() {
        let p: Profile = serde_json::from_str("{}").unwrap();
        assert_eq!(p.version, 0);
        assert!(p.tools.search.max_results.is_none());
    }

    #[test]
    fn round_trip_partial_profile() {
        let json = r#"{"version":1,"tools":{"search":{"maxResults":80,"contextLines":1,"rank":"matches"},"read":{"smallFileLines":150}}}"#;
        let p: Profile = serde_json::from_str(json).unwrap();
        assert_eq!(p.tools.search.max_results, Some(80));
        assert_eq!(p.tools.search.context_lines, Some(1));
        assert_eq!(p.tools.search.rank.as_deref(), Some("matches"));
        assert_eq!(p.tools.read.small_file_lines, Some(150));
    }

    #[test]
    fn load_from_existing_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("p.json");
        std::fs::write(&path, r#"{"version":1,"tools":{"read":{"smallFileLines":300}}}"#).unwrap();
        let p = load_from(&path).unwrap();
        assert_eq!(p.tools.read.small_file_lines, Some(300));
    }

    #[test]
    fn pick_fields_allowlist() {
        let src = serde_json::json!({"path":"/secret","maxResults":50,"content":"sensitive"});
        let picked = pick_fields(&src, &["maxResults", "rank"]);
        assert_eq!(picked["maxResults"], 50);
        assert!(picked.get("path").is_none());
        assert!(picked.get("content").is_none());
    }

    #[test]
    fn slug_is_filename_safe() {
        let s = slug_with_hash("git@github.com:AgentWorkforce/wash.git");
        assert!(!s.contains('/'));
        assert!(!s.contains(':'));
        assert!(!s.contains('@'));
        assert!(s.contains('_'));
    }

    #[test]
    fn fnv1a_is_deterministic() {
        assert_eq!(fnv1a(b"hello"), fnv1a(b"hello"));
        assert_ne!(fnv1a(b"hello"), fnv1a(b"world"));
    }
}
