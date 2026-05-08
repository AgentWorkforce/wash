//! PreToolUse safety net: block built-in file tools and point the model at the relaywash
//! equivalent. Runs even if the active agent's `disallowedTools` doesn't propagate (sub-agents,
//! `/agents` switches).

use anyhow::Result;
use serde_json::{Value, json};
use std::io::Write;

use super::{write_continue, write_json};

pub fn run(payload: &Value, out: &mut impl Write) -> Result<()> {
    let tool = payload
        .get("tool_name")
        .or_else(|| payload.get("toolName"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let replacement = match tool {
        "Read" => "relaywash__Read",
        "Edit" => "relaywash__Edit",
        "Write" => "relaywash__Edit",
        "Grep" => "relaywash__Search",
        "Glob" => "relaywash__Search",
        "NotebookEdit" => "relaywash__Edit",
        _ => return write_continue(out),
    };
    write_json(
        out,
        &json!({
            "decision": "block",
            "reason": format!("relaywash: built-in {tool} is disabled. Use {replacement} instead."),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(payload: Value) -> String {
        let mut buf = Vec::new();
        run(&payload, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn blocks_read() {
        let s = drive(json!({"tool_name": "Read"}));
        assert!(s.contains("\"decision\":\"block\""));
        assert!(s.contains("relaywash__Read"));
    }

    #[test]
    fn blocks_glob_pointing_at_search() {
        let s = drive(json!({"tool_name": "Glob"}));
        assert!(s.contains("relaywash__Search"));
    }

    #[test]
    fn allows_unrelated_tool() {
        let s = drive(json!({"tool_name": "Bash"}));
        assert!(s.contains("\"continue\":true"));
        assert!(!s.contains("decision"));
    }

    #[test]
    fn handles_alternate_camelcase_key() {
        let s = drive(json!({"toolName": "Edit"}));
        assert!(s.contains("\"decision\":\"block\""));
        assert!(s.contains("relaywash__Edit"));
    }

    #[test]
    fn empty_payload_continues() {
        let s = drive(Value::Null);
        assert!(s.contains("\"continue\":true"));
    }
}
