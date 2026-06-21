use anyhow::{Result, anyhow};
use serde_json::{Map, Value, json};
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
    /// Tool-supplied annotation. The MCP formatter injects this as `_meta` into both the
    /// structured response and the visible text block, computing `responseBytes` along the
    /// way. Carrying `Meta` here (rather than embedding it directly in `value`) keeps the
    /// shape consistent across tools and lets the ledger record it without re-parsing JSON.
    pub meta: Option<Meta>,
}

impl ToolResult {
    pub fn new(tool_name: impl Into<String>, value: Value) -> Self {
        Self {
            tool_name: tool_name.into(),
            value,
            meta: None,
        }
    }

    pub fn with_meta(mut self, meta: Meta) -> Self {
        self.meta = Some(meta);
        self
    }
}

pub struct McpServer {
    name: String,
    version: String,
    tools: Vec<Tool>,
    session_id: Option<String>,
    /// Set by `dispatch` when an `shutdown`/`exit` request arrives. The run loop checks
    /// this between frames and returns Ok(()) so destructors run via normal scope exit
    /// (rather than `process::exit`, which skips them).
    shutdown: Cell<bool>,
}

impl McpServer {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            tools: Vec::new(),
            session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
            shutdown: Cell::new(false),
        }
    }

    pub fn register(&mut self, tool: Tool) {
        self.tools.push(tool);
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
                // A corrupt frame (bad UTF-8, or unparseable JSON below) is skipped, not
                // fatal: this is a long-lived stdio server and one bad frame must not take
                // down every subsequent valid request. Mirrors the header-recovery in
                // `take_framed_message`.
                let Ok(body) = String::from_utf8(msg_bytes) else {
                    continue;
                };
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
            "tools/call" => self.call_tool(params).map(Some),
            "ping" => Ok(Some(json!({}))),
            "shutdown" | "exit" => {
                self.shutdown.set(true);
                Ok(None)
            }
            _ => Err(anyhow!("Method not implemented: {method}")),
        }
    }

    /// Execute a `tools/call` and return its JSON-RPC `result` value.
    ///
    /// The MCP spec splits two kinds of failure, and this method is where that policy
    /// lives: a *protocol* failure (missing `name`, unknown tool) returns `Err`, which
    /// the caller turns into a JSON-RPC `error`; a tool *execution* failure is NOT an
    /// `Err` — it comes back as a normal result with `isError: true` so the model can
    /// read the failure text and react, rather than seeing a generic "tool failed".
    /// Kept separate from `dispatch` so this spec-sensitive contract is unit-testable
    /// without driving the full stdio protocol.
    fn call_tool(&self, params: &Value) -> Result<Value> {
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
        let ctx = ToolContext {
            session_id: self.session_id.clone(),
        };
        Ok(match (tool.handler)(&args, &ctx) {
            Ok(out) => format_tool_result(&out),
            Err(e) => error_tool_result(&e.to_string()),
        })
    }
}

/// The wire shape for a *tool execution* failure: a normal `result` carrying the error
/// text with `isError: true` (see the `tools/call` handler for why this is not a JSON-RPC
/// error). Defined once so the bench harness, which replays tool calls outside the server,
/// cannot drift from what the live server actually emits.
pub fn error_tool_result(message: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": message}],
        "isError": true,
    })
}

pub fn format_tool_result(r: &ToolResult) -> Value {
    // The model reads `content[].text`. Use compact JSON — pretty-printing roughly
    // doubles the whitespace tokens for nested results, which defeats the whole point
    // of this server. Hosts that prefer a parsed view read `structuredContent`.
    let value = inject_meta(r.value.clone(), r.meta.as_ref());
    let text = serde_json::to_string(&value).unwrap_or_else(|_| "{}".into());
    json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": value,
    })
}

