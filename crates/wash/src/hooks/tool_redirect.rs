//! PreToolUse on Bash: warn (do not block) when the model invokes a shell command that has
//! a structured relaywash replacement. Covers a focused whitelist — not exhaustive.

use anyhow::Result;
use regex::Regex;
use serde_json::{Value, json};
use std::io::Write;
use std::sync::OnceLock;

use super::{write_continue, write_json};

struct Pattern {
    re: Regex,
    hint: &'static str,
}

/// Single source of truth: each row pairs a regex source with its hint.
/// Adding a row is one line, and misalignment is no longer expressible.
const PATTERNS: &[(&str, &str)] = &[
    (
        r"^(?:cat|bat|head|tail|less|more)\s+\S",
        "relaywash__Read",
    ),
    (r"^grep\b", "relaywash__Search"),
    (r"^rg\b", "relaywash__Search"),
    (r"^find\s+\S", "relaywash__Search"),
    (
        r"^git\s+(?:status|diff|log|show)\b",
        "relaywash__GitState",
    ),
    (
        r"^(?:pnpm|npm|yarn)\s+(?:run\s+)?test\b",
        "relaywash__TestRun",
    ),
    (
        r"^(?:pytest|jest|go\s+test|cargo\s+test)\b",
        "relaywash__TestRun",
    ),
    (
        r"^(?:pnpm|npm|yarn)\s+(?:run\s+)?build\b",
        "relaywash__Build",
    ),
    (
        r"^(?:tsc|cargo\s+build|go\s+build|vite\s+build|webpack)\b",
        "relaywash__Build",
    ),
    (r"^gh\s+pr\s+(?:view|list|diff)\b", "relaywash__GhPR"),
    (r"^gh\s+api\s+repos/\S+/pulls\b", "relaywash__GhPR"),
];

fn patterns() -> &'static [Pattern] {
    static PS: OnceLock<Vec<Pattern>> = OnceLock::new();
    PS.get_or_init(|| {
        PATTERNS
            .iter()
            .map(|(src, hint)| Pattern {
                re: Regex::new(src).unwrap(),
                hint,
            })
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

    #[test]
    fn every_pattern_has_a_hint() {
        // Regression guard: with the single-table representation, every row
        // intrinsically carries both pieces. This asserts the built pattern
        // list matches the source table 1:1 and every hint is non-empty.
        let built = patterns();
        assert_eq!(built.len(), PATTERNS.len());
        for (p, (_src, hint)) in built.iter().zip(PATTERNS.iter()) {
            assert!(!p.hint.is_empty(), "hint must be non-empty");
            assert_eq!(p.hint, *hint);
        }
    }
}
