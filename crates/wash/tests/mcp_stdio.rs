//! End-to-end MCP test: spawn the binary, drive it over stdio, assert framed responses.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wash::mcp::{ToolContext, format_tool_result};
use wash::tools;

fn frame(msg: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(msg).unwrap();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut out = header.into_bytes();
    out.extend_from_slice(&body);
    out
}

fn read_one(stream: &mut impl Read, deadline: Instant) -> Option<Value> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while Instant::now() < deadline {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = find_subseq(&buf, b"\r\n\r\n") {
            let header = std::str::from_utf8(&buf[..end]).ok()?;
            let len = header
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length:").map(str::trim))?
                .parse::<usize>()
                .ok()?;
            let start = end + 4;
            while buf.len() < start + len {
                let n = stream.read(&mut chunk).ok()?;
                if n == 0 {
                    return None;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let body = &buf[start..start + len];
            return serde_json::from_slice(body).ok();
        }
    }
    None
}

fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Cache safety: the JSON shape returned by `tools/list` must not depend on the active
/// profile. If it did, the Anthropic prompt cache would invalidate every time the
/// adaptive layer changes a default. This test asserts byte-equality between two
/// listings taken with different `RELAYWASH_PROFILE_PATH` values.
#[test]
fn tools_list_is_byte_stable_across_profiles() {
    let bin = env!("CARGO_BIN_EXE_wash");
    let with_profile = list_tools_with_env(bin, &[("RELAYWASH_PROFILE_PATH", profile_a_path())]);
    let with_other = list_tools_with_env(bin, &[("RELAYWASH_PROFILE_PATH", profile_b_path())]);
    assert_eq!(
        with_profile, with_other,
        "tools/list bytes diverged across profile values — cache safety violated",
    );
}

fn profile_a_path() -> String {
    let dir = std::env::temp_dir().join("wash-test-profile-a");
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("p.json");
    std::fs::write(
        &f,
        r#"{"version":1,"tools":{"search":{"maxResults":10},"read":{"smallFileLines":50}}}"#,
    )
    .unwrap();
    f.to_string_lossy().into_owned()
}

fn profile_b_path() -> String {
    let dir = std::env::temp_dir().join("wash-test-profile-b");
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("p.json");
    std::fs::write(
        &f,
        r#"{"version":1,"tools":{"search":{"maxResults":500,"contextLines":8}}}"#,
    )
    .unwrap();
    f.to_string_lossy().into_owned()
}

fn list_tools_with_env(bin: &str, envs: &[(&str, String)]) -> serde_json::Value {
    let mut cmd = Command::new(bin);
    cmd.arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn wash mcp");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    stdin
        .write_all(&frame(&serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"initialize","params":{}
        })))
        .unwrap();
    stdin
        .write_all(&frame(&serde_json::json!({
            "jsonrpc":"2.0","id":2,"method":"tools/list","params":{}
        })))
        .unwrap();
    stdin.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let _init = read_one(&mut stdout, deadline);
    let list = read_one(&mut stdout, deadline).expect("tools/list");
    drop(stdin);
    let _ = child.wait();
    list["result"].clone()
}

/// Acceptance test for issue #30: every relaywash tool must emit `_meta` in both the
/// structured response and the visible text block. Tools whose handlers shell out (Build,
/// TestRun) are exercised through error paths so the test stays hermetic.
#[test]
fn every_core_tool_emits_meta() {
    let all = tools::all();
    let find = |name: &str| {
        all.iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("missing tool {name}"))
    };
    let ctx = ToolContext { session_id: Some("meta-test".into()) };

    // Search
    let search = find("relaywash__Search");
    let res = (search.handler)(&json!({"paths": ["**/*.toml"], "maxResults": 1}), &ctx).unwrap();
    assert_meta(&res, &["Glob"]);

    // Read (small file inside this crate)
    let read = find("relaywash__Read");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let res = (read.handler)(&json!({"path": path.to_string_lossy()}), &ctx).unwrap();
    assert_meta(&res, &["Read"]);

    // GitState (status of this very repo — guaranteed to be a git worktree under test)
    let git_state = find("relaywash__GitState");
    let res = (git_state.handler)(&json!({"op": "log", "maxFiles": 1}), &ctx).unwrap();
    assert_meta(&res, &["Bash:git-log"]);

    // Build — invoke with an unknown builder so we hit the no-command error path without
    // spawning a real toolchain.
    let build = find("relaywash__Build");
    let res = (build.handler)(&json!({"builder": "unknown-builder"}), &ctx).unwrap();
    assert_meta(&res, &["Bash:build"]);

    // TestRun — same idea: the `getFailureLog` branch reads a tmp dir and returns a
    // structured `{"found": false}` without spawning a runner.
    let test_run = find("relaywash__TestRun");
    let res =
        (test_run.handler)(&json!({"getFailureLog": "definitely-not-a-real-test-name"}), &ctx)
            .unwrap();
    assert_meta(&res, &["Bash:test"]);
}

