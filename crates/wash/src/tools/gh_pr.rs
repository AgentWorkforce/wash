//! relaywash__GhPR — structured PR access via the `gh` CLI.

use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::process::Command;

use crate::mcp::{Tool, ToolResult};
use crate::meta::Meta;

const DESCRIPTION: &str = "Structured PR access (replaces gh pr view/list/diff and gh api repos/.../pulls). Returns a small subset of fields by default; use `fields` to expand. Bodies and diff hunks are truncated.";

const VIEW_DEFAULT_FIELDS: &[&str] = &[
    "number",
    "title",
    "state",
    "author",
    "headRefName",
    "baseRefName",
    "mergeable",
    "isDraft",
];
const LIST_FIELDS: &[&str] = &["number", "title", "state", "author", "updatedAt"];

const DEFAULT_MAX_DIFF_LINES: usize = 200;
const DEFAULT_MAX_COMMENTS: usize = 20;
const BODY_TRUNCATE: usize = 1500;
const COMMENT_TRUNCATE: usize = 500;

pub fn tool() -> Tool {
    Tool {
        name: "relaywash__GhPR".into(),
        description: DESCRIPTION.into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "op": {"type":"string","enum":["view","list","diff","comments"]},
                "number": {"type":"integer"},
                "repo": {"type":"string"},
                "fields": {"type":"array","items":{"type":"string"}},
                "maxComments": {"type":"integer","default": DEFAULT_MAX_COMMENTS},
                "maxDiffLines": {"type":"integer","default": DEFAULT_MAX_DIFF_LINES},
                "cwd": {"type":"string"}
            },
            "required": ["op"],
            "additionalProperties": false
        }),
        handler: Box::new(|args, _ctx| run(args)),
    }
}

fn run(args: &Value) -> Result<ToolResult> {
    let op = args
        .get("op")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing op"))?
        .to_string();
    let cwd: PathBuf = args
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    let replaces = match op.as_str() {
        "view" => "Bash:gh-pr-view",
        "list" => "Bash:gh-pr-list",
        "diff" => "Bash:gh-pr-diff",
        "comments" => "Bash:gh-api-pr-comments",
        other => bail!("unknown op: {other}"),
    };

    let value = match op.as_str() {
        "view" => view(&cwd, args)?,
        "list" => list(&cwd, args)?,
        "diff" => diff(&cwd, args)?,
        "comments" => comments(&cwd, args)?,
        _ => unreachable!(),
    };
    let meta = Meta::new([replaces.to_string()], 1);
    let mut out = match value {
        Value::Object(o) => o,
        other => {
            let mut m = Map::new();
            m.insert("data".into(), other);
            m
        }
    };
    out.insert("_meta".into(), serde_json::to_value(&meta)?);
    Ok(ToolResult::new("relaywash__GhPR", Value::Object(out)))
}

