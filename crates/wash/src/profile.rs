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
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

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
    #[serde(rename = "maxTotalBytes", skip_serializing_if = "Option::is_none")]
    pub max_total_bytes: Option<u64>,
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

/// Cache entry: the loaded `Profile` plus the cache key that produced it. The key is
/// the full priority-ordered list of candidate paths we consult and each candidate's
/// mtime at load time. Tracking every candidate (not just the chosen one) means
/// state changes anywhere in the chain — a previously-missing higher-priority file
/// appearing, a broken file getting rewritten — invalidate the cache.
#[derive(Clone)]
struct CacheEntry {
    profile: Arc<Profile>,
    candidates: Vec<PathBuf>,
    mtimes: Vec<Option<SystemTime>>,
}

/// Process-level mtime-watched cache. The MCP server lives for the whole session, so a
/// profile written mid-session (e.g., by the future `wash learn aggregate` aggregator)
/// must be picked up without restart. We pay one `metadata()` syscall per candidate
/// per access and only re-parse JSON when something changes.
fn cached() -> &'static RwLock<Option<CacheEntry>> {
    static C: RwLock<Option<CacheEntry>> = RwLock::new(None);
    &C
}

/// Resolve the active profile for the current process. Walks the candidate list in
/// priority order — `RELAYWASH_PROFILE_PATH` if set, else per-repo, else global — and
/// takes the first file that *successfully loads*. A file that exists but fails to
/// parse is skipped so a broken per-repo profile does not silently mask a valid global
/// one. The empty default is returned when nothing loads.
///
/// We cache against the full candidate list plus each candidate's current mtime, so any
/// mtime change (including missing → present transitions) invalidates the cache.
pub fn get() -> Arc<Profile> {
    let candidates = candidate_paths();
    let mtimes: Vec<Option<SystemTime>> = candidates.iter().map(|p| mtime_of(p)).collect();

    {
        let guard = cached().read().expect("profile cache poisoned");
        if let Some(entry) = guard.as_ref() {
            if entry.candidates == candidates && entry.mtimes == mtimes {
                return entry.profile.clone();
            }
        }
    }

    let profile = Arc::new(
        candidates
            .iter()
            .find_map(|p| load_from(p))
            .unwrap_or_default(),
    );

    let new_entry = CacheEntry {
        profile: profile.clone(),
        candidates,
        mtimes,
    };
    *cached().write().expect("profile cache poisoned") = Some(new_entry);
    profile
}

/// Build the prioritized list of candidate profile paths. The env override short-
/// circuits the fallback chain so tests and explicit users can target one specific file.
fn candidate_paths() -> Vec<PathBuf> {
    if let Ok(path) = std::env::var("RELAYWASH_PROFILE_PATH") {
        return vec![PathBuf::from(path)];
    }
    let home = ledger_home();
    let key = current_repo_key();
    vec![
        home.join("profiles").join(format!("{key}.json")),
        home.join("profiles").join("_global.json"),
    ]
}

