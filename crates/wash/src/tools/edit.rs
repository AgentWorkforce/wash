//! relaywash__Edit — batched multi-file edits with fuzzy matching and tree-sitter post-check.

use anyhow::{Result, anyhow};
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;

use crate::ast::{parses_cleanly};
use crate::fuzzy::fuzzy_find_all;
use crate::language::Language;
use crate::mcp::{Tool, ToolResult};
use crate::meta::Meta;

const DESCRIPTION: &str = "Batched multi-file edit with fuzzy matching and post-edit syntax check. Pass an array of edits and they apply atomically per-file. Whitespace and visually-equivalent Unicode differences in `oldText` are tolerated for matching only.";

pub fn tool() -> Tool {
    Tool {
        name: "relaywash__Edit".into(),
        description: DESCRIPTION.into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "oldText": { "type": "string" },
                            "newText": { "type": "string" },
                            "fuzzy": { "type": "boolean", "default": true }
                        },
                        "required": ["path","oldText","newText"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["edits"],
            "additionalProperties": false
        }),
        handler: Box::new(|args, _ctx| run(args)),
    }
}

#[derive(Debug, Clone)]
struct EditSpec {
    path: String,
    old_text: String,
    new_text: String,
    fuzzy: bool,
    /// Index in the original input array — preserved through grouping so the output
    /// can be re-ordered to match input order.
    input_index: usize,
}

#[derive(Debug, Clone, Serialize)]
struct EditResult {
    path: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

fn run(args: &Value) -> Result<ToolResult> {
    let edits_arr = args
        .get("edits")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("missing edits"))?;
    if edits_arr.is_empty() {
        return Err(anyhow!("edits must be non-empty"));
    }
    let total = edits_arr.len();
    let mut all: Vec<EditSpec> = Vec::with_capacity(total);
    for (i, e) in edits_arr.iter().enumerate() {
        let path = e
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("edit[{i}].path missing"))?;
        let old_text = e
            .get("oldText")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("edit[{i}].oldText missing"))?;
        let new_text = e
            .get("newText")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("edit[{i}].newText missing"))?;
        let fuzzy = e.get("fuzzy").and_then(|v| v.as_bool()).unwrap_or(true);
        all.push(EditSpec {
            path: path.into(),
            old_text: old_text.into(),
            new_text: new_text.into(),
            fuzzy,
            input_index: i,
        });
    }

    // Group by path, preserving first-seen order of paths.
    let mut grouped: IndexMap<String, Vec<EditSpec>> = IndexMap::new();
    for e in all {
        grouped.entry(e.path.clone()).or_default().push(e);
    }

    // Apply per-file; collect (input_index, EditResult) pairs.
    let mut indexed: Vec<(usize, EditResult)> = Vec::with_capacity(total);
    for (path, edits) in grouped {
        let results = apply_to_file(&path, edits);
        indexed.extend(results);
    }
    // Re-sort by original input order so the response shape matches input.
    indexed.sort_by_key(|(i, _)| *i);
    let results: Vec<EditResult> = indexed.into_iter().map(|(_, r)| r).collect();

    Ok(ToolResult::new(
        "relaywash__Edit",
        json!({
            "results": results,
        }),
    )
    .with_meta(Meta::new(["Edit".to_string()], total as u32)))
}

fn apply_to_file(path: &str, edits: Vec<EditSpec>) -> Vec<(usize, EditResult)> {
    if !Path::new(path).exists() {
        return edits
            .into_iter()
            .map(|e| {
                (
                    e.input_index,
                    EditResult {
                        path: path.to_string(),
                        ok: false,
                        reason: Some("file does not exist".into()),
                    },
                )
            })
            .collect();
    }
    let original = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            let reason = format!("read failed: {e}");
            return edits
                .into_iter()
                .map(|spec| {
                    (
                        spec.input_index,
                        EditResult {
                            path: path.to_string(),
                            ok: false,
                            reason: Some(reason.clone()),
                        },
                    )
                })
                .collect();
        }
    };

    let language = Language::detect(path);
    let clean_before = if language == Language::Unknown {
        true
    } else {
        parses_cleanly(&original, language)
    };

    let mut current = original.clone();
    let mut partial: Vec<(usize, PartialResult)> = Vec::with_capacity(edits.len());

    for edit in edits.iter() {
        let matches = locate(&current, edit);
        if matches.is_empty() {
            partial.push((edit.input_index, PartialResult::Failed("oldText not found".into())));
            return rollback(path, edits, partial, None);
        }
        if matches.len() > 1 {
            let reason = format!(
                "ambiguous match ({} occurrences) — disambiguate by including more context",
                matches.len()
            );
            partial.push((edit.input_index, PartialResult::Failed(reason)));
            return rollback(path, edits, partial, None);
        }
        let (start, end) = matches[0];
        let mut next = String::with_capacity(current.len() - (end - start) + edit.new_text.len());
        next.push_str(&current[..start]);
        next.push_str(&edit.new_text);
        next.push_str(&current[end..]);
        current = next;
        partial.push((edit.input_index, PartialResult::Ok));
    }

    if clean_before
        && language != Language::Unknown
        && !parses_cleanly(&current, language)
    {
        return rollback(
            path,
            edits,
            partial,
            Some("post-edit syntax check failed".into()),
        );
    }

    if let Err(e) = atomic_write(path, &current) {
        let reason = format!("write failed: {e}");
        return rollback(path, edits, partial, Some(reason));
    }

    partial
        .into_iter()
        .map(|(input_idx, p)| {
            (
                input_idx,
                EditResult {
                    path: path.to_string(),
                    ok: matches!(p, PartialResult::Ok),
                    reason: match p {
                        PartialResult::Ok => None,
                        PartialResult::Failed(r) => Some(r),
                    },
                },
            )
        })
        .collect()
}

