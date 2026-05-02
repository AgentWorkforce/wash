# relaywash

**Clean agent tool output. Lower token burn.**

A Claude Code plugin that replaces the harness's built-in file and shell tools with structured
equivalents that return only what the model needs. The default `Read`/`Edit`/`Write`/`Grep`/
`Glob`/`NotebookEdit` tools dump full files, full directories, and full shell logs into the
model's context. relaywash returns ranked snippets, signature views, parsed errors, and
truncated diffs instead.

## Install

```
/plugin marketplace add AgentWorkforce/wash
/plugin install relaywash@agentworkforce
```

That's it. relaywash takes effect on the next session: the active agent has built-in file
tools disabled, and the MCP server exposes the structured replacements.

## What's in the box

| Tool | Replaces | Notes |
|------|----------|-------|
| `relaywash__Search` | Glob + Grep + Read | Ranked snippets across matched files. One call where vanilla takes ~9. |
| `relaywash__Read` | Read | AST-aware: signatures mode for known languages, mtime cache, range follow-ups. |
| `relaywash__Edit` | Edit | Batched multi-file edits with whitespace/Unicode fuzzy matching, atomic per file, tree-sitter post-check. |
| `relaywash__GitState` | git status/diff/log/show | Structured: file lists + summary stats; per-file diffs truncated. |
| `relaywash__TestRun` | pnpm test / pytest / jest / go test / cargo test | Counts + failed-test summaries. Use `getFailureLog` to drill into one failure. |
| `relaywash__Build` | tsc / cargo / go / pnpm build | One line on success; parsed `errors[]` on tsc/cargo/go failures; `errorTail` otherwise. |
| `relaywash__GhPR` | gh pr view/list/diff + gh api ...pulls... | Field selector; truncated bodies and hunks. |

Each result carries `_meta: { replaces, collapsedCalls }` so `relayburn` (the measurement
side of the project) can attribute savings.

## What's disabled and why

The active agent (`relaywash:code`) has these built-in tools blocked:

- `Read`, `Edit`, `Write`, `Grep`, `Glob`, `NotebookEdit`

A `PreToolUse` safety-net hook also blocks them at the harness level, so even sub-agents
or `/agents` switches can't fall through to vanilla `Read`. `Bash` is still available; a
warn-only hook nudges you toward the structured equivalent when a known shell pattern shows
up (e.g. `git status`, `pnpm test`).

## Measurement

relaywash bundles a stub of `relayburn/sdk` (the real package is blocked on
[AgentWorkforce/burn#218](https://github.com/AgentWorkforce/burn/issues/218)). On every
session end, the `Stop` hook ingests the session into the local ledger at
`~/.relayburn/` (override with `RELAYBURN_HOME`). To see your savings:

```
/relaywash-savings
```

For benchmark PRs, opt into the `--measure` shadow-read mode:

```
RELAYWASH_MEASURE=1 claude
```

Each replacement-tool call writes a line to `${RELAYBURN_HOME}/measure/compare.jsonl`
recording the replacement bytes vs the vanilla equivalent's bytes for offline analysis.

## Repository layout

```
.claude-plugin/        plugin.json + marketplace.json
.mcp.json              launches servers/relaywash-server.js
settings.json          activates the relaywash:code agent
agents/                code (default) + explore (cheap haiku read-only)
hooks/                 PreToolUse / PostToolUse / SessionStart / Stop
scripts/               hook scripts + /relaywash-savings + burn-compare
servers/               built MCP server entry
src/                   server source (plain ESM JS)
  tools/               one file per relaywash__ tool
  ast/                 tree-sitter wrappers (currently brace-balance fallback)
  fuzzy/               whitespace + Unicode normalization for Edit matching
  burn/                relayburn/sdk stub
  mcp/                 minimal MCP server over stdio
build/                 build.mjs (validates entry, stamps server file)
fixtures/corpus/       recorded sessions for burn-compare
test/                  node:test
docs/                  including compaction-attribution.md
```

## Hacking

Requires Node >= 20.11.

```
node build/build.mjs        # validate entry + write servers/relaywash-server.js
node --test test/           # run unit tests
node scripts/burn-compare.js   # compare replacement vs vanilla bytes on the fixture corpus
```

## License

MIT.
