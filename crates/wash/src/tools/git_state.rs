//! relaywash__GitState — structured git status/diff/log/show.
//!
//! Returns file lists + summary stats; per-file diffs are truncated. Replaces raw
//! `git status` / `git diff` / `git log` / `git show` Bash calls.

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Value, json};
use std::process::{Command, Stdio};

use crate::mcp::{Tool, ToolResult};
use crate::meta::Meta;

const DESCRIPTION: &str = "Structured git status/diff/log/show. Returns file lists + summary stats; per-file diffs are truncated. Use this instead of raw `git status`/`git diff`/`git log`/`git show` Bash calls.";

const DEFAULT_MAX_FILES: usize = 50;
const DEFAULT_MAX_LINES: usize = 200;

pub fn tool() -> Tool {
    Tool {
        name: "relaywash__GitState".into(),
        description: DESCRIPTION.into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["status","diff","log","show"] },
                "paths": { "type": "array", "items": {"type":"string"} },
                "revision": { "type": "string" },
                "base": { "type": "string" },
                "maxFiles": { "type": "integer", "default": DEFAULT_MAX_FILES },
                "maxLines": { "type": "integer", "default": DEFAULT_MAX_LINES, "description": "Max diff lines per file." },
                "withBody": { "type": "boolean", "default": false, "description": "`log` only — include commit body." },
                "cwd": { "type": "string" }
            },
            "required": ["op"],
            "additionalProperties": false
        }),
        handler: Box::new(|args, _ctx| {
            let parsed = Args::parse(args)?;
            let op = parsed.op;
            let mut value = run(parsed)?;
            if let Value::Object(map) = &mut value {
                map.insert(
                    "_meta".into(),
                    serde_json::to_value(Meta::new([format!("Bash:git-{}", op_label(op))], 1))?,
                );
            }
            Ok(ToolResult::new("relaywash__GitState", value))
        }),
    }
}

fn op_label(op: Op) -> &'static str {
    match op {
        Op::Status => "status",
        Op::Diff => "diff",
        Op::Log => "log",
        Op::Show => "show",
    }
}

#[derive(Debug)]
struct Args {
    op: Op,
    paths: Vec<String>,
    revision: Option<String>,
    base: Option<String>,
    max_files: usize,
    max_lines: usize,
    with_body: bool,
    cwd: String,
}

#[derive(Debug, Clone, Copy)]
enum Op {
    Status,
    Diff,
    Log,
    Show,
}

impl Args {
    fn parse(v: &Value) -> Result<Self> {
        let op = match v.get("op").and_then(|x| x.as_str()) {
            Some("status") => Op::Status,
            Some("diff") => Op::Diff,
            Some("log") => Op::Log,
            Some("show") => Op::Show,
            Some(other) => bail!("unknown op: {other}"),
            None => bail!("missing op"),
        };
        let paths: Vec<String> = v
            .get("paths")
            .and_then(|x| x.as_array())
            .map(|arr| arr.iter().filter_map(|s| s.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let revision = v.get("revision").and_then(|x| x.as_str()).map(String::from);
        let base = v.get("base").and_then(|x| x.as_str()).map(String::from);
        let max_files = v
            .get("maxFiles")
            .and_then(|x| x.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_FILES);
        let max_lines = v
            .get("maxLines")
            .and_then(|x| x.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_LINES);
        let with_body = v.get("withBody").and_then(|x| x.as_bool()).unwrap_or(false);
        let cwd = v
            .get("cwd")
            .and_then(|x| x.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| ".".into())
            });
        Ok(Self {
            op,
            paths,
            revision,
            base,
            max_files,
            max_lines,
            with_body,
            cwd,
        })
    }
}

fn run(a: Args) -> Result<Value> {
    match a.op {
        Op::Status => Ok(serde_json::to_value(git_status(&a)?)?),
        Op::Log => Ok(serde_json::to_value(git_log(&a)?)?),
        Op::Diff | Op::Show => Ok(serde_json::to_value(git_diff_or_show(&a)?)?),
    }
}