/// Inject `_meta` into the structured response. Scalar/array values are wrapped under a
/// `data` key so `_meta` can sit alongside; objects are mutated in place.
///
/// `responseBytes` is the compact-JSON size of the payload *excluding* `_meta` itself,
/// so it reflects the bytes a model actually pays for. Computing it here means tools
/// cannot drift.
fn inject_meta(value: Value, meta: Option<&Meta>) -> Value {
    let Some(meta) = meta else { return value };
    let mut wrapped = match value {
        Value::Object(map) => map,
        other => {
            let mut m = Map::new();
            m.insert("data".into(), other);
            m
        }
    };
    let response_bytes = serde_json::to_vec(&Value::Object(wrapped.clone()))
        .map(|v| v.len() as u64)
        .unwrap_or(0);
    let mut meta = meta.clone();
    meta.response_bytes = Some(response_bytes);
    let meta_value = serde_json::to_value(&meta).unwrap_or(Value::Null);
    wrapped.insert("_meta".into(), meta_value);
    Value::Object(wrapped)
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
    let Ok(header) = std::str::from_utf8(&buf[..header_end]) else {
        buf.drain(..header_end + 4);
        return None;
    };
    let Some(len) = parse_content_length(header) else {
        buf.drain(..header_end + 4);
        return None;
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
    fn formatter_injects_meta_into_object_value() {
        let r = ToolResult::new("relaywash__Demo", json!({"ok": true}))
            .with_meta(Meta::new(["Read".to_string()], 1));
        let out = format_tool_result(&r);
        let structured = &out["structuredContent"];
        let meta = &structured["_meta"];
        assert_eq!(meta["replaces"], json!(["Read"]));
        assert_eq!(meta["collapsedCalls"], 1);
        assert_eq!(meta["schemaVersion"], crate::meta::SCHEMA_VERSION);
        assert!(meta["responseBytes"].as_u64().unwrap() > 0);

        let text = out["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            parsed["_meta"]["schemaVersion"],
            crate::meta::SCHEMA_VERSION
        );
    }

    #[test]
    fn formatter_wraps_non_object_value_under_data() {
        let r = ToolResult::new("relaywash__Demo", json!(["a", "b"]))
            .with_meta(Meta::new(["Read".to_string()], 1));
        let out = format_tool_result(&r);
        let structured = &out["structuredContent"];
        assert_eq!(structured["data"], json!(["a", "b"]));
        assert!(structured["_meta"].is_object());
    }

    fn server_with_tool(name: &str, handler: ToolHandler) -> McpServer {
        let mut s = McpServer::new("test", "0");
        s.register(Tool {
            name: name.into(),
            description: "t".into(),
            input_schema: json!({}),
            handler,
        });
        s
    }

    #[test]
    fn call_tool_execution_error_becomes_is_error_result() {
        // A handler returning Err is a tool *execution* failure: it must come back as a
        // normal result with isError:true, NOT as a JSON-RPC error (Err from call_tool).
        let s = server_with_tool(
            "relaywash__Boom",
            Box::new(|_, _| Err(anyhow!("kaboom detail"))),
        );
        let out = s
            .call_tool(&json!({"name": "relaywash__Boom"}))
            .expect("not a protocol error");
        assert_eq!(out["isError"], json!(true));
        assert_eq!(out["content"][0]["text"], json!("kaboom detail"));
    }

    #[test]
    fn call_tool_missing_name_is_protocol_error() {
        let s = server_with_tool(
            "relaywash__Ok",
            Box::new(|_, _| Ok(ToolResult::new("x", json!({})))),
        );
        assert!(s.call_tool(&json!({})).is_err());
    }

    #[test]
    fn call_tool_unknown_tool_is_protocol_error() {
        let s = server_with_tool(
            "relaywash__Ok",
            Box::new(|_, _| Ok(ToolResult::new("x", json!({})))),
        );
        assert!(s.call_tool(&json!({"name": "relaywash__Nope"})).is_err());
    }

    #[test]
    fn call_tool_success_returns_formatted_result() {
        let s = server_with_tool(
            "relaywash__Ok",
            Box::new(|_, _| Ok(ToolResult::new("relaywash__Ok", json!({"v": 1})))),
        );
        let out = s.call_tool(&json!({"name": "relaywash__Ok"})).unwrap();
        assert_eq!(out["structuredContent"]["v"], json!(1));
        assert!(out.get("isError").is_none());
    }

    #[test]
    fn formatter_passthrough_when_meta_absent() {
        let r = ToolResult::new("relaywash__Demo", json!({"ok": true}));
        let out = format_tool_result(&r);
        assert!(out["structuredContent"]["_meta"].is_null());
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