fn mtime_of(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
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
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// `RELAYWASH_PROFILE_PATH`, `RELAYBURN_HOME`, and `cached()` are all process-wide,
    /// so tests that exercise `get()` must run serially to avoid clobbering each other.
    static GET_SERIAL: Mutex<()> = Mutex::new(());

    /// Clear the process-level cache so each `get()` test starts from a clean slate.
    fn reset_cache() {
        *cached().write().unwrap() = None;
    }

    /// RAII helper: clears the env vars `get()` reads and the cache on drop, so a test
    /// that panics mid-flight cannot leak global state into a later test sharing
    /// `GET_SERIAL`. Used at the top of every test that mutates these globals.
    struct GetTestCleanup;
    impl Drop for GetTestCleanup {
        fn drop(&mut self) {
            // SAFETY: `GET_SERIAL` is held for the duration of the test, so no other
            // thread is touching these env vars while we clear them.
            unsafe {
                std::env::remove_var("RELAYWASH_PROFILE_PATH");
                std::env::remove_var("RELAYBURN_HOME");
            }
            reset_cache();
        }
    }

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

    /// Issue #24: a profile rewritten mid-session must be re-read on the next `get()`,
    /// not stuck on whatever was cached at process start.
    #[test]
    fn get_reloads_when_profile_file_mtime_changes() {
        let _g = GET_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _cleanup = GetTestCleanup;
        reset_cache();

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("p.json");

        // v1
        std::fs::write(
            &path,
            r#"{"version":1,"tools":{"search":{"maxResults":10}}}"#,
        )
        .unwrap();
        // Backdate so the v2 write is guaranteed to advance mtime even on
        // coarse-resolution filesystems.
        let past = SystemTime::now() - std::time::Duration::from_secs(2);
        set_mtime(&path, past);

        // SAFETY: tests under `GET_SERIAL` are serialized; no other thread touches this
        // env var while the guard is held.
        unsafe {
            std::env::set_var("RELAYWASH_PROFILE_PATH", &path);
        }
        let p1 = get();
        assert_eq!(p1.tools.search.max_results, Some(10));

        // v2 — overwrite and bump mtime to "now".
        std::fs::write(
            &path,
            r#"{"version":1,"tools":{"search":{"maxResults":777}}}"#,
        )
        .unwrap();
        set_mtime(&path, SystemTime::now());

        let p2 = get();
        assert_eq!(
            p2.tools.search.max_results,
            Some(777),
            "profile cache failed to reload after mtime advance"
        );
    }

    /// Cheapness check: two `get()` calls with no filesystem change return the same
    /// underlying `Arc`, proving we didn't re-parse JSON on the second call.
    #[test]
    fn get_returns_cached_instance_when_unchanged() {
        let _g = GET_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _cleanup = GetTestCleanup;
        reset_cache();

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("p.json");
        std::fs::write(
            &path,
            r#"{"version":1,"tools":{"search":{"maxResults":42}}}"#,
        )
        .unwrap();

        unsafe {
            std::env::set_var("RELAYWASH_PROFILE_PATH", &path);
        }
        let a = get();
        let b = get();
        assert!(
            Arc::ptr_eq(&a, &b),
            "second get() should reuse the cached Arc when mtime is unchanged"
        );
        assert_eq!(a.tools.search.max_results, Some(42));
    }

    /// Regression: a per-repo profile that exists but fails to parse must not mask the
    /// global profile. The previous resolve-then-load split short-circuited on
    /// `exists()` and silently returned the empty default when the per-repo file was
    /// malformed; now the loader walks the candidate list and skips files that fail to
    /// parse.
    #[test]
    fn get_falls_back_to_global_when_per_repo_is_unreadable() {
        let _g = GET_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _cleanup = GetTestCleanup;
        reset_cache();

        let tmp = TempDir::new().unwrap();
        let profiles = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles).unwrap();

        // SAFETY: `GET_SERIAL` serializes env-var access across tests.
        unsafe {
            std::env::set_var("RELAYBURN_HOME", tmp.path());
            // Make sure no leftover override from another test interferes.
            std::env::remove_var("RELAYWASH_PROFILE_PATH");
        }

        // Resolve the per-repo key the same way `get()` will, so we can plant the
        // malformed file at exactly the path that gets stat'd.
        let key = current_repo_key();
        std::fs::write(profiles.join(format!("{key}.json")), "{ not valid json").unwrap();
        std::fs::write(
            profiles.join("_global.json"),
            r#"{"version":1,"tools":{"search":{"maxResults":999}}}"#,
        )
        .unwrap();

        let p = get();
        assert_eq!(
            p.tools.search.max_results,
            Some(999),
            "broken per-repo profile should fall through to the global profile",
        );
    }

    /// Helper: set a file's mtime. We avoid the `filetime` crate dep by writing the
    /// timestamp via the platform `utimensat` shim through `std::fs::File::set_modified`
    /// (stable since 1.75).
    fn set_mtime(path: &std::path::Path, t: SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(t).unwrap();
    }
}
