#!/usr/bin/env node
// PreToolUse safety net: block built-in file tools and point the model at the relaywash equivalent.
// Runs even if the active agent's `disallowedTools` doesn't propagate (sub-agents, /agents switches).

import { readFileSync } from 'node:fs';

let payload = {};
try {
  const raw = readFileSync(0, 'utf8');
  if (raw.trim()) payload = JSON.parse(raw);
} catch {
  // No stdin or non-JSON; treat as no-op.
}

const tool = payload.tool_name || payload.toolName || '';

const redirects = {
  Read: 'relaywash__Read',
  Edit: 'relaywash__Edit',
  Write: 'relaywash__Edit',
  Grep: 'relaywash__Search',
  Glob: 'relaywash__Search',
  NotebookEdit: 'relaywash__Edit',
};

const replacement = redirects[tool];
if (replacement) {
  process.stdout.write(
    JSON.stringify({
      decision: 'block',
      reason: `relaywash: built-in ${tool} is disabled. Use ${replacement} instead.`,
    }) + '\n',
  );
  process.exit(0);
}

process.stdout.write(JSON.stringify({ continue: true }) + '\n');
