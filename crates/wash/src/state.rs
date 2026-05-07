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

#[cfg(test)]
pub fn reset() {
    let mut s = state().lock().unwrap();
    s.read_cache.clear();
    s.last_search_symbol = None;
}
