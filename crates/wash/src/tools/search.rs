//! relaywash__Search — collapses Glob + Grep + Read into one ranked-snippet response.

use anyhow::Result;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::mcp::{Tool, ToolResult};
use crate::meta::Meta;
use crate::profile;
use crate::search::{DEFAULT_MAX_FILE_BYTES, SearchHit, SearchOpts};
use crate::state;

const DESCRIPTION: &str = "Combined glob + grep + read. Returns ranked snippets across matched files. Use this instead of chaining Glob → Grep → Read. Always returns snippets only, never full file contents.";

const DEFAULT_MAX_RESULTS: usize = 50;
const DEFAULT_CONTEXT_LINES: u32 = 2;

pub fn tool() -> Tool {
    Tool {
        name: "relaywash__Search".into(),
        description: DESCRIPTION.into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "paths": { "type": "array", "items": {"type":"string"}, "description": "Glob patterns. Default: [\"**/*\"] minus .gitignore." },
                "content": { "type": "string", "description": "Regex to match in file contents." },
                "symbol": { "type": "string", "description": "Identifier to find (word-boundary search). Use this OR `content`." },
                "maxResults": { "type": "integer", "minimum": 1, "default": DEFAULT_MAX_RESULTS },
                "contextLines": { "type": "integer", "minimum": 0, "default": DEFAULT_CONTEXT_LINES },
                "maxFileBytes": { "type": "integer", "minimum": 0, "description": "Skip files larger than this. 0 disables the cap. Default ~10MB." },
                "rank": { "type": "string", "enum": ["matches","mtime","path-depth"], "default": "matches" },
                "cwd": { "type": "string", "description": "Search root. Defaults to process.cwd()." }
            },
            "additionalProperties": false
        }),
        handler: Box::new(|args, _ctx| run(args)),
    }
}

fn run(args: &Value) -> Result<ToolResult> {
    let cwd: PathBuf = args
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    // Profile-aware defaults: when the agent omits an arg, fall back to the active
    // per-repo profile if it has a value, else the static default. The tool *schema*
    // never changes (cache safety — see src/profile.rs).
    let active_profile = profile::get();
    let prof = &active_profile.tools.search;
    let max_results = args
        .get("maxResults")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .or(prof.max_results)
        .unwrap_or(DEFAULT_MAX_RESULTS);
    let context_lines = args
        .get("contextLines")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .or(prof.context_lines)
        .unwrap_or(DEFAULT_CONTEXT_LINES);
    // 0 explicitly disables the size cap, whether passed in the call or set on the
    // profile. Omitted everywhere → static default (~10MB).
    let max_file_bytes: Option<u64> = {
        let raw = args
            .get("maxFileBytes")
            .and_then(|v| v.as_u64())
            .or(prof.max_file_bytes)
            .unwrap_or(DEFAULT_MAX_FILE_BYTES);
        if raw == 0 { None } else { Some(raw) }
    };
    let rank = args
        .get("rank")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| prof.rank.clone())
        .unwrap_or_else(|| "matches".into());
    let paths: Vec<String> = args
        .get("paths")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|s| s.as_str().map(String::from)).collect())
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| vec!["**/*".into()]);

    let symbol = args.get("symbol").and_then(|v| v.as_str());
    let content = args.get("content").and_then(|v| v.as_str());

    // Side-effect: note the symbol so a subsequent Read can use it for body inclusion.
    state::note_searched_symbol(symbol.or(content));

    let pattern = match (content, symbol) {
        (Some(p), _) if !p.is_empty() => Some(p.to_string()),
        (_, Some(s)) if !s.is_empty() => Some(format!(r"\b{}\b", regex::escape(s))),
        _ => None,
    };

    let output = crate::search::run(SearchOpts {
        cwd: cwd.clone(),
        pattern: pattern.clone(),
        paths,
        context_lines,
        max_file_bytes,
    })?;

    let pattern_was_used = pattern.is_some();
    let ranked = rank_results(output.hits, &rank, &cwd);
    let truncated = ranked.len() > max_results;
    let results: Vec<SearchHit> = ranked.into_iter().take(max_results).collect();

    // Cap `skipped` so a monorepo with thousands of vendored bundles or binaries
    // can't blow up the response. Mirror the `maxResults` budget — agents that
    // ask for 50 hits don't want 50k skipped entries either.
    let skipped_total = output.skipped.len();
    let skipped_truncated = skipped_total > max_results;
    let skipped: Vec<_> = output.skipped.into_iter().take(max_results).collect();

    let replaces: Vec<&str> = if pattern_was_used {
        if results.iter().any(|r| !r.snippet.is_empty()) {
            vec!["Glob", "Grep", "Read"]
        } else {
            vec!["Glob", "Grep"]
        }
    } else {
        vec!["Glob"]
    };
    let collapsed = (results.len() * replaces.len()).max(1) as u32;
    let value = json!({
        "results": results,
        "truncated": truncated,
        "skipped": skipped,
        "skippedTotal": skipped_total,
        "skippedTruncated": skipped_truncated,
    });
    Ok(ToolResult::new("relaywash__Search", value)
        .with_meta(Meta::new(replaces.iter().map(|s| s.to_string()), collapsed)))
}

fn rank_results(mut results: Vec<SearchHit>, mode: &str, cwd: &std::path::Path) -> Vec<SearchHit> {
    match mode {
        "mtime" => {
            let mut with_mtime: Vec<(SearchHit, u128)> = results
                .into_iter()
                .map(|r| {
                    let mtime = std::fs::metadata(cwd.join(&r.path))
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis())
                        .unwrap_or(0);
                    (r, mtime)
                })
                .collect();
            with_mtime.sort_by(|a, b| b.1.cmp(&a.1));
            with_mtime.into_iter().map(|x| x.0).collect()
        }
        "path-depth" => {
            results.sort_by(|a, b| {
                let da = a.path.matches('/').count();
                let db = b.path.matches('/').count();
                da.cmp(&db).then_with(|| a.path.cmp(&b.path))
            });
            results
        }
        _ => {
            // "matches" (default)
            results.sort_by(|a, b| {
                b.match_count
                    .cmp(&a.match_count)
                    .then_with(|| a.path.cmp(&b.path))
            });
            results
        }
    }
}
