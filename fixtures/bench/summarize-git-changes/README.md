# summarize-git-changes (scaffold)

**Status:** scaffold-only. Implementation lands in a follow-up to issue #43.

**Intent:** drive `relaywash__GitState` (or the eventual git-summary tool) on a
captured `git diff` / `git log` payload and verify that:

- The summary mentions every modified file.
- Output bytes are bounded relative to the raw diff size (a soft budget
  encoded with `kind: "max-total-bytes"`).
- Adjacent hunks are merged rather than reported as duplicate noise.

Pre-recorded diff/log inputs will live alongside this file once the tool path
is finalized.
