//! relaywash__Read — AST-aware read with signatures mode, mtime cache, and heuristics
//! to suppress signatures where they backfire (small files / small functions / recently-searched
//! symbols).

use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::ast::{LineMapEntry, Signatures, extract_signatures, find_body_end};
use crate::language::Language;
use crate::mcp::{Tool, ToolContext, ToolResult};
use crate::meta::Meta;
use crate::profile;
use crate::state;

const DEFAULT_SMALL_FILE_LINES: usize = 200;
const DEFAULT_SMALL_FUNCTION_LINES: usize = 20;
const DESCRIPTION: &str = "AST-aware read. Default mode \"signatures\" returns imports + declarations + signatures (bodies elided) plus a `lineMap` so you can issue precise `mode: \"range\"` follow-ups. Small files come back fully. Repeated reads of an unchanged file in the same session return empty content.";

pub fn tool() -> Tool {
    Tool {
        name: "relaywash__Read".into(),
        description: DESCRIPTION.into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "mode": { "type": "string", "enum": ["signatures","range","full"] },
                "range": {
                    "type": "array",
                    "items": {"type":"integer"},
                    "minItems": 2,
                    "maxItems": 2,
                    "description": "1-based inclusive [start, end] line range."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        handler: Box::new(|args, ctx| run(args, ctx)),
    }
}

fn run(args: &Value, ctx: &ToolContext) -> Result<ToolResult> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing path"))?
        .to_string();
    let mode = args.get("mode").and_then(|v| v.as_str()).map(String::from);
    let range: Option<(usize, usize)> = args
        .get("range")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            if arr.len() == 2 {
                let a = arr[0].as_u64()? as usize;
                let b = arr[1].as_u64()? as usize;
                Some((a, b))
            } else {
                None
            }
        });
    let session_id = ctx.session_id.clone().unwrap_or_else(|| "default".into());

    let language = Language::detect(&path);
    let stat = std::fs::metadata(&path)?;
    let mtime_ms = stat
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let cached = state::read_cache_get(&session_id, &path);
    let unchanged = cached == Some(mtime_ms);

    if unchanged && range.is_none() {
        return Ok(read_result(json!({
            "content": "",
            "truncated": false,
            "languageDetected": language.as_str(),
        })));
    }

    let text = std::fs::read_to_string(&path)?;
    state::read_cache_put(&session_id, &path, mtime_ms);

    if mode.as_deref() == Some("range") || range.is_some() {
        let Some((start, end)) = range else {
            bail!("mode: \"range\" requires `range: [start, end]`");
        };
        if start < 1 || start > end {
            bail!("range must be 1-based inclusive [start, end] with start >= 1 and start <= end");
        }
        let lines: Vec<&str> = text.split('\n').collect();
        let s = start.saturating_sub(1).min(lines.len());
        let e = end.min(lines.len());
        let slice = lines[s..e].join("\n");
        return Ok(read_result(json!({
            "content": slice,
            "truncated": false,
            "languageDetected": language.as_str(),
        })));
    }

    if mode.as_deref() == Some("full") || language == Language::Unknown {
        return Ok(read_result(json!({
            "content": text,
            "truncated": false,
            "languageDetected": language.as_str(),
        })));
    }

    // signatures mode (default)
    let active_profile = profile::get();
    let prof = &active_profile.tools.read;
    let small_file_lines = prof.small_file_lines.unwrap_or(DEFAULT_SMALL_FILE_LINES);
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.len() <= small_file_lines {
        return Ok(read_result(json!({
            "content": text,
            "truncated": false,
            "languageDetected": language.as_str(),
        })));
    }

    let sigs = extract_signatures(&text, language);
    let small_function_lines = prof
        .small_function_lines
        .unwrap_or(DEFAULT_SMALL_FUNCTION_LINES);
    let augmented = augment_with_small_bodies(
        &text,
        &sigs,
        state::last_searched_symbol().as_deref(),
        small_function_lines,
    );

    let baseline = text.len() as u64;
    Ok(ToolResult::new(
        "relaywash__Read",
        json!({
            "content": augmented,
            "truncated": true,
            "languageDetected": language.as_str(),
            "lineMap": sigs.line_map,
        }),
    )
    .with_meta(Meta::new(["Read".to_string()], 1).with_baseline(baseline)))
}

fn read_result(value: Value) -> ToolResult {
    ToolResult::new("relaywash__Read", value).with_meta(Meta::new(["Read".to_string()], 1))
}

