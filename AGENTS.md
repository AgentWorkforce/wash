# Agent guide for relaywash

Conventions an agent or human needs to know to work productively in this repo.
Pairs with [`README.md`](./README.md): README is what relaywash does, this
file is how to work on it.

## Layout

The repo is Rust-first. `crates/wash/` is the source of truth for the MCP
server, hook handlers, and the command-line surface. The npm packages are
install wrappers around the Rust binary.

### Rust crate (`crates/wash/`)

`wash` is the only Rust crate in this workspace. It builds both the library and
the `wash` binary:

```
src/mcp/       - stdio MCP server and protocol framing.
src/tools/     - one module per `relaywash__*` replacement tool.
src/hooks/     - Claude Code hook handlers invoked by `wash hook <kind>`.
src/ast/       - tree-sitter signature extraction and parse checks.
src/profile.rs - adaptive default loader for Search and Read.
src/savings.rs - `/relaywash-savings` presenter over relayburn data.
```

Every new replacement tool should live under `src/tools/`, expose a stable MCP
schema, and return `_meta.replaces` / `_meta.collapsedCalls` where it replaces
vanilla tool sequences. Hook behavior belongs under `src/hooks/`; keep hook
failures best-effort unless blocking is the purpose of the hook.

### npm packages (`packages/` and root)

The root package is the published wrapper:

```
relaywash                     - root npm package; exposes `wash` and
                                `relaywash` through `bin/wash.mjs`.
@relaywash/wash-darwin-arm64  - prebuilt native binary package.
@relaywash/wash-darwin-x64    - prebuilt native binary package.
@relaywash/wash-linux-arm64   - prebuilt native binary package.
@relaywash/wash-linux-x64     - prebuilt native binary package.
@relaywash/wash-win32-x64     - prebuilt native binary package.
```

Do not add a standalone TypeScript implementation. If a feature is missing,
add it to the Rust crate and keep the Node launcher as a binary resolver.

### Plugin files

```
.claude-plugin/   - Claude plugin manifest and marketplace metadata.
.mcp.json         - plugin MCP launcher; calls `node bin/wash.mjs mcp`.
settings.json     - activates the `relaywash:code` agent.
agents/           - default coding agent and read-only exploration agent.
hooks/            - Claude hook wiring; handlers live in Rust.
```

## Common commands

```bash
cargo build --release       # Build the Rust binary.
cargo test --release        # Unit tests plus MCP stdio integration coverage.

node bin/wash.mjs --version # Smoke-test the launcher.
node scripts/copy-binary.mjs # Copy the local binary into the host platform package.
```

When debugging MCP behavior locally, prefer the Rust binary or launcher:

```bash
target/release/wash mcp
node bin/wash.mjs mcp
```

## Changelog

Curate `[Unreleased]` in `CHANGELOG.md` as you land PRs. `wash` keeps one
hand-written changelog at the repo root because the root `relaywash` package is
the user-facing wrapper and the platform packages are binary carriers only.

Changelog entries should be concise and impact-first. Prefer one short bullet
per user-visible change: name the command, tool, hook, schema, or package
touched and the practical effect. Drop issue/PR links, internal review notes,
implementation backstory, and "foundation for..." phrasing unless that text
clearly explains the shipped impact.

## Releases

```bash
# Tag-driven release:
#   push a v* tag, e.g. v0.1.0
#
# Manual release:
#   GitHub Actions -> release
#   version: exact version to publish, e.g. 0.1.0
```

The release workflow builds the Rust binary for every supported target,
publishes the five `@relaywash/wash-*` platform packages plus the `relaywash`
wrapper to npm at the same version, then creates one GitHub Release. Release
notes are generated from `CHANGELOG.md`; keep the target version block or
`[Unreleased]` ready before dispatching.

## When in doubt

- **Tool behavior:** start in `crates/wash/src/tools/mod.rs`, then the specific
  tool module.
- **MCP protocol:** read `crates/wash/src/mcp/server.rs`.
- **Claude hook behavior:** read `hooks/hooks.json` for wiring and
  `crates/wash/src/hooks/` for implementation.
- **Launcher/package behavior:** read `bin/wash.mjs`,
  `scripts/copy-binary.mjs`, and the package manifests under `packages/`.
- **Savings attribution:** read `crates/wash/src/meta.rs`,
  `crates/wash/src/hooks/session_stop.rs`, and `crates/wash/src/savings.rs`.
