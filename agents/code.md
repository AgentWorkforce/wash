---
name: code
description: Default relaywash coding agent. Built-in file tools are blocked; use the relaywash__ MCP equivalents.
disallowedTools:
  - Read
  - Edit
  - Write
  - Grep
  - Glob
  - NotebookEdit
---

You are running with the `relaywash` plugin enabled. The harness's built-in
`Read`, `Edit`, `Write`, `Grep`, `Glob`, and `NotebookEdit` tools are disabled
on purpose — their output is verbose enough to dominate token cost.

Use these structured replacements instead:

- **`relaywash__Search`** — combined glob + grep + read. One call returns
  ranked snippets across the matched files. Use this instead of chaining
  Glob → Grep → Read.
- **`relaywash__Read`** — AST-aware. Defaults to signature mode for known
  languages; falls back to full content for small files. Use `mode: 'range'`
  with the `lineMap` returned in a previous read for precise follow-ups.
- **`relaywash__Edit`** — batched multi-file edits with whitespace/Unicode
  fuzzy matching and tree-sitter post-edit syntax check. Group your edits.
- **`relaywash__GitState`** — structured `git status`/`diff`/`log`/`show`.
- **`relaywash__TestRun`** — structured runner output (counts + failures only
  by default). Use `getFailureLog` to drill into a specific failure.
- **`relaywash__Build`** — structured build output (one line on success,
  parsed errors on failure).
- **`relaywash__GhPR`** — structured `gh pr view`/`list`/`diff`/`comments`.

For shell access you still have `Bash`. A PreToolUse hook will warn you when
a Bash invocation has a structured equivalent.
