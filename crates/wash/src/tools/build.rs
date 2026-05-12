//! relaywash__Build — structured build output: one line on success; parsed errors on failure.

use anyhow::Result;
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::mcp::{Tool, ToolResult};
use crate::meta::Meta;

const DESCRIPTION: &str = "Run the project build and return a tiny structured response. Successful builds return one line; failing tsc/cargo/go builds return parsed `errors[]`; other builders return an `errorTail`.";

const DEFAULT_TAIL_LINES: usize = 50;

pub fn tool() -> Tool {
    Tool {
        name: "relaywash__Build".into(),
        description: DESCRIPTION.into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "builder": {
                    "type": "string",
                    "enum": ["auto","pnpm","npm","yarn","tsc","cargo","go","vite","webpack"],
                    "default": "auto"
                },
                "target": {"type":"string"},
                "errorTailLines": {"type":"integer","default": DEFAULT_TAIL_LINES},
                "cwd": {"type":"string"}
            },
            "additionalProperties": false
        }),
        handler: Box::new(|args, _ctx| run(args)),
    }
}

#[derive(Debug, Clone, Serialize)]
struct BuildError {
    file: String,
    line: u32,
    col: u32,
    message: String,
}

fn run(args: &Value) -> Result<ToolResult> {
    let cwd: PathBuf = args
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let target = args.get("target").and_then(|v| v.as_str()).map(String::from);
    let tail_lines = args
        .get("errorTailLines")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_TAIL_LINES);
    let requested = args.get("builder").and_then(|v| v.as_str()).unwrap_or("auto");
    let builder = if requested == "auto" {
        detect_builder(&cwd)
    } else {
        requested.to_string()
    };

    let cmd = build_command(&builder, target.as_deref());
    let Some(cmd) = cmd else {
        return ok_value(json!({
            "builder": builder,
            "success": false,
            "duration": 0,
            "errorTail": format!("no command for builder: {builder}"),
            "fullLogPath": Value::Null,
            "_meta": Meta::new(["Bash:build".to_string()], 1),
        }));
    };

    let t0 = Instant::now();
    let out = Command::new(&cmd[0]).args(&cmd[1..]).current_dir(&cwd).output();
    let duration = t0.elapsed().as_millis() as u64;
    let (stdout, stderr, status_code) = match out {
        Ok(o) => (
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
            o.status.code(),
        ),
        Err(e) => {
            return ok_value(json!({
                "builder": builder,
                "success": false,
                "duration": duration,
                "errorTail": format!("spawn {} failed: {}", cmd[0], e),
                "fullLogPath": Value::Null,
                "_meta": Meta::new(["Bash:build".to_string()], 1),
            }));
        }
    };
    let raw = format!("{stdout}\n{stderr}");
    let log_path = write_log("build", &raw).ok();
    let baseline = raw.len() as u64;

    let success = status_code == Some(0);
    if success {
        return ok_value(json!({
            "builder": builder,
            "success": true,
            "duration": duration,
            "fullLogPath": log_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "_meta": Meta::new(["Bash:build".to_string()], 1).with_baseline(baseline),
        }));
    }

    let errors = parse_errors(&builder, &raw);
    if !errors.is_empty() {
        return ok_value(json!({
            "builder": builder,
            "success": false,
            "duration": duration,
            "errors": errors,
            "fullLogPath": log_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "_meta": Meta::new(["Bash:build".to_string()], 1).with_baseline(baseline),
        }));
    }
    let tail = tail_lines_of(&raw, tail_lines);
    ok_value(json!({
        "builder": builder,
        "success": false,
        "duration": duration,
        "errorTail": tail,
        "fullLogPath": log_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
        "_meta": Meta::new(["Bash:build".to_string()], 1).with_baseline(baseline),
    }))
}

fn ok_value(value: Value) -> Result<ToolResult> {
    Ok(ToolResult::new("relaywash__Build", value))
}

fn detect_builder(cwd: &Path) -> String {
    let has = |p: &str| cwd.join(p).exists();
    if has("Cargo.toml") {
        return "cargo".into();
    }
    if has("go.mod") {
        return "go".into();
    }
    if has("tsconfig.json") && !has("package.json") {
        return "tsc".into();
    }
    if has("pnpm-lock.yaml") {
        return "pnpm".into();
    }
    if has("yarn.lock") {
        return "yarn".into();
    }
    if has("package-lock.json") || has("package.json") {
        return "npm".into();
    }
    "tsc".into()
}

fn build_command(builder: &str, target: Option<&str>) -> Option<Vec<String>> {
    let s = |x: &str| x.to_string();
    Some(match builder {
        "pnpm" => {
            let mut v = vec![s("pnpm"), s("build")];
            if let Some(t) = target {
                v.push(t.into());
            }
            v
        }
        "npm" => vec![s("npm"), s("run"), s("build")],
        "yarn" => vec![s("yarn"), s("build")],
        "tsc" => {
            let mut v = vec![s("npx"), s("tsc")];
            if let Some(t) = target {
                v.push(s("-p"));
                v.push(t.into());
            }
            v
        }
        "cargo" => vec![s("cargo"), s("build")],
        "go" => vec![s("go"), s("build"), target.unwrap_or("./...").into()],
        "vite" => vec![s("npx"), s("vite"), s("build")],
        "webpack" => vec![s("npx"), s("webpack"), s("build")],
        _ => return None,
    })
}

