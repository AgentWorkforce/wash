# Changelog

Cross-package release notes for relaywash. Platform packages are versioned in
lockstep and do not carry separate narrative changelogs.

## [Unreleased]

### Fixed

- `relaywash__GhPR`: `comments` op resolves `owner/repo` from the git remote
  when the `repo` arg is omitted, replacing the broken literal-placeholder
  fallback that produced 404s.

## [0.1.0] - 2026-05-25

### Added

- `relaywash`: Claude Code plugin that disables vanilla file-edit/read/search
  tools for the active coding agent and exposes compact MCP replacements.
- `relaywash__Search`, `relaywash__Read`, `relaywash__Edit`,
  `relaywash__GitState`, `relaywash__TestRun`, `relaywash__Build`, and
  `relaywash__GhPR`: initial replacement tools for ranked search snippets,
  signature-focused reads, batched edits, structured git state, compact test
  and build failures, and trimmed GitHub PR data.
- `/relaywash-savings`: slash-command presenter over the local relayburn
  ledger for reporting collapsed-call savings.
- npm distribution through the `relaywash` wrapper and five prebuilt
  `@relaywash/wash-*` platform packages.

### Changed

- Measurement data now flows into the local relayburn ledger on session stop,
  so `/relaywash-savings` reports against the same ledger as `burn`.
- Replacement-tool savings annotations are emitted consistently across MCP
  responses.

### Fixed

- `relaywash__Search`: skips oversized and binary files, honors
  `maxFileBytes = 0`, and caps skipped-file reporting.
- `relaywash__Edit`: rolls back batched edits atomically, reports all sibling
  failures after rollback, preserves target file permissions, and propagates
  sync failures.
- MCP server tool failures now return `result.isError` instead of JSON-RPC
  transport errors.
- Session-stop savings ingest no longer fails under the synchronous
  `relayburn-sdk` ingest API.
