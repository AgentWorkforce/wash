//! Deterministic task categorizer for assistant turns.
//!
//! Given the set of tool names invoked in a turn (plus a small amount of
//! optional textual context — bash command strings, etc.), assign one of a fixed
//! set of categories. The rules are intentionally simple: tool-name based with a
//! handful of bash-command heuristics. Anything that doesn't match falls back to
//! `general` so the field is never blank.

/// Categories assigned to a single assistant turn.
///
/// String representation is stable on disk (JSONL) — do not rename without a
/// migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Exploration,
    Coding,
    Refactoring,
    Debugging,
    Testing,
    Build,
    Git,
    Planning,
    Delegation,
    Conversation,
    General,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Exploration => "exploration",
            Category::Coding => "coding",
            Category::Refactoring => "refactoring",
            Category::Debugging => "debugging",
            Category::Testing => "testing",
            Category::Build => "build",
            Category::Git => "git",
            Category::Planning => "planning",
            Category::Delegation => "delegation",
            Category::Conversation => "conversation",
            Category::General => "general",
        }
    }
}

/// Strip MCP namespace prefixes so callers can match on bare tool names.
fn canonical(tool: &str) -> &str {
    tool.strip_prefix("mcp__relaywash__")
        .or_else(|| tool.strip_prefix("relaywash__"))
        .or_else(|| tool.strip_prefix("mcp__github__"))
        .unwrap_or(tool)
}

/// Classify a single assistant turn.
///
/// `tools` is the list of tool_use block names from the turn. `bash_commands`
/// is the concatenated `command` strings from any Bash tool_use blocks — used to
/// distinguish e.g. `git ...` from `cargo test` when only `Bash` is observed.
pub fn classify(tools: &[String], bash_commands: &str) -> Category {
    if tools.is_empty() {
        return Category::Conversation;
    }

    // Track presence of category-defining tool families. Order of checks below
    // implements priority: testing > build > git > coding > delegation >
    // exploration > planning > general.
    let mut has_edit_write = false;
    let mut has_test = false;
    let mut has_build = false;
    let mut has_git_tool = false;
    let mut has_search_read_only = true;
    let mut has_agent = false;
    let mut has_planning = false;
    let mut has_any = false;

    for raw in tools {
        let t = canonical(raw);
        has_any = true;
        match t {
            "Edit" | "Write" | "NotebookEdit" | "MultiEdit" => {
                has_edit_write = true;
                has_search_read_only = false;
            }
            "TestRun" => {
                has_test = true;
                has_search_read_only = false;
            }
            "Build" => {
                has_build = true;
                has_search_read_only = false;
            }
            "GitState" => {
                has_git_tool = true;
                has_search_read_only = false;
            }
            "GhPR" => {
                // Treat GhPR as delegation-leaning (handing work off / reviewing
                // remote state) but still tool-call-driven, so not pure conversation.
                has_agent = true;
                has_search_read_only = false;
            }
            "Agent" | "Task" => {
                has_agent = true;
                has_search_read_only = false;
            }
            "ExitPlanMode" | "TodoWrite" => {
                has_planning = true;
                has_search_read_only = false;
            }
            "Read" | "Search" | "Grep" | "Glob" | "WebFetch" | "WebSearch" => {
                // Pure-read tools keep search_read_only=true.
            }
            "Bash" => {
                let lc = bash_commands.to_ascii_lowercase();
                if mentions_test(&lc) {
                    has_test = true;
                }
                if mentions_build(&lc) {
                    has_build = true;
                }
                if mentions_git(&lc) {
                    has_git_tool = true;
                }
                has_search_read_only = false;
            }
            _ => {
                // mcp__github__* and other namespaced tools — treat as exploration-ish
                // (don't force coding/refactoring) but break the read-only invariant
                // because they may mutate remote state.
                if raw.starts_with("mcp__github__") || raw.starts_with("mcp__") {
                    has_search_read_only = false;
                }
            }
        }
    }

    if !has_any {
        return Category::Conversation;
    }

    // Priority order — higher-signal categories first.
    if has_test {
        return Category::Testing;
    }
    if has_build {
        return Category::Build;
    }
    if has_git_tool {
        return Category::Git;
    }
    if has_edit_write {
        // Distinguish coding vs refactoring by bash hint — refactor often follows
        // a search/replace sweep across multiple files. Keep it simple: if the
        // turn also did a Search, lean "refactoring".
        let touched_search = tools.iter().any(|t| {
            let c = canonical(t);
            c == "Search" || c == "Grep" || c == "Glob"
        });
        if touched_search {
            return Category::Refactoring;
        }
        return Category::Coding;
    }
    if has_agent {
        return Category::Delegation;
    }
    if has_planning {
        return Category::Planning;
    }
    if has_search_read_only {
        return Category::Exploration;
    }
    Category::General
}