fn augment_with_small_bodies(
    full_text: &str,
    sigs: &Signatures,
    searched: Option<&str>,
    small_function_lines: usize,
) -> String {
    if sigs.line_map.is_empty() {
        return sigs.content.clone();
    }
    let full_lines: Vec<&str> = full_text.split('\n').collect();
    let mut sig_lines: Vec<String> = sigs.content.split('\n').map(String::from).collect();
    // `source_lines` is aligned with `sig_lines`: same length, with each entry the
    // 1-based source line that produced that rendered line (or 0 for synthetic lines
    // such as the trailing `}` after an elided body).
    let mut source_lines: Vec<u32> = sigs.source_lines.clone();
    debug_assert_eq!(source_lines.len(), sig_lines.len());

    for entry in &sigs.line_map {
        let header_idx = (entry.line as usize).saturating_sub(1);
        if header_idx >= full_lines.len() {
            continue;
        }
        let body_end = find_body_end(&full_lines, header_idx);
        let body_len = body_end.saturating_sub(header_idx);
        let matches_searched = searched
            .map(|s| s.eq_ignore_ascii_case(&entry.symbol))
            .unwrap_or(false);
        if !matches_searched && body_len > small_function_lines {
            continue;
        }
        // Locate the rendered header by exact source line — not `starts_with`, which
        // picks the wrong header when two share a prefix (e.g., `fn foo` vs `fn foobar`).
        let Some(sig_idx) = source_lines.iter().position(|&r| r == entry.line) else {
            continue;
        };
        let full_body: Vec<String> = full_lines[header_idx..=body_end]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // Replace the signature header + the elided `}` line with the full body. The
        // synthetic `}` we splice over carries source line 0; pad `source_lines` so it
        // stays aligned with `sig_lines` after the replacement.
        let to_replace = if sig_idx + 1 < sig_lines.len() && source_lines[sig_idx + 1] == 0 {
            2
        } else {
            1
        };
        let body_rows: Vec<u32> =
            (header_idx..=body_end).map(|i| i as u32 + 1).collect();
        sig_lines.splice(sig_idx..sig_idx + to_replace, full_body);
        source_lines.splice(sig_idx..sig_idx + to_replace, body_rows);
    }
    sig_lines.join("\n")
}

#[allow(dead_code)]
fn _types(_: LineMapEntry, _: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::ToolContext;
    use std::fs;
    use tempfile::TempDir;

    fn ctx(session: &str) -> ToolContext {
        ToolContext { session_id: Some(session.to_string()) }
    }

    fn call(args: Value, ctx: &ToolContext) -> Result<Value> {
        run(&args, ctx).map(|r| r.value)
    }

    #[test]
    fn small_ts_file_returns_full_content() {
        crate::state::reset();
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("small.ts");
        fs::write(&p, "export const x = 1;\n").unwrap();
        let v = call(json!({"path": p.to_string_lossy()}), &ctx("s1")).unwrap();
        assert_eq!(v["content"], "export const x = 1;\n");
        assert_eq!(v["truncated"], false);
        assert_eq!(v["languageDetected"], "typescript");
    }

    #[test]
    fn unchanged_file_returns_empty_on_repeat() {
        crate::state::reset();
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("a.ts");
        fs::write(&p, "export const x = 1;\n").unwrap();
        let path = p.to_string_lossy().to_string();
        let _first = call(json!({"path": path}), &ctx("s2")).unwrap();
        let second = call(json!({"path": path}), &ctx("s2")).unwrap();
        assert_eq!(second["content"], "");
    }

    #[test]
    fn large_ts_file_returns_signatures_via_tree_sitter() {
        crate::state::reset();
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("big.ts");
        // Construct >200 lines so it triggers signatures mode.
        let mut src = String::from("import { foo } from \"bar\";\n\n");
        src.push_str("export function compute(x: number): number {\n");
        for _ in 0..210 {
            src.push_str("  // padding line\n");
        }
        src.push_str("  return x * 2;\n}\n\n");
        src.push_str("export class Greeter {\n  greet(): string { return \"hi\"; }\n}\n");
        fs::write(&p, &src).unwrap();

        let v = call(json!({"path": p.to_string_lossy()}), &ctx("s3")).unwrap();
        let content = v["content"].as_str().unwrap();
        assert!(content.contains("export function compute"), "header preserved");
        assert!(content.contains("…"), "body elided");
        assert!(!content.contains("padding line"), "body bytes elided");
        assert!(content.contains("export class Greeter"), "class preserved");
        assert_eq!(v["truncated"], true);
        assert!(v["lineMap"].is_array());
    }

    #[test]
    fn range_mode_slices_lines() {
        crate::state::reset();
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("a.ts");
        let body: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        fs::write(&p, &body).unwrap();
        let v = call(
            json!({"path": p.to_string_lossy(), "mode": "range", "range": [3, 5]}),
            &ctx("s4"),
        )
        .unwrap();
        assert_eq!(v["content"], "line 3\nline 4\nline 5");
    }
}