fn gh(cwd: &std::path::Path, args: &[&str]) -> Result<String> {
    let out = Command::new("gh").args(args).current_dir(cwd).output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(if !out.stderr.is_empty() {
            &out.stderr
        } else {
            &out.stdout
        });
        return Err(anyhow!("gh {} failed: {}", args.join(" "), err.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn view(cwd: &std::path::Path, args: &Value) -> Result<Value> {
    let number = args
        .get("number")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("GhPR view requires `number`"))?;
    let custom_fields: Vec<String> = args
        .get("fields")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|s| s.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let fields: Vec<&str> = if custom_fields.is_empty() {
        VIEW_DEFAULT_FIELDS.to_vec()
    } else {
        custom_fields.iter().map(|s| s.as_str()).collect()
    };
    let fields_csv = fields.join(",");
    let n = number.to_string();
    let mut cmd: Vec<&str> = vec!["pr", "view", &n, "--json", &fields_csv];
    let repo = args.get("repo").and_then(|v| v.as_str()).map(String::from);
    if let Some(r) = &repo {
        cmd.push("--repo");
        cmd.push(r);
    }
    let raw = gh(cwd, &cmd)?;
    let mut parsed: Value = serde_json::from_str(&raw)?;
    if let Some(obj) = parsed.as_object_mut() {
        if let Some(author) = obj.get_mut("author") {
            if let Some(login) = author.as_object().and_then(|a| a.get("login")).cloned() {
                *author = login;
            }
        }
        if let Some(body) = obj.get_mut("body").and_then(|v| v.as_str().map(String::from)) {
            if body.chars().count() > BODY_TRUNCATE {
                obj.insert(
                    "body".into(),
                    Value::String(format!("{}\n... (truncated)", truncate_chars(&body, BODY_TRUNCATE))),
                );
            }
        }
    }
    Ok(parsed)
}

fn list(cwd: &std::path::Path, args: &Value) -> Result<Value> {
    let fields_csv = LIST_FIELDS.join(",");
    let mut cmd: Vec<&str> = vec!["pr", "list", "--json", &fields_csv, "--limit", "30"];
    let repo = args.get("repo").and_then(|v| v.as_str()).map(String::from);
    if let Some(r) = &repo {
        cmd.push("--repo");
        cmd.push(r);
    }
    let raw = gh(cwd, &cmd)?;
    let parsed: Value = serde_json::from_str(&raw)?;
    let arr = parsed
        .as_array()
        .ok_or_else(|| anyhow!("gh pr list: expected JSON array"))?;
    let pulls: Vec<Value> = arr
        .iter()
        .map(|p| {
            json!({
                "number": p.get("number"),
                "title": p.get("title"),
                "state": p.get("state"),
                "author": p
                    .get("author")
                    .and_then(|a| a.as_object().and_then(|o| o.get("login").cloned()))
                    .or_else(|| p.get("author").cloned()),
                "updatedAt": p.get("updatedAt"),
            })
        })
        .collect();
    Ok(json!({ "pulls": pulls }))
}

#[derive(Serialize)]
struct DiffFile {
    path: String,
    added: u32,
    removed: u32,
    hunks: String,
    truncated: bool,
}

fn diff(cwd: &std::path::Path, args: &Value) -> Result<Value> {
    let number = args
        .get("number")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("GhPR diff requires `number`"))?;
    let max_lines = args
        .get("maxDiffLines")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_MAX_DIFF_LINES);
    let n = number.to_string();
    let mut cmd: Vec<&str> = vec!["pr", "diff", &n];
    let repo = args.get("repo").and_then(|v| v.as_str()).map(String::from);
    if let Some(r) = &repo {
        cmd.push("--repo");
        cmd.push(r);
    }
    let raw = gh(cwd, &cmd)?;
    let files = parse_per_file_diffs(&raw, max_lines);
    Ok(json!({
        "number": number,
        "files": files,
        "total": files.len(),
    }))
}

fn parse_per_file_diffs(raw: &str, max_lines: usize) -> Vec<DiffFile> {
    let mut out = Vec::new();
    let mut blocks: Vec<&str> = raw.split("\ndiff --git ").collect();
    if blocks.is_empty() {
        return out;
    }
    let first = blocks.remove(0);
    let mut iter: Vec<&str> = if let Some(stripped) = first.strip_prefix("diff --git ") {
        let mut v = vec![stripped];
        v.extend(blocks);
        v
    } else {
        blocks
    };
    iter.retain(|b| !b.trim().is_empty());

    for b in iter {
        let mut lines = b.split('\n');
        let header = lines.next().unwrap_or("");
        let path = parse_diff_header(header).unwrap_or_else(|| header.to_string());
        let mut added = 0u32;
        let mut removed = 0u32;
        let mut hunk_lines: Vec<&str> = Vec::new();
        let mut in_hunk = false;
        for l in lines {
            if l.starts_with("@@") {
                in_hunk = true;
                hunk_lines.push(l);
                continue;
            }
            if !in_hunk {
                continue;
            }
            hunk_lines.push(l);
            if l.starts_with('+') && !l.starts_with("+++") {
                added += 1;
            } else if l.starts_with('-') && !l.starts_with("---") {
                removed += 1;
            }
        }
        let total = hunk_lines.len();
        let (body, truncated) = if total > max_lines {
            let half = max_lines / 2;
            let head: Vec<&str> = hunk_lines.iter().take(half).copied().collect();
            let tail: Vec<&str> = hunk_lines
                .iter()
                .rev()
                .take(half)
                .copied()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let body = format!(
                "{}\n... ({} lines truncated) ...\n{}",
                head.join("\n"),
                total - max_lines,
                tail.join("\n"),
            );
            (body, true)
        } else {
            (hunk_lines.join("\n"), false)
        };
        out.push(DiffFile {
            path,
            added,
            removed,
            hunks: body,
            truncated,
        });
    }
    out
}