fn mentions_test(lc: &str) -> bool {
    lc.contains("cargo test")
        || lc.contains("npm test")
        || lc.contains("pnpm test")
        || lc.contains("yarn test")
        || lc.contains("pytest")
        || lc.contains("go test")
        || lc.contains("jest")
        || lc.contains("vitest")
}

fn mentions_build(lc: &str) -> bool {
    lc.contains("cargo build")
        || lc.contains("cargo check")
        || lc.contains("npm run build")
        || lc.contains("pnpm build")
        || lc.contains("yarn build")
        || lc.contains("tsc ")
        || lc.contains("make ")
        || lc.starts_with("make")
}

fn mentions_git(lc: &str) -> bool {
    // Match `git ` as a token, not e.g. `digit`.
    lc.starts_with("git ")
        || lc.contains(" git ")
        || lc.contains("&&git ")
        || lc.contains(";git ")
        || lc.contains("gh pr ")
        || lc.contains("gh issue ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn no_tools_is_conversation() {
        assert_eq!(classify(&[], ""), Category::Conversation);
    }

    #[test]
    fn edit_alone_is_coding() {
        assert_eq!(classify(&tools(&["Edit"]), ""), Category::Coding);
        assert_eq!(classify(&tools(&["Write"]), ""), Category::Coding);
    }

    #[test]
    fn edit_after_search_is_refactoring() {
        assert_eq!(
            classify(&tools(&["Search", "Edit", "Edit"]), ""),
            Category::Refactoring
        );
    }

    #[test]
    fn testrun_tool_wins_over_edit() {
        // Even with an Edit in the turn, a TestRun means the user was iterating
        // on tests — categorize as testing.
        assert_eq!(
            classify(&tools(&["Edit", "TestRun"]), ""),
            Category::Testing
        );
    }

    #[test]
    fn bash_cargo_test_classifies_as_testing() {
        assert_eq!(
            classify(&tools(&["Bash"]), "cargo test --all"),
            Category::Testing
        );
    }

    #[test]
    fn bash_cargo_build_classifies_as_build() {
        assert_eq!(
            classify(&tools(&["Bash"]), "cargo build --release"),
            Category::Build
        );
    }

    #[test]
    fn bash_git_classifies_as_git() {
        assert_eq!(
            classify(&tools(&["Bash"]), "git status && git diff"),
            Category::Git
        );
    }

    #[test]
    fn gitstate_tool_classifies_as_git() {
        assert_eq!(
            classify(&tools(&["mcp__relaywash__GitState"]), ""),
            Category::Git
        );
    }

    #[test]
    fn read_only_search_is_exploration() {
        assert_eq!(
            classify(&tools(&["Read", "Search", "Grep"]), ""),
            Category::Exploration
        );
    }

    #[test]
    fn agent_tool_is_delegation() {
        assert_eq!(classify(&tools(&["Task"]), ""), Category::Delegation);
        assert_eq!(classify(&tools(&["Agent"]), ""), Category::Delegation);
    }

    #[test]
    fn ghpr_routes_through_delegation() {
        assert_eq!(
            classify(&tools(&["mcp__relaywash__GhPR"]), ""),
            Category::Delegation
        );
    }

    #[test]
    fn todowrite_is_planning() {
        assert_eq!(classify(&tools(&["TodoWrite"]), ""), Category::Planning);
    }

    #[test]
    fn unknown_namespaced_tool_falls_through_to_general() {
        assert_eq!(
            classify(&tools(&["mcp__github__list_issues"]), ""),
            Category::General
        );
    }

    #[test]
    fn category_strings_are_stable() {
        // Disk format invariant — these strings appear in the JSONL ledger.
        assert_eq!(Category::Exploration.as_str(), "exploration");
        assert_eq!(Category::Coding.as_str(), "coding");
        assert_eq!(Category::Refactoring.as_str(), "refactoring");
        assert_eq!(Category::Debugging.as_str(), "debugging");
        assert_eq!(Category::Testing.as_str(), "testing");
        assert_eq!(Category::Build.as_str(), "build");
        assert_eq!(Category::Git.as_str(), "git");
        assert_eq!(Category::Planning.as_str(), "planning");
        assert_eq!(Category::Delegation.as_str(), "delegation");
        assert_eq!(Category::Conversation.as_str(), "conversation");
        assert_eq!(Category::General.as_str(), "general");
    }
}