fn git(cwd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("spawn git {}", args.join(" ")))?;
    if !out.status.success() {
        return Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(if !out.stderr.is_empty() { &out.stderr } else { &out.stdout }),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[derive(Serialize)]
struct StatusOut {
    branch: String,
    ahead: u32,
    behind: u32,
    files: Vec<StatusFile>,
}

#[derive(Serialize)]
struct StatusFile {
    path: String,
    change: String,
}

fn git_status(a: &Args) -> Result<StatusOut> {
    let branch = git(&a.cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();

    let (mut ahead, mut behind) = (0u32, 0u32);
    if let Ok(s) = git(&a.cwd, &["rev-list", "--left-right", "--count", "@{u}...HEAD"]) {
        let mut parts = s.split_whitespace();
        if let (Some(b), Some(aa)) = (parts.next(), parts.next()) {
            behind = b.parse().unwrap_or(0);
            ahead = aa.parse().unwrap_or(0);
        }
    }

    let mut cmd: Vec<&str> = vec!["status", "--porcelain=v1"];
    if !a.paths.is_empty() {
        cmd.push("--");
        for p in &a.paths {
            cmd.push(p);
        }
    }
    let raw = git(&a.cwd, &cmd)?;
    let files = raw
        .lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let code = line.get(..2).unwrap_or("");
            let path = line.get(3..).unwrap_or("").to_string();
            StatusFile { path, change: code_to_change(code) }
        })
        .collect();
    Ok(StatusOut { branch, ahead, behind, files })
}

fn code_to_change(code: &str) -> String {
    let c = code.replace(' ', "");
    match c.as_str() {
        "M" | "MM" => "modified".into(),
        "A" => "added".into(),
        "D" => "deleted".into(),
        "R" => "renamed".into(),
        "??" => "untracked".into(),
        other => other.trim().to_string(),
    }
}

#[derive(Serialize)]
struct LogOut {
    commits: Vec<Commit>,
}

#[derive(Serialize)]
struct Commit {
    sha: String,
    author: String,
    date: String,
    subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
}

fn git_log(a: &Args) -> Result<LogOut> {
    let sep1 = "\x1f";
    let sep2 = "\x1e";
    let pretty = format!(
        "--pretty=format:%H{s1}%an{s1}%ad{s1}%s{s1}%b{s2}",
        s1 = sep1,
        s2 = sep2,
    );
    let n_arg = a.max_files.to_string();
    let mut cmd: Vec<&str> = vec!["log", &pretty, "--date=iso-strict", "-n", &n_arg];
    if let Some(rev) = &a.revision {
        cmd.push(rev);
    }
    if !a.paths.is_empty() {
        cmd.push("--");
        for p in &a.paths {
            cmd.push(p);
        }
    }
    let raw = git(&a.cwd, &cmd)?;

    let mut commits = Vec::new();
    for block in raw.split(sep2) {
        let trimmed = block.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.splitn(5, sep1);
        let sha = parts.next().unwrap_or("");
        let author = parts.next().unwrap_or("");
        let date = parts.next().unwrap_or("");
        let subject = parts.next().unwrap_or("");
        let body = parts.next().unwrap_or("");
        let mut c = Commit {
            sha: sha.chars().take(12).collect(),
            author: author.into(),
            date: date.into(),
            subject: subject.into(),
            body: None,
        };
        if a.with_body && !body.trim().is_empty() {
            c.body = Some(body.trim().into());
        }
        commits.push(c);
    }
    Ok(LogOut { commits })
}

#[derive(Serialize)]
struct DiffOut {
    summary: String,
    files: Vec<DiffFile>,
    truncated: bool,
}

#[derive(Serialize)]
struct DiffFile {
    path: String,
    added: u32,
    removed: u32,
    hunks: String,
    truncated: bool,
}

