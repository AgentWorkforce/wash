# diagnose-failing-test (scaffold)

**Status:** scaffold-only. Implementation lands in a follow-up to issue #43.

**Intent:** feed captured `cargo test` / `npm test` output through
`relaywash__TestRun` (when wired in) and verify the agent gets back:

- The failing test name + file + line.
- The minimal diff between actual and expected output.
- A pointer to the assertion site, not the full test runner log.

Companion fixtures will live in this directory: `test.log`, optionally a
`failing.rs` sample, and the expected step list.
