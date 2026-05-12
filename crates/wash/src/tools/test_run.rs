//! relaywash__TestRun — structured runner output: counts + failed test summaries.

use anyhow::Result;
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

use crate::mcp::{Tool, ToolResult};
use crate::meta::Meta;

const DESCRIPTION: &str = "Run tests and return structured counts + failure summaries. Use `failuresOnly` (default true) to elide passing-test noise. Use `getFailureLog: <name>` to fetch the log slice for a single failure from a previous run.";

const DEFAULT_MAX_FAILURES: usize = 10;

pub fn tool() -> Tool {
    Tool {
        name: "relaywash__TestRun".into(),
        description: DESCRIPTION.into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "runner": {
                    "type": "string",
                    "enum": ["auto","pnpm","npm","yarn","jest","pytest","go","cargo","node"],
                    "default": "auto"
                },
                "pattern": {"type":"string","description":"Test name filter passed to the runner."},
                "paths": {"type":"array","items":{"type":"string"}},
                "failuresOnly": {"type":"boolean","default": true},
                "maxFailures": {"type":"integer","default": DEFAULT_MAX_FAILURES},
                "getFailureLog": {
                    "type": "string",
                    "description": "Fetch the log slice for one named failure (from a previous run)."
                },
                "cwd": {"type":"string"}
            },
            "additionalProperties": false
        }),
        handler: Box::new(|args, _ctx| run(args)),
    }
}

#[derive(Debug, Clone, Serialize)]
struct Failure {
    name: String,
    file: String,
    message: String,
}

#[derive(Debug, Clone, Default)]
struct ParseOut {
    passed: u32,
    failed: u32,
    skipped: u32,
    failures: Vec<Failure>,
}

fn run(args: &Value) -> Result<ToolResult> {
    if let Some(name) = args.get("getFailureLog").and_then(|v| v.as_str()) {
        return ok_value(fetch_failure_slice(name)?);
    }

    let cwd: PathBuf = args
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let pattern = args.get("pattern").and_then(|v| v.as_str()).map(String::from);
    let paths: Vec<String> = args
        .get("paths")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let failures_only = args.get("failuresOnly").and_then(|v| v.as_bool()).unwrap_or(true);
    let max_failures = args
        .get("maxFailures")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_MAX_FAILURES);

    let requested = args.get("runner").and_then(|v| v.as_str()).unwrap_or("auto");
    let runner = if requested == "auto" {
        detect_runner(&cwd)
    } else {
        requested.to_string()
    };

    let cmd = build_command(&runner, pattern.as_deref(), &paths);
    let Some(cmd) = cmd else {
        return ok_value(json!({
            "runner": runner,
            "passed": 0,
            "failed": 0,
            "skipped": 0,
            "duration": 0,
            "failures": [],
            "fullLogPath": Value::Null,
            "error": format!("no command for runner: {runner}"),
        }));
    };

    let t0 = Instant::now();
    let out = Command::new(&cmd[0]).args(&cmd[1..]).current_dir(&cwd).output();
    let duration = t0.elapsed().as_millis() as u64;
    let (stdout, stderr, baseline) = match out {
        Ok(o) => {
            // Baseline is the raw byte count the agent would have paid for, computed
            // from the original stdout/stderr so lossy UTF-8 decoding can't skew it.
            let baseline = (o.stdout.len() + o.stderr.len()) as u64;
            (
                String::from_utf8_lossy(&o.stdout).into_owned(),
                String::from_utf8_lossy(&o.stderr).into_owned(),
                baseline,
            )
        }
        Err(e) => {
            return ok_value(json!({
                "runner": runner,
                "passed": 0,
                "failed": 0,
                "skipped": 0,
                "duration": duration,
                "failures": [],
                "fullLogPath": Value::Null,
                "error": format!("spawn {} failed: {e}", cmd[0]),
            }));
        }
    };
    let raw = format!("{stdout}{stderr}");
    let log_path = crate::tools::build::write_log("testrun", &raw).ok();

    let parsed = parse_runner_output(&runner, &raw);
    let failures: Vec<Failure> = if failures_only {
        parsed.failures.iter().take(max_failures).cloned().collect()
    } else {
        parsed.failures.clone()
    };

    ok_value_with_baseline(json!({
        "runner": runner,
        "passed": parsed.passed,
        "failed": parsed.failed,
        "skipped": parsed.skipped,
        "duration": duration,
        "failures": failures,
        "fullLogPath": log_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
    }), baseline)
}