fn git_diff_or_show(a: &Args) -> Result<DiffOut> {
    let head = String::from("HEAD");
    let revision = a.revision.as_ref().unwrap_or(&head).clone();

    let mut stat_cmd: Vec<String> = match a.op {
        Op::Show => vec!["show".into(), revision.clone()],
        _ => {
            let mut v = vec!["diff".into()];
            match (&a.base, &a.revision) {
                (Some(b), Some(r)) => v.push(format!("{b}..{r}")),
                (None, Some(r)) => v.push(r.clone()),
                _ => {}
            }
            v
        }
    };
    stat_cmd.push("--stat=200".into());
    if !a.paths.is_empty() {
        stat_cmd.push("--".into());
        for p in &a.paths {
            stat_cmd.push(p.clone());
        }
    }
    let stat = git(&a.cwd, &stat_cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;

    let mut diff_cmd: Vec<String> = match a.op {
        Op::Show => vec!["show".into(), revision, "--no-color".into()],
        _ => {
            let mut v = vec!["diff".into(), "--no-color".into()];
            match (&a.base, &a.revision) {
                (Some(b), Some(r)) => v.push(format!("{b}..{r}")),
                (None, Some(r)) => v.push(r.clone()),
                _ => {}
            }
            v
        }
    };
    if !a.paths.is_empty() {
        diff_cmd.push("--".into());
        for p in &a.paths {
            diff_cmd.push(p.clone());
        }
    }
    let raw = git(&a.cwd, &diff_cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;
    let per_file = parse_per_file_diffs(&raw, a.max_lines);
    let total = per_file.len();
    let limited: Vec<DiffFile> = per_file.into_iter().take(a.max_files).collect();

    let summary = stat
        .trim()
        .lines()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");

    Ok(DiffOut { summary, files: limited, truncated: total > a.max_files })
}

fn parse_per_file_diffs(raw: &str, max_lines: usize) -> Vec<DiffFile> {
    let mut out = Vec::new();
    let mut blocks = raw.split("\ndiff --git ").peekable();
    // The first split element starts before the first `diff --git`. If `raw` itself starts
    // with `diff --git`, prepend a marker; otherwise discard the prelude.
    let first = blocks.next().unwrap_or("");
    let mut iter: Vec<&str> = if first.starts_with("diff --git ") {
        let mut v = vec![&first["diff --git ".len()..]];
        v.extend(blocks);
        v
    } else {
        blocks.collect()
    };
    // Drop empty leading blocks if any.
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
            let tail: Vec<&str> = hunk_lines.iter().rev().take(half).copied().collect::<Vec<_>>().into_iter().rev().collect();
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

        out.push(DiffFile { path, added, removed, hunks: body, truncated });
    }
    out
}

fn parse_diff_header(header: &str) -> Option<String> {
    // Matches "a/<src> b/<dst>" — we want <dst>.
    let a_start = header.find("a/")?;
    let after_a = &header[a_start + 2..];
    let b_start = after_a.find(" b/")?;
    let dst = &after_a[b_start + 3..];
    Some(dst.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command as ShellCommand;
    use tempfile::TempDir;

    fn init_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        let p = dir.path();
        run_in(p, &["init", "-q", "-b", "main"]);
        run_in(p, &["config", "user.email", "test@example.com"]);
        run_in(p, &["config", "user.name", "test"]);
        run_in(p, &["config", "commit.gpgsign", "false"]);
        dir
    }

    fn run_in(cwd: &Path, args: &[&str]) {
        let r = ShellCommand::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git");
        assert!(r.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&r.stderr));
    }

    fn write(cwd: &Path, name: &str, body: &str) {
        std::fs::write(cwd.join(name), body).unwrap();
    }

    #[test]
    fn status_lists_untracked_and_modified() {
        let dir = init_repo();
        let p = dir.path();
        write(p, "a.txt", "hello\n");
        run_in(p, &["add", "a.txt"]);
        run_in(p, &["commit", "-q", "-m", "init"]);
        write(p, "b.txt", "new\n");
        write(p, "a.txt", "hello modified\n");
        let args = Args::parse(&json!({"op":"status","cwd": p.to_string_lossy()})).unwrap();
        let s = git_status(&args).unwrap();
        assert_eq!(s.branch, "main");
        let paths: Vec<&str> = s.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"a.txt"));
        assert!(paths.contains(&"b.txt"));
    }

    #[test]
    fn log_returns_commits() {
        let dir = init_repo();
        let p = dir.path();
        write(p, "a.txt", "1\n");
        run_in(p, &["add", "a.txt"]);
        run_in(p, &["commit", "-q", "-m", "first"]);
        write(p, "a.txt", "2\n");
        run_in(p, &["commit", "-q", "-am", "second"]);
        let args = Args::parse(&json!({"op":"log","cwd": p.to_string_lossy(),"maxFiles": 10})).unwrap();
        let out = git_log(&args).unwrap();
        assert_eq!(out.commits.len(), 2);
        assert_eq!(out.commits[0].subject, "second");
        assert_eq!(out.commits[1].subject, "first");
    }

    #[test]
    fn diff_truncates_long_hunks() {
        let dir = init_repo();
        let p = dir.path();
        write(p, "a.txt", "x\n");
        run_in(p, &["add", "a.txt"]);
        run_in(p, &["commit", "-q", "-m", "init"]);
        let big = (0..500).map(|i| format!("line {i}\n")).collect::<String>();
        write(p, "a.txt", &big);
        let args = Args::parse(&json!({
            "op":"diff",
            "cwd": p.to_string_lossy(),
            "maxLines": 20,
        })).unwrap();
        let out = git_diff_or_show(&args).unwrap();
        let f = &out.files[0];
        assert!(f.truncated, "expected truncation");
        assert!(f.hunks.contains("lines truncated"));
    }
}
