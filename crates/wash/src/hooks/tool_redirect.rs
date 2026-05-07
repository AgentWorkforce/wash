//! PreToolUse on Bash: warn (do not block) when the model invokes a shell command that has
//! a structured relaywash replacement. Covers a focused whitelist — not exhaustive.

use anyhow::Result;
use regex::Regex;
use serde_json::{Value, json};
use std::io::Write;
use std::sync::OnceLock;

use super::{write_continue, write_json};

struct Pattern {
    re: &'static Regex,
    hint: &'static str,
}

fn patterns() -> &'static [Pattern] {
    static PS: OnceLock<Vec<Pattern>> = OnceLock::new();
    static REGEXES: OnceLock<Vec<Regex>> = OnceLock::new();
    let regexes = REGEXES.get_or_init(|| {
        vec![
            Regex::new(r"^(?:cat|bat|head|tail|less|more)\s+\S").unwrap(),
            Regex::new(r"^grep\b").unwrap(),
            Regex::new(r"^rg\b").unwrap(),
            Regex::new(r"^find\s+\S").unwrap(),
            Regex::new(r"^git\s+(?:status|diff|log|show)\b").unwrap(),
            Regex::new(r"^(?:pnpm|npm|yarn)\s+(?:run\s+)?test\b").unwrap(),
            Regex::new(r"^(?:pytest|jest|go\s+test|cargo\s+test)\b").unwrap(),
            Regex::new(r"^(?:pnpm|npm|yarn)\s+(?:run\s+)?build\b").unwrap(),
            Regex::new(r"^(?:tsc|cargo\s+build|go\s+build|vite\s+build|webpack)\b").unwrap(),
            Regex::new(r"^gh\s+pr\s+(?:view|list|diff)\b").unwrap(),
            Regex::new(r"^gh\s+api\s+repos/\S+/pulls\b").unwrap(),
        ]
    });
    PS.get_or_init(|| {
        let hints = [
            "relaywash__Read",
            "relaywash__Search",
            "relaywash__Search",
            "relaywash__Search",
            "relaywash__GitState",
            "relaywash__TestRun",
            "relaywash__TestRun",
            "relaywash__Build",
            "relaywash__Build",
            "relaywash__GhPR",
            "relaywash__GhPR",
        ];
        regexes
            .iter()
            .zip(hints.iter().copied())
            .map(|(re, hint)| Pattern { re, hint })
            .collect()
    })
}

pub fn run(payload: &Value, out: &mut impl Write) -> Result<()> {
    let cmd = payload
        .get("tool_input")
        .or_else(|| payload.get("toolInput"))
        .and_then(|v| v.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if cmd.is_empty() {
        return write_continue(out);
    }
    for p in patterns() {
        if p.re.is_match(cmd) {
            let hint = p.hint;
            let trunc: String = cmd.chars().take(80).collect();
            write_json(
                out,
                &json!({
                    "continue": true,
                    "systemMessage": format!(
                        "relaywash: `{trunc}` has a structured equivalent ({hint}). Consider using it next time for smaller responses.",
                    ),
                }),
            )?;
            return Ok(());
        }
    }
    write_continue(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(cmd: &str) -> String {
        let payload = json!({"tool_input": {"command": cmd}});
        let mut buf = Vec::new();
        run(&payload, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn nudges_git_status() {
        let s = drive("git status");
        assert!(s.contains("relaywash__GitState"));
        assert!(s.contains("\"continue\":true"));
    }

    #[test]
    fn nudges_pytest() {
        let s = drive("pytest tests/");
        assert!(s.contains("relaywash__TestRun"));
    }

    #[test]
    fn nudges_gh_pr_view() {
        let s = drive("gh pr view 42");
        assert!(s.contains("relaywash__GhPR"));
    }

    #[test]
    fn unrelated_command_silent() {
        let s = drive("echo hi");
        assert!(s.contains("\"continue\":true"));
        assert!(!s.contains("systemMessage"));
    }

    #[test]
    fn long_command_truncated_in_message() {
        let cmd = "git status ".to_string() + &"x".repeat(200);
        let s = drive(&cmd);
        assert!(s.contains("relaywash__GitState"));
        assert!(!s.contains(&"x".repeat(150)), "message must be truncated");
    }
}
