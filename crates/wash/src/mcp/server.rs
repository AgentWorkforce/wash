use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::cell::Cell;
use std::io::{Read, Write};

use crate::meta::Meta;

const PROTOCOL_VERSION: &str = "2024-11-05";

pub type ToolHandler = Box<dyn Fn(&Value, &ToolContext) -> Result<ToolResult> + Send + Sync>;

pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub handler: ToolHandler,
}

pub struct ToolContext {
    pub session_id: Option<String>,
}

pub struct ToolResult {
    pub tool_name: String,
    pub value: Value,
    pub meta: Option<Meta>,
}

impl ToolResult {
    pub fn new(tool_name: impl Into<String>, value: Value, meta: Option<Meta>) -> Self {
        Self { tool_name: tool_name.into(), value, meta }
    }
}

pub type PostCall = Box<dyn Fn(&ToolResult) + Send + Sync>;

pub struct McpServer {
    name: String,
    version: String,
    tools: Vec<Tool>,
    post_call: Option<PostCall>,
    session_id: Option<String>,
    /// Set by `dispatch` when an `shutdown`/`exit` request arrives. The run loop checks
    /// this between frames and returns Ok(()) so Drops fire and any buffered ledger
    /// state is flushed via normal scope exit (rather than `process::exit` which skips
    /// destructors).
    shutdown: Cell<bool>,
}

impl McpServer {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            tools: Vec::new(),
            post_call: None,
            session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
            shutdown: Cell::new(false),
        }
    }

    pub fn register(&mut self, tool: Tool) {
        self.tools.push(tool);
    }

    pub fn set_post_call(&mut self, f: PostCall) {
        self.post_call = Some(f);
    }

    pub fn run(self) -> Result<()> {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut reader = stdin.lock();
        let mut writer = stdout.lock();
        let mut buf: Vec<u8> = Vec::with_capacity(8192);
        let mut chunk = [0u8; 4096];

        loop {
            let n = match reader.read(&mut chunk) {
                Ok(0) => return Ok(()),
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            buf.extend_from_slice(&chunk[..n]);

            while let Some(msg_bytes) = take_framed_message(&mut buf) {
                let body = String::from_utf8(msg_bytes)
                    .context("MCP frame body is not valid UTF-8")?;
                let parsed: Value = match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Value::Array(arr) = parsed {
                    for m in arr {
                        self.handle_one(&m, &mut writer)?;
                        if self.shutdown.get() {
                            return Ok(());
                        }
                    }
                } else {
                    self.handle_one(&parsed, &mut writer)?;
                    if self.shutdown.get() {
                        return Ok(());
                    }
                }
            }
        }
    }

    fn handle_one(&self, msg: &Value, writer: &mut impl Write) -> Result<()> {
        let Some(method) = msg.get("method").and_then(|m| m.as_str()) else {
            return Ok(());
        };
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(json!({}));

        let result = self.dispatch(method, &params);

        if let Some(id) = id {
            if !id.is_null() {
                match result {
                    Ok(Some(value)) => {
                        send(writer, &json!({"jsonrpc":"2.0","id":id,"result":value}))?;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        send(
                            writer,
                            &json!({
                                "jsonrpc":"2.0","id":id,
                                "error":{"code":-32000,"message":e.to_string()}
                            }),
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn dispatch(&self, method: &str, params: &Value) -> Result<Option<Value>> {
        match method {
            "initialize" => Ok(Some(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "serverInfo": {"name": self.name, "version": self.version},
                "capabilities": {"tools": {}},
            }))),
            "initialized" | "notifications/initialized" => Ok(None),
            "tools/list" => {
                let arr: Vec<Value> = self
                    .tools
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "inputSchema": t.input_schema,
                        })
                    })
                    .collect();
                Ok(Some(json!({"tools": arr})))
            }
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("tools/call: missing name"))?;
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let tool = self
                    .tools
                    .iter()
                    .find(|t| t.name == name)
                    .ok_or_else(|| anyhow!("Unknown tool: {name}"))?;
                let ctx = ToolContext { session_id: self.session_id.clone() };
                let out = (tool.handler)(&args, &ctx)?;
                if let Some(cb) = &self.post_call {
                    cb(&out);
                }
                Ok(Some(format_tool_result(&out)))
            }
            "ping" => Ok(Some(json!({}))),
            "shutdown" | "exit" => {
                self.shutdown.set(true);
                Ok(None)
            }
            _ => Err(anyhow!("Method not implemented: {method}")),
        }
    }
}

fn format_tool_result(r: &ToolResult) -> Value {
    // The model reads `content[].text`. Use compact JSON — pretty-printing roughly
    // doubles the whitespace tokens for nested results, which defeats the whole point
    // of this server. Hosts that prefer a parsed view read `structuredContent`.
    let text = serde_json::to_string(&r.value).unwrap_or_else(|_| "{}".into());
    json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": r.value,
    })
}

fn send(writer: &mut impl Write, payload: &Value) -> Result<()> {
    let body = serde_json::to_vec(payload)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

/// Pull one complete LSP-style framed message from `buf`. Returns the body bytes and trims
/// `buf` past the consumed prefix. Returns `None` if no complete message is available yet.
///
/// On malformed headers (non-UTF-8 or missing `Content-Length`), drains the bad header
/// up through the `\r\n\r\n` and returns `None`. Without this consumption the bad header
/// would stay at the front of the buffer forever and wedge the parser on subsequent reads.
fn take_framed_message(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let header_end = find_subseq(buf, b"\r\n\r\n")?;
    let drop_header = || -> Option<Vec<u8>> { None };
    let Ok(header) = std::str::from_utf8(&buf[..header_end]) else {
        buf.drain(..header_end + 4);
        return drop_header();
    };
    let Some(len) = parse_content_length(header) else {
        buf.drain(..header_end + 4);
        return drop_header();
    };
    let start = header_end + 4;
    if buf.len() < start + len {
        return None;
    }
    let body = buf[start..start + len].to_vec();
    buf.drain(..start + len);
    Some(body)
}

fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_content_length(header: &str) -> Option<usize> {
    for line in header.split("\r\n") {
        let mut parts = line.splitn(2, ':');
        let key = parts.next()?.trim();
        let val = parts.next()?.trim();
        if key.eq_ignore_ascii_case("Content-Length") {
            return val.parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_one_message() {
        let mut buf = b"Content-Length: 5\r\n\r\nhello".to_vec();
        let body = take_framed_message(&mut buf).unwrap();
        assert_eq!(body, b"hello");
        assert!(buf.is_empty());
    }

    #[test]
    fn frames_partial_returns_none() {
        let mut buf = b"Content-Length: 10\r\n\r\nhello".to_vec();
        assert!(take_framed_message(&mut buf).is_none());
    }

    #[test]
    fn malformed_header_drops_and_recovers() {
        // Bad header followed by a valid frame. The first call drops the bad header,
        // the second consumes the good one.
        let mut buf = b"Garbage: yes\r\n\r\nContent-Length: 5\r\n\r\nhello".to_vec();
        assert!(take_framed_message(&mut buf).is_none());
        let body = take_framed_message(&mut buf).expect("recover after bad header");
        assert_eq!(body, b"hello");
    }

    #[test]
    fn frames_multiple_back_to_back() {
        let mut buf = b"Content-Length: 2\r\n\r\nhi\
                        Content-Length: 5\r\n\r\nworld"
            .to_vec();
        let a = take_framed_message(&mut buf).unwrap();
        let b = take_framed_message(&mut buf).unwrap();
        assert_eq!(a, b"hi");
        assert_eq!(b, b"world");
    }
}