enum PartialResult {
    Ok,
    Failed(String),
}

/// Build the per-edit response for a failed batch. Every sibling edit in the same file
/// is reported `ok: false` — the file was never written, so an `ok: true` here would be
/// a lie the agent can't see through. The failed edit keeps its own reason; siblings
/// (and any edits past the failure point) get a reason that points at the cause.
fn rollback(
    path: &str,
    edits: Vec<EditSpec>,
    partial: Vec<(usize, PartialResult)>,
    override_reason: Option<String>,
) -> Vec<(usize, EditResult)> {
    let sibling_reason: String = if let Some(ref r) = override_reason {
        format!("rolled back ({r})")
    } else {
        match partial.last() {
            Some((failed_idx, PartialResult::Failed(reason))) => {
                format!("rolled back (sibling edit {failed_idx} failed: {reason})")
            }
            _ => "rolled back".to_string(),
        }
    };

    let partial_count = partial.len();
    let mut out: Vec<(usize, EditResult)> = partial
        .into_iter()
        .map(|(input_idx, p)| {
            let reason = match p {
                PartialResult::Ok => Some(sibling_reason.clone()),
                PartialResult::Failed(r) => Some(r),
            };
            (
                input_idx,
                EditResult {
                    path: path.to_string(),
                    ok: false,
                    reason,
                },
            )
        })
        .collect();
    for spec in edits.into_iter().skip(partial_count) {
        out.push((
            spec.input_index,
            EditResult {
                path: path.to_string(),
                ok: false,
                reason: Some(sibling_reason.clone()),
            },
        ));
    }
    out
}

fn locate(text: &str, edit: &EditSpec) -> Vec<(usize, usize)> {
    let exact = find_all_exact(text, &edit.old_text);
    if !edit.fuzzy {
        return exact;
    }
    if !exact.is_empty() {
        return exact;
    }
    fuzzy_find_all(text, &edit.old_text)
}