/// Assert that the formatter injects `_meta` into both `structuredContent` and the visible
/// text block, with the expected `replaces` entries and the central `schemaVersion`/
/// `responseBytes` fields populated.
fn assert_meta(res: &wash::mcp::ToolResult, expected_replaces: &[&str]) {
    let formatted = format_tool_result(res);
    let structured_meta = &formatted["structuredContent"]["_meta"];
    assert!(structured_meta.is_object(), "structured _meta missing");
    let replaces: Vec<&str> = structured_meta["replaces"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    for r in expected_replaces {
        assert!(replaces.contains(r), "expected replaces to contain {r}, got {replaces:?}");
    }
    assert!(structured_meta["responseBytes"].as_u64().unwrap() > 0);
    assert!(structured_meta["schemaVersion"].as_u64().unwrap() >= 1);

    let text = formatted["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert!(parsed.get("_meta").is_some(), "text block missing _meta");
}

/// Issue #23: tool execution failures must surface as a `result` with `isError: true`
/// so the model can read the failure text, NOT as a JSON-RPC `error` (which would
/// render as a generic "tool failed" with no detail in Claude Code). Protocol-level
/// failures (unknown method, missing tool name, etc.) still take the `error` path —
/// those are exercised in `tools_call_protocol_errors_use_jsonrpc_error`.
#[test]
fn tools_call_handler_errors_become_is_error_result() {
    let bin = env!("CARGO_BIN_EXE_wash");
    let mut child = Command::new(bin)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wash mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();

    stdin
        .write_all(&frame(&json!({
            "jsonrpc":"2.0","id":1,"method":"initialize","params":{}
        })))
        .unwrap();
    // Read with a missing `path` argument: the handler returns Err. Pre-fix this
    // emitted a JSON-RPC error with code -32000; per the MCP spec it should be a
    // normal result with `isError: true` and the message in `content[].text`.
    stdin
        .write_all(&frame(&json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"relaywash__Read","arguments":{}}
        })))
        .unwrap();
    stdin.flush().unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let _init = read_one(&mut stdout, deadline).expect("initialize response");
    let resp = read_one(&mut stdout, deadline).expect("tools/call response");

    assert_eq!(resp["id"], 2);
    assert!(resp.get("error").is_none(), "handler Err should NOT surface as JSON-RPC error: {resp}");
    let result = &resp["result"];
    assert_eq!(result["isError"], json!(true), "expected isError: true, got {result}");
    let text = result["content"][0]["text"].as_str().expect("error text");
    assert!(!text.is_empty(), "error text should be non-empty");

    drop(stdin);
    let _ = child.wait();
}

/// Counterpart to the previous test: protocol-level failures (unknown tool name,
/// missing `name` field, etc.) still ride the JSON-RPC `error` channel. Only the
/// handler-returned `Err` case was reclassified.
#[test]
fn tools_call_protocol_errors_use_jsonrpc_error() {
    let bin = env!("CARGO_BIN_EXE_wash");
    let mut child = Command::new(bin)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wash mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();

    stdin
        .write_all(&frame(&json!({
            "jsonrpc":"2.0","id":1,"method":"initialize","params":{}
        })))
        .unwrap();
    stdin
        .write_all(&frame(&json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"relaywash__DoesNotExist","arguments":{}}
        })))
        .unwrap();
    stdin.flush().unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let _init = read_one(&mut stdout, deadline).expect("initialize response");
    let resp = read_one(&mut stdout, deadline).expect("tools/call response");

    assert_eq!(resp["id"], 2);
    assert!(resp.get("result").is_none(), "unknown tool should not return a result: {resp}");
    assert_eq!(resp["error"]["code"], json!(-32000));
    let msg = resp["error"]["message"].as_str().expect("error message");
    assert!(msg.contains("Unknown tool"), "unexpected error message: {msg}");

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn mcp_initialize_and_tools_list() {
    let bin = env!("CARGO_BIN_EXE_wash");
    let mut child = Command::new(bin)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wash mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();

    stdin
        .write_all(&frame(&json!({
            "jsonrpc":"2.0","id":1,"method":"initialize","params":{}
        })))
        .unwrap();
    stdin
        .write_all(&frame(&json!({
            "jsonrpc":"2.0","id":2,"method":"tools/list","params":{}
        })))
        .unwrap();
    stdin.flush().unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let init = read_one(&mut stdout, deadline).expect("initialize response");
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["serverInfo"]["name"], "relaywash");

    let list = read_one(&mut stdout, deadline).expect("tools/list response");
    assert_eq!(list["id"], 2);
    let tools = list["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for expected in [
        "relaywash__Search",
        "relaywash__Read",
        "relaywash__Edit",
        "relaywash__GitState",
        "relaywash__TestRun",
        "relaywash__Build",
        "relaywash__GhPR",
    ] {
        assert!(names.contains(&expected), "missing {expected}; got {names:?}");
    }

    drop(stdin);
    let _ = child.wait();
}