fn parse_errors(builder: &str, raw: &str) -> Vec<BuildError> {
    match builder {
        "tsc" | "pnpm" | "npm" | "yarn" => parse_tsc_errors(raw),
        "cargo" => parse_cargo_errors(raw),
        "go" => parse_go_errors(raw),
        _ => Vec::new(),
    }
}

fn parse_tsc_errors(raw: &str) -> Vec<BuildError> {
    static R1: OnceLock<Regex> = OnceLock::new();
    static R2: OnceLock<Regex> = OnceLock::new();
    let r1 = R1.get_or_init(|| {
        Regex::new(r"(?m)^(.+?)\((\d+),(\d+)\):\s*error\s+TS\d+:\s*(.+)$").unwrap()
    });
    let r2 = R2.get_or_init(|| {
        Regex::new(r"(?m)^(.+?):(\d+):(\d+)\s*-\s*error\s+TS\d+:\s*(.+)$").unwrap()
    });
    let mut out = Vec::new();
    for cap in r1.captures_iter(raw) {
        out.push(cap_to_err(&cap, 1, 2, 3, 4));
    }
    for cap in r2.captures_iter(raw) {
        out.push(cap_to_err(&cap, 1, 2, 3, 4));
    }
    out
}

fn parse_cargo_errors(raw: &str) -> Vec<BuildError> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| {
        Regex::new(r"(?m)^error(?:\[E\d+\])?:\s*(.+?)\n\s+-->\s*(.+?):(\d+):(\d+)").unwrap()
    });
    re.captures_iter(raw)
        .map(|cap| BuildError {
            file: cap[2].into(),
            line: cap[3].parse().unwrap_or(0),
            col: cap[4].parse().unwrap_or(0),
            message: cap[1].into(),
        })
        .collect()
}

fn parse_go_errors(raw: &str) -> Vec<BuildError> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"(?m)^(.+?\.go):(\d+):(\d+):\s*(.+)$").unwrap());
    re.captures_iter(raw)
        .map(|cap| cap_to_err(&cap, 1, 2, 3, 4))
        .collect()
}

fn cap_to_err(cap: &regex::Captures, file: usize, line: usize, col: usize, msg: usize) -> BuildError {
    BuildError {
        file: cap[file].into(),
        line: cap[line].parse().unwrap_or(0),
        col: cap[col].parse().unwrap_or(0),
        message: cap[msg].into(),
    }
}

fn tail_lines_of(raw: &str, n: usize) -> String {
    let lines: Vec<&str> = raw.split('\n').collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

pub(crate) fn log_dir() -> PathBuf {
    std::env::temp_dir().join("relaywash-logs")
}

pub(crate) fn write_log(prefix: &str, body: &str) -> std::io::Result<PathBuf> {
    let dir = log_dir();
    std::fs::create_dir_all(&dir)?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{prefix}-{ts}.log"));
    std::fs::write(&path, body)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tsc_paren_format() {
        let raw = "src/foo.ts(12,34): error TS2304: Cannot find name 'bar'.\n";
        let errs = parse_tsc_errors(raw);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].file, "src/foo.ts");
        assert_eq!(errs[0].line, 12);
        assert_eq!(errs[0].col, 34);
        assert!(errs[0].message.contains("bar"));
    }

    #[test]
    fn parses_tsc_dash_format() {
        let raw = "src/foo.ts:5:9 - error TS2304: Cannot find name 'foo'.\n";
        let errs = parse_tsc_errors(raw);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].file, "src/foo.ts");
        assert_eq!(errs[0].line, 5);
        assert_eq!(errs[0].col, 9);
    }

    #[test]
    fn parses_cargo_errors() {
        let raw = "error[E0425]: cannot find value `x` in this scope\n  --> src/main.rs:3:5\n   |\n";
        let errs = parse_cargo_errors(raw);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].file, "src/main.rs");
        assert_eq!(errs[0].line, 3);
        assert_eq!(errs[0].col, 5);
        assert!(errs[0].message.contains("cannot find"));
    }

    #[test]
    fn parses_go_errors() {
        let raw = "./main.go:7:1: undefined: foo\n";
        let errs = parse_go_errors(raw);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].file, "./main.go");
        assert_eq!(errs[0].line, 7);
        assert_eq!(errs[0].col, 1);
    }

    #[test]
    fn detect_builder_prefers_cargo_when_present() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        assert_eq!(detect_builder(dir.path()), "cargo");
    }

    #[test]
    fn detect_builder_prefers_go_when_present() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module x\n").unwrap();
        assert_eq!(detect_builder(dir.path()), "go");
    }

    #[test]
    fn tail_lines_returns_last_n() {
        let raw = (1..=10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let t = tail_lines_of(&raw, 3);
        assert_eq!(t, "line 8\nline 9\nline 10");
    }
}