/// Write atomically: stage the new contents in a sibling temp file, then rename over the
/// target. A crash mid-write leaves the original file untouched — `std::fs::write` would
/// truncate first and could leave a half-written file behind.
///
/// Preserves the target's existing permissions across the replace. `rename` swaps the
/// inode for our fresh temp file, which would otherwise drop the original mode bits
/// (e.g., a `0755` script becoming non-executable). On Windows, MoveFileEx assigns the
/// destination directory's default ACL on rename — copying permissions onto the temp
/// file before the rename gives the same effective behavior on both platforms.
fn atomic_write(path: &str, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let target = Path::new(path);
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".{file_name}.wash-{pid}-{nanos}.tmp"));
    let original_perms = std::fs::metadata(target).ok().map(|m| m.permissions());

    let write_result = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        if let Some(perms) = &original_perms {
            std::fs::set_permissions(&tmp, perms.clone())?;
        }
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    if let Err(e) = std::fs::rename(&tmp, target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

fn find_all_exact(text: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = text[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        out.push((start, end));
        from = start + needle.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn tmp_file(content: &str, ext: &str) -> (TempDir, String) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(format!("f{ext}"));
        fs::write(&path, content).unwrap();
        (dir, path.to_string_lossy().into_owned())
    }

    fn call(edits: Value) -> Result<Value> {
        run(&json!({"edits": edits})).map(|r| r.value)
    }

    #[test]
    fn single_edit_writes_verbatim_new_text() {
        let (_dir, path) = tmp_file("export const x = 1;\n", ".ts");
        let v = call(json!([{"path": path, "oldText": "const x = 1", "newText": "const x = 42"}])).unwrap();
        assert_eq!(v["results"][0]["ok"], true);
        assert_eq!(fs::read_to_string(&path).unwrap(), "export const x = 42;\n");
    }

    #[test]
    fn batched_edits_across_files() {
        let (_a_dir, a) = tmp_file("a = 1", ".ts");
        let (_b_dir, b) = tmp_file("b = 2", ".ts");
        let v = call(json!([
            {"path": a, "oldText": "a = 1", "newText": "a = 11"},
            {"path": b, "oldText": "b = 2", "newText": "b = 22"},
        ]))
        .unwrap();
        let oks: u32 = v["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| if r["ok"].as_bool().unwrap_or(false) { 1 } else { 0 })
            .sum();
        assert_eq!(oks, 2);
        assert_eq!(fs::read_to_string(&a).unwrap(), "a = 11");
        assert_eq!(fs::read_to_string(&b).unwrap(), "b = 22");
    }

    #[test]
    fn fuzzy_tolerates_tab_vs_spaces() {
        let (_dir, path) = tmp_file("export function foo(x) {\n\treturn x\n}\n", ".ts");
        let v = call(json!([{
            "path": path,
            "oldText": "export function foo(x) {\n    return x\n}",
            "newText": "export function foo(x) {\n  return x + 1\n}"
        }]))
        .unwrap();
        assert_eq!(v["results"][0]["ok"], true, "{:?}", v["results"][0]);
        assert!(fs::read_to_string(&path).unwrap().contains("return x + 1"));
    }

    #[test]
    fn ambiguous_match_rejected() {
        let (_dir, path) = tmp_file("foo();\nfoo();\n", ".ts");
        let v = call(json!([{"path": path, "oldText": "foo();", "newText": "bar();"}])).unwrap();
        assert_eq!(v["results"][0]["ok"], false);
        let reason = v["results"][0]["reason"].as_str().unwrap();
        assert!(reason.contains("ambiguous"), "got reason: {reason}");
    }

    #[test]
    fn post_edit_syntax_check_rolls_back() {
        let before = "export function foo() { return 1 }\n";
        let (_dir, path) = tmp_file(before, ".ts");
        let v = call(json!([{
            "path": path,
            "oldText": "{ return 1 }",
            "newText": "{ return 1"  // unbalanced — strips closing brace
        }]))
        .unwrap();
        assert_eq!(v["results"][0]["ok"], false);
        assert_eq!(fs::read_to_string(&path).unwrap(), before, "file must be unchanged");
    }

    #[test]
    fn missing_old_text_fails_gracefully() {
        let (_dir, path) = tmp_file("hello\n", ".ts");
        let v = call(json!([{"path": path, "oldText": "world", "newText": "X"}])).unwrap();
        assert_eq!(v["results"][0]["ok"], false);
        assert_eq!(v["results"][0]["reason"].as_str().unwrap(), "oldText not found");
    }

    #[test]
    fn second_edit_failure_rolls_back_first() {
        let (_dir, path) = tmp_file("a = 1\nb = 2\n", ".ts");
        let v = call(json!([
            {"path": path, "oldText": "a = 1", "newText": "a = 11"},
            {"path": path, "oldText": "missing", "newText": "X"}
        ]))
        .unwrap();
        // The file was never written — neither edit should report success.
        assert_eq!(v["results"][0]["ok"], false);
        assert_eq!(v["results"][1]["ok"], false);
        let reason0 = v["results"][0]["reason"].as_str().unwrap();
        assert!(
            reason0.contains("sibling edit 1 failed"),
            "expected sibling-fail reason, got: {reason0}"
        );
        assert_eq!(v["results"][1]["reason"].as_str().unwrap(), "oldText not found");
        assert_eq!(fs::read_to_string(&path).unwrap(), "a = 1\nb = 2\n");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = tmp_file("#!/bin/sh\necho original\n", ".sh");
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();

        let v = call(json!([{
            "path": &path,
            "oldText": "echo original",
            "newText": "echo edited"
        }]))
        .unwrap();
        assert_eq!(v["results"][0]["ok"], true);

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "executable bit must survive atomic rename");
        assert!(fs::read_to_string(&path).unwrap().contains("echo edited"));
    }

    #[test]
    fn third_edit_after_failure_marked_rolled_back() {
        let (_dir, path) = tmp_file("a\nb\nc\n", ".ts");
        let v = call(json!([
            {"path": path, "oldText": "a", "newText": "A"},
            {"path": path, "oldText": "missing", "newText": "X"},
            {"path": path, "oldText": "c", "newText": "C"}
        ]))
        .unwrap();
        // All three results must be ok:false — the file was never written.
        for i in 0..3 {
            assert_eq!(v["results"][i]["ok"], false, "edit {i} should be ok:false");
        }
        assert_eq!(v["results"][1]["reason"].as_str().unwrap(), "oldText not found");
        for i in [0usize, 2] {
            let reason = v["results"][i]["reason"].as_str().unwrap();
            assert!(
                reason.contains("sibling edit 1 failed"),
                "edit {i} expected sibling-fail reason, got: {reason}"
            );
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\nb\nc\n");
    }

    #[test]
    fn post_edit_syntax_check_failure_marks_all_failed() {
        let before = "export function foo() { return 1 }\nexport const x = 2;\n";
        let (_dir, path) = tmp_file(before, ".ts");
        let v = call(json!([
            {"path": path, "oldText": "{ return 1 }", "newText": "{ return 1"},
            {"path": path, "oldText": "const x = 2", "newText": "const x = 3"}
        ]))
        .unwrap();
        assert_eq!(v["results"][0]["ok"], false);
        assert_eq!(v["results"][1]["ok"], false);
        for i in 0..2 {
            let reason = v["results"][i]["reason"].as_str().unwrap();
            assert!(
                reason.contains("post-edit syntax check failed"),
                "edit {i} expected post-edit reason, got: {reason}"
            );
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), before, "file must be unchanged");
    }
}
