//! Per-session state shared across tool invocations: the Read mtime cache and the
//! most-recent Search symbol. Matches the JS module-level state model.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Default)]
struct State {
    /// Outer key = sessionId, inner key = absolute file path.
    read_cache: HashMap<String, HashMap<String, u128>>,
    last_search_symbol: Option<String>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State::default()))
}

pub fn note_searched_symbol(sym: Option<&str>) {
    let mut s = state().lock().unwrap();
    s.last_search_symbol = sym.filter(|s| !s.is_empty()).map(|s| s.to_string());
}

pub fn last_searched_symbol() -> Option<String> {
    state().lock().unwrap().last_search_symbol.clone()
}

pub fn read_cache_get(session: &str, path: &str) -> Option<u128> {
    state()
        .lock()
        .unwrap()
        .read_cache
        .get(session)
        .and_then(|m| m.get(path).copied())
}

pub fn read_cache_put(session: &str, path: &str, mtime_ms: u128) {
    state()
        .lock()
        .unwrap()
        .read_cache
        .entry(session.to_string())
        .or_default()
        .insert(path.to_string(), mtime_ms);
}

/// Test-only escape hatch.
///
/// **Caveat for callers:** `cargo test` runs tests in parallel by default, and this
/// function mutates the process-global state. Tests that call `reset()` MUST also use
/// unique session IDs (or fresh `TempDir`s passed to lower-level helpers) so two
/// concurrent tests don't trample each other's per-session entries between a
/// `reset()` and the next operation. The current test suite follows this discipline:
/// each Read test uses a distinct session id (`s1`, `s2`, …) and never reads state
/// it didn't itself put there in the same test.
#[cfg(test)]
pub fn reset() {
    let mut s = state().lock().unwrap();
    s.read_cache.clear();
    s.last_search_symbol = None;
}