fn ok_value(value: Value) -> Result<ToolResult> {
    Ok(ToolResult::new("relaywash__TestRun", value)
        .with_meta(Meta::new(["Bash:test".to_string()], 1)))
}

fn ok_value_with_baseline(value: Value, baseline: u64) -> Result<ToolResult> {
    Ok(ToolResult::new("relaywash__TestRun", value)
        .with_meta(Meta::new(["Bash:test".to_string()], 1).with_baseline(baseline)))
}

fn detect_runner(cwd: &Path) -> String {
    let has = |p: &str| cwd.join(p).exists();
    if has("Cargo.toml") {
        return "cargo".into();
    }
    if has("go.mod") {
        return "go".into();
    }
    if has("pytest.ini") || has("pyproject.toml") {
        return "pytest".into();
    }
    if has("jest.config.js") || has("jest.config.ts") || has("jest.config.cjs") {
        return "jest".into();
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
    "node".into()
}

fn build_command(runner: &str, pattern: Option<&str>, paths: &[String]) -> Option<Vec<String>> {
    let s = |x: &str| x.to_string();
    Some(match runner {
        "pnpm" => {
            let mut v = vec![s("pnpm"), s("test")];
            if let Some(p) = pattern {
                v.extend([s("--"), s("-t"), p.into()]);
            }
            v.extend(paths.iter().cloned());
            v
        }
        "npm" => {
            let mut v = vec![s("npm"), s("test"), s("--")];
            if let Some(p) = pattern {
                v.extend([s("-t"), p.into()]);
            }
            v.extend(paths.iter().cloned());
            v
        }
        "yarn" => {
            let mut v = vec![s("yarn"), s("test")];
            if let Some(p) = pattern {
                v.extend([s("-t"), p.into()]);
            }
            v.extend(paths.iter().cloned());
            v
        }
        "jest" => {
            let mut v = vec![s("npx"), s("jest")];
            if let Some(p) = pattern {
                v.extend([s("-t"), p.into()]);
            }
            v.extend(paths.iter().cloned());
            v
        }
        "pytest" => {
            let mut v = vec![s("pytest")];
            if let Some(p) = pattern {
                v.extend([s("-k"), p.into()]);
            }
            v.extend(paths.iter().cloned());
            v
        }
        "go" => {
            let mut v = vec![s("go"), s("test")];
            if !paths.is_empty() {
                v.extend(paths.iter().cloned());
            } else {
                v.push(s("./..."));
            }
            if let Some(p) = pattern {
                v.extend([s("-run"), p.into()]);
            }
            v
        }
        "cargo" => {
            let mut v = vec![s("cargo"), s("test")];
            if let Some(p) = pattern {
                v.push(p.into());
            }
            v
        }
        "node" => {
            let mut v = vec![s("node"), s("--test")];
            if let Some(p) = pattern {
                // node --test exposes test-name filtering via --test-name-pattern.
                v.extend([s("--test-name-pattern"), p.into()]);
            }
            if !paths.is_empty() {
                v.extend(paths.iter().cloned());
            } else {
                v.push(s("test/"));
            }
            v
        }
        _ => return None,
    })
}

fn parse_runner_output(runner: &str, raw: &str) -> ParseOut {
    match runner {
        "pytest" => parse_pytest(raw),
        "go" => parse_go_test(raw),
        "cargo" => parse_cargo_test(raw),
        "node" => parse_node_test(raw),
        _ => parse_jest(raw),
    }
}

fn parse_pytest(raw: &str) -> ParseOut {
    static PASSED: OnceLock<Regex> = OnceLock::new();
    static FAILED: OnceLock<Regex> = OnceLock::new();
    static SKIPPED: OnceLock<Regex> = OnceLock::new();
    static FAIL_LINE: OnceLock<Regex> = OnceLock::new();
    let mut out = ParseOut::default();
    if let Some(c) = PASSED.get_or_init(|| Regex::new(r"(\d+)\s+passed").unwrap()).captures(raw) {
        out.passed = c[1].parse().unwrap_or(0);
    }
    if let Some(c) = FAILED.get_or_init(|| Regex::new(r"(\d+)\s+failed").unwrap()).captures(raw) {
        out.failed = c[1].parse().unwrap_or(0);
    }
    if let Some(c) = SKIPPED.get_or_init(|| Regex::new(r"(\d+)\s+skipped").unwrap()).captures(raw) {
        out.skipped = c[1].parse().unwrap_or(0);
    }
    let fail_re = FAIL_LINE.get_or_init(|| Regex::new(r"FAILED\s+(\S+)::(\S+)").unwrap());
    for cap in fail_re.captures_iter(raw) {
        out.failures.push(Failure {
            name: cap[2].into(),
            file: cap[1].into(),
            message: String::new(),
        });
    }
    out
}

fn parse_go_test(raw: &str) -> ParseOut {
    static PASS: OnceLock<Regex> = OnceLock::new();
    static FAIL: OnceLock<Regex> = OnceLock::new();
    let pass_re = PASS.get_or_init(|| Regex::new(r"(?m)^---\s*PASS:\s").unwrap());
    let fail_re = FAIL.get_or_init(|| Regex::new(r"(?m)^---\s*FAIL:\s+(\S+)").unwrap());
    let mut out = ParseOut::default();
    out.passed = pass_re.find_iter(raw).count() as u32;
    for cap in fail_re.captures_iter(raw) {
        out.failed += 1;
        out.failures.push(Failure {
            name: cap[1].into(),
            file: String::new(),
            message: String::new(),
        });
    }
    out
}

fn parse_cargo_test(raw: &str) -> ParseOut {
    static SUMMARY: OnceLock<Regex> = OnceLock::new();
    let re = SUMMARY.get_or_init(|| {
        Regex::new(
            r"(?i)test result: (?:ok|FAILED)\.\s+(\d+)\s+passed;\s+(\d+)\s+failed;\s+(\d+)\s+ignored",
        )
        .unwrap()
    });
    let mut out = ParseOut::default();
    if let Some(c) = re.captures(raw) {
        out.passed = c[1].parse().unwrap_or(0);
        out.failed = c[2].parse().unwrap_or(0);
        out.skipped = c[3].parse().unwrap_or(0);
    }
    // The "failures:" block lists failed test names.
    if let Some(idx) = raw.find("failures:\n\n") {
        let after = &raw[idx + "failures:\n\n".len()..];
        for line in after.lines() {
            let t = line.trim();
            if t.is_empty() {
                break;
            }
            if !t.starts_with(char::is_whitespace) && !line.starts_with(' ') && !line.starts_with('\t') {
                // Stop when we leave the indented block.
                if !t.starts_with("test ") && !t.contains("::") {
                    break;
                }
            }
            out.failures.push(Failure {
                name: t.into(),
                file: String::new(),
                message: String::new(),
            });
        }
    }
    out
}

fn parse_node_test(raw: &str) -> ParseOut {
    static PASS: OnceLock<Regex> = OnceLock::new();
    static FAIL: OnceLock<Regex> = OnceLock::new();
    static SKIP: OnceLock<Regex> = OnceLock::new();
    static NOT_OK: OnceLock<Regex> = OnceLock::new();
    let mut out = ParseOut::default();
    if let Some(c) = PASS.get_or_init(|| Regex::new(r"#\s*pass\s+(\d+)").unwrap()).captures(raw) {
        out.passed = c[1].parse().unwrap_or(0);
    }
    if let Some(c) = FAIL.get_or_init(|| Regex::new(r"#\s*fail\s+(\d+)").unwrap()).captures(raw) {
        out.failed = c[1].parse().unwrap_or(0);
    }
    if let Some(c) = SKIP.get_or_init(|| Regex::new(r"#\s*skipped\s+(\d+)").unwrap()).captures(raw) {
        out.skipped = c[1].parse().unwrap_or(0);
    }
    let not_ok = NOT_OK.get_or_init(|| Regex::new(r"(?m)^not ok \d+ - (.+)$").unwrap());
    for cap in not_ok.captures_iter(raw) {
        out.failures.push(Failure {
            name: cap[1].trim().into(),
            file: String::new(),
            message: String::new(),
        });
    }
    out
}

fn parse_jest(raw: &str) -> ParseOut {
    static SUMMARY: OnceLock<Regex> = OnceLock::new();
    let re = SUMMARY.get_or_init(|| {
        Regex::new(r"Tests?:\s*(?:(\d+)\s+failed,\s*)?(?:(\d+)\s+skipped,\s*)?(\d+)\s+passed").unwrap()
    });
    let mut out = ParseOut::default();
    if let Some(c) = re.captures(raw) {
        out.failed = c.get(1).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        out.skipped = c.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        out.passed = c.get(3).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
    }
    // Find each `● <name>` block — Rust's `regex` doesn't support lookahead so we walk manually.
    let delimiters = ["\n●", "\nTest Suites:", "\nTests:"];
    let mut iter = raw.match_indices('●').peekable();
    while let Some((pos, _)) = iter.next() {
        // Require it to be at the start of a line (or start of raw).
        if pos > 0 && raw.as_bytes().get(pos - 1).copied() != Some(b'\n') {
            continue;
        }
        let after_marker = &raw[pos + '●'.len_utf8()..];
        let Some(name_end) = after_marker.find('\n') else { continue };
        let name = after_marker[..name_end].trim().to_string();
        if name.is_empty() {
            continue;
        }
        // Body starts after the blank line following the name.
        let rest = &after_marker[name_end..];
        let body_start_rel = match rest.find("\n\n") {
            Some(i) => i + 2,
            None => continue,
        };
        let body_abs_start = pos + '●'.len_utf8() + name_end + body_start_rel;
        let mut body_end = raw.len();
        for delim in delimiters {
            if let Some(rel) = raw[body_abs_start..].find(delim) {
                body_end = body_end.min(body_abs_start + rel);
            }
        }
        let body = &raw[body_abs_start..body_end];
        let truncated = if body.len() > 1000 { &body[..1000] } else { body };
        out.failures.push(Failure {
            name,
            file: String::new(),
            message: truncated.into(),
        });
    }
    out
}

fn fetch_failure_slice(name: &str) -> Result<Value> {
    let dir = crate::tools::build::log_dir();
    if !dir.exists() {
        return Ok(json!({"found": false}));
    }
    let mut entries: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|r| r.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x == "log")
                .unwrap_or(false)
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());
    let Some(latest) = entries.last() else {
        return Ok(json!({"found": false}));
    };
    let body = std::fs::read_to_string(latest.path())?;
    let Some(idx) = body.find(name) else {
        return Ok(json!({"found": false}));
    };
    let start = idx.saturating_sub(500);
    let end = (idx + 2000).min(body.len());
    Ok(json!({
        "found": true,
        "slice": &body[start..end],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pytest_summary_parsed() {
        let raw = "============== 3 failed, 7 passed, 2 skipped in 1.23s ===============\nFAILED tests/foo.py::test_bar\nFAILED tests/baz.py::test_qux\n";
        let out = parse_pytest(raw);
        assert_eq!(out.passed, 7);
        assert_eq!(out.failed, 3);
        assert_eq!(out.skipped, 2);
        assert_eq!(out.failures.len(), 2);
        assert_eq!(out.failures[0].name, "test_bar");
        assert_eq!(out.failures[0].file, "tests/foo.py");
    }

    #[test]
    fn cargo_summary_parsed() {
        let raw = "running 5 tests\ntest a ... ok\ntest b ... FAILED\n\nfailures:\n\n    crate::module::test_b\n\ntest result: FAILED. 4 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let out = parse_cargo_test(raw);
        assert_eq!(out.passed, 4);
        assert_eq!(out.failed, 1);
        assert_eq!(out.skipped, 0);
        assert_eq!(out.failures.len(), 1);
        assert!(out.failures[0].name.contains("test_b"));
    }

    #[test]
    fn go_test_failures_counted() {
        let raw = "=== RUN   TestFoo\n--- PASS: TestFoo (0.00s)\n=== RUN   TestBar\n--- FAIL: TestBar (0.01s)\n    bar_test.go:5: assertion failed\nFAIL\n";
        let out = parse_go_test(raw);
        assert_eq!(out.passed, 1);
        assert_eq!(out.failed, 1);
        assert_eq!(out.failures.len(), 1);
        assert_eq!(out.failures[0].name, "TestBar");
    }

    #[test]
    fn node_test_summary_parsed() {
        let raw = "TAP version 13\nnot ok 1 - my failing test\n# pass 3\n# fail 1\n# skipped 0\n";
        let out = parse_node_test(raw);
        assert_eq!(out.passed, 3);
        assert_eq!(out.failed, 1);
        assert_eq!(out.failures.len(), 1);
        assert_eq!(out.failures[0].name, "my failing test");
    }

    #[test]
    fn jest_summary_parsed() {
        let raw = "● App > renders\n\n  Expected: 1\n  Received: 2\n\n● App > clicks\n\n  TypeError: x is not a function\n\nTests: 2 failed, 1 skipped, 5 passed, 8 total\n";
        let out = parse_jest(raw);
        assert_eq!(out.passed, 5);
        assert_eq!(out.failed, 2);
        assert_eq!(out.skipped, 1);
        assert_eq!(out.failures.len(), 2);
        assert_eq!(out.failures[0].name, "App > renders");
        assert!(out.failures[0].message.contains("Expected: 1"));
        assert_eq!(out.failures[1].name, "App > clicks");
    }

    #[test]
    fn detect_runner_prefers_cargo() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        assert_eq!(detect_runner(dir.path()), "cargo");
    }
}
