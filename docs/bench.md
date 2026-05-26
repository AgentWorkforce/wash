# `wash bench` — evaluation harness

`wash bench` runs fixed benchmark tasks against fixture corpora and emits a
JSON suite report covering tool-call counts, response bytes, estimated tokens,
expectation pass/fail, cap hits, and repeated-call detection.

The harness drives the same handlers the MCP server registers, so any change
in `relaywash__Search` / `relaywash__Read` shows up in the numbers immediately.

## Quick start

```bash
# Run every task and print the JSON report to stdout.
wash bench run --label local

# CI subset: only tasks tagged "fastSubset": true. Target sub-30-seconds total.
wash bench run --fast --out target/bench/ci.json --label "$(git rev-parse --short HEAD)"

# Compare two saved runs and exit non-zero on regression.
wash bench compare --before baseline.json --after target/bench/ci.json
```

`wash bench run` exits non-zero when any task fails its expectations.
`wash bench compare` exits non-zero when any regression is detected (default
thresholds: 10 % bytes growth, 0 % call-count growth, any new failing
expectation, any new cap hit).

## Layout

```
fixtures/
  corpus/                      # raw source content, reused from earlier work
    find-and-read/
    large-codebase/
    multi-edit-refactor/
  bench/                       # per-task expectations files
    find-and-read/
      expectations.json
    large-codebase/
      expectations.json
    explore-subsystem/         # scaffold-only — implementation in follow-up
      expectations.json
      README.md
    multi-edit-refactor/       # scaffold-only
    diagnose-build-error/      # scaffold-only
    diagnose-failing-test/     # scaffold-only
    summarize-git-changes/     # scaffold-only
```

Fixture expectations are stored as data (JSON) rather than baked into the
runner so adding a task means writing one file, not touching Rust code.

## Expectations file shape

Each task has an `expectations.json` with this shape:

```json
{
  "name": "find-and-read",
  "description": "What this task is checking.",
  "corpus": "fixtures/corpus/find-and-read",
  "status": "implemented",
  "fastSubset": true,
  "steps": [
    { "name": "search-symbol", "tool": "Search", "args": { "symbol": "computeTotal" } },
    { "name": "read-target",   "tool": "Read",   "args": { "path": "src/billing.ts", "mode": "range", "range": [18, 40] } }
  ],
  "expectations": [
    { "kind": "file-in-top-results", "name": "billing-in-top", "step": "search-symbol", "files": ["src/billing.ts"], "topN": 3 },
    { "kind": "line-range-read",     "name": "body-in-range",  "step": "read-target",   "ranges": [[15, 45]] },
    { "kind": "min-hit-count",       "name": "≥1 hit",        "step": "search-symbol", "min": 1 },
    { "kind": "max-hit-count",       "name": "≤20 hits",      "step": "search-symbol", "max": 20 },
    { "kind": "max-total-bytes",     "name": "byte budget",    "max": 50000 }
  ]
}
```

The runner injects `cwd` (Search) or resolves `path` relative to the corpus
directory (Read) before calling the handler. Set `status: "scaffold"` to leave
a task in place without running it — useful for staking out directories that
will be filled in by follow-up PRs.

## Metrics collected

Per call:

- Tool name and args.
- `responseBytes` from `_meta`, with `responseTokens` derived through
  `crate::tokens::estimate_tokens` so estimates stay comparable across tools.
- `baselineBytes` from `_meta` when the tool reports a vanilla baseline.
- `capHit` (Search `truncated`, Search `skippedTruncated`, Read `truncated`).
- `error` for handler errors.

Per task:

- `callCount`, `totalBytes`, `totalTokens`, `anyCapHit`, `anyRepeatedCall`.
- `expectations[].passed` with a `detail` string on failure.

Per suite:

- Roll-up of every task's totals.
- `anyRepeatedCall` flips when any (tool, args) pair appears more than once
  across the whole suite.

## CI hook

The runner's `--fast` flag selects tasks tagged `"fastSubset": true` so PR
checks can stay quick. The intended flow:

```bash
# In a baseline workflow (e.g. on main), upload the report as an artifact:
wash bench run --fast --label "main@$(git rev-parse --short HEAD)" \
  --out target/bench/main.json

# In a PR workflow, download the artifact and compare:
wash bench run --fast --label "pr@$(git rev-parse --short HEAD)" \
  --out target/bench/pr.json
wash bench compare --before main.json --after target/bench/pr.json
```

Comments in `crates/wash/src/main.rs` (the `BenchCmd::Run` struct) document
this pattern at the source so it stays discoverable when the CI workflow is
added.

## Adding a task

1. Drop a directory under `fixtures/bench/<name>/`.
2. Write `expectations.json` with the shape above (point `corpus` at a
   `fixtures/corpus/` directory — reuse rather than duplicate sample files).
3. Add `--fast`-eligible tasks with `"fastSubset": true` only when they
   reliably finish under a second each.
4. Run `cargo test -p wash bench` to confirm the runner picks it up.

## Comparator thresholds

`wash bench compare` accepts `--max-bytes-growth` and `--max-calls-growth`
(both fractions, e.g. `0.10` for 10 %). The defaults are 10 % bytes growth
and 0 % call growth. New failing expectations and new cap hits are always
treated as regressions; flip them off in code if you ever need to.

## Scaffold tasks

The five scaffold-only tasks (`explore-subsystem`, `multi-edit-refactor`,
`diagnose-build-error`, `diagnose-failing-test`, `summarize-git-changes`) have
empty `steps`/`expectations` arrays and `status: "scaffold"`. They appear in
suite reports with a single passing `scaffold-only` expectation so it's
obvious from the report which tasks are pending implementation.
