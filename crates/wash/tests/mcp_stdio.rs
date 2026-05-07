//! End-to-end MCP test: spawn the binary, drive it over stdio, assert framed responses.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