fn parse_diff_header(header: &str) -> Option<String> {
    let a_start = header.find("a/")?;
    let after_a = &header[a_start + 2..];
    let b_start = after_a.find(" b/")?;
    Some(after_a[b_start + 3..].trim().to_string())
}

/// Char-aware truncation. Byte-slicing a UTF-8 string at a fixed byte offset panics
/// when the cut lands mid-codepoint (common with emoji and non-ASCII text in PR bodies
/// and comments).
fn truncate_chars(s: &str, limit: usize) -> String {
    s.chars().take(limit).collect()
}

fn comments(cwd: &std::path::Path, args: &Value) -> Result<Value> {
    let number = args
        .get("number")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("GhPR comments requires `number`"))?;
    let max = args
        .get("maxComments")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_MAX_COMMENTS);
    let repo = args.get("repo").and_then(|v| v.as_str()).map(String::from);
    let repo_seg = repo.unwrap_or_else(|| "{owner}/{repo}".to_string());
    let review_url = format!("repos/{repo_seg}/pulls/{number}/comments");
    let issue_url = format!("repos/{repo_seg}/issues/{number}/comments");
    let review_raw = gh(cwd, &["api", &review_url])?;
    let issue_raw = gh(cwd, &["api", &issue_url])?;
    let review: Value = serde_json::from_str(&review_raw)?;
    let issues: Value = serde_json::from_str(&issue_raw)?;
    let trim = |s: &str| -> String {
        if s.chars().count() > COMMENT_TRUNCATE {
            format!("{}\n... (truncated)", truncate_chars(s, COMMENT_TRUNCATE))
        } else {
            s.to_string()
        }
    };
    let mut comments_out: Vec<Value> = Vec::new();
    if let Some(arr) = issues.as_array() {
        for c in arr {
            comments_out.push(json!({
                "author": c.get("user").and_then(|u| u.get("login")),
                "body": trim(c.get("body").and_then(|b| b.as_str()).unwrap_or("")),
                "createdAt": c.get("created_at"),
            }));
        }
    }
    if let Some(arr) = review.as_array() {
        for c in arr {
            comments_out.push(json!({
                "author": c.get("user").and_then(|u| u.get("login")),
                "body": trim(c.get("body").and_then(|b| b.as_str()).unwrap_or("")),
                "createdAt": c.get("created_at"),
                "path": c.get("path"),
                "line": c.get("line").or_else(|| c.get("original_line")),
            }));
        }
    }
    comments_out.sort_by(|a, b| {
        let ka = a.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");
        let kb = b.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");
        ka.cmp(kb)
    });
    let total = comments_out.len();
    comments_out.truncate(max);
    Ok(json!({
        "number": number,
        "comments": comments_out,
        "total": total,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_diff_header() {
        assert_eq!(
            parse_diff_header("a/src/foo.ts b/src/foo.ts").as_deref(),
            Some("src/foo.ts")
        );
    }

    #[test]
    fn per_file_diff_truncates_long_hunks() {
        let body: String = (0..300).map(|i| format!("+line {i}\n")).collect();
        let raw = format!(
            "diff --git a/big.ts b/big.ts\nindex abcd..efgh 100644\n--- a/big.ts\n+++ b/big.ts\n@@ -1,1 +1,300 @@\n{body}"
        );
        let files = parse_per_file_diffs(&raw, 50);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.path, "big.ts");
        assert!(f.truncated);
        assert!(f.hunks.contains("lines truncated"));
        assert!(f.added > 0);
    }

    #[test]
    fn per_file_diff_short_not_truncated() {
        let raw = "diff --git a/short.ts b/short.ts\nindex abc..def 100644\n--- a/short.ts\n+++ b/short.ts\n@@ -1,1 +1,2 @@\n line one\n+line two\n";
        let files = parse_per_file_diffs(raw, 200);
        assert_eq!(files.len(), 1);
        assert!(!files[0].truncated);
        assert_eq!(files[0].added, 1);
    }
}
