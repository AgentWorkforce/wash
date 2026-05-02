#!/usr/bin/env node
// PreToolUse on Bash: warn (do not block) when the model invokes a shell command that has a
// structured relaywash replacement. Covers a focused whitelist — not exhaustive.

import { readFileSync } from 'node:fs';

let payload = {};
try {
  const raw = readFileSync(0, 'utf8');
  if (raw.trim()) payload = JSON.parse(raw);
} catch {
  process.stdout.write(JSON.stringify({ continue: true }) + '\n');
  process.exit(0);
}

const cmd = (payload.tool_input?.command || payload.toolInput?.command || '').trim();
if (!cmd) {
  process.stdout.write(JSON.stringify({ continue: true }) + '\n');
  process.exit(0);
}

// Pattern → suggested replacement. First match wins.
const PATTERNS = [
  { re: /^(?:cat|bat|head|tail|less|more)\s+\S/, hint: 'relaywash__Read' },
  { re: /^grep\b/, hint: 'relaywash__Search' },
  { re: /^rg\b/, hint: 'relaywash__Search' },
  { re: /^find\s+\S/, hint: 'relaywash__Search' },
  { re: /^git\s+(status|diff|log|show)\b/, hint: 'relaywash__GitState' },
  { re: /^(?:pnpm|npm|yarn)\s+(?:run\s+)?test\b/, hint: 'relaywash__TestRun' },
  { re: /^(?:pytest|jest|go\s+test|cargo\s+test)\b/, hint: 'relaywash__TestRun' },
  { re: /^(?:pnpm|npm|yarn)\s+(?:run\s+)?build\b/, hint: 'relaywash__Build' },
  { re: /^(?:tsc|cargo\s+build|go\s+build|vite\s+build|webpack)\b/, hint: 'relaywash__Build' },
  { re: /^gh\s+pr\s+(view|list|diff)\b/, hint: 'relaywash__GhPR' },
  { re: /^gh\s+api\s+repos\/[^\s]+\/pulls\b/, hint: 'relaywash__GhPR' },
];

for (const { re, hint } of PATTERNS) {
  if (re.test(cmd)) {
    // Warn-only: emit a system message via JSON, do not block the call.
    process.stdout.write(
      JSON.stringify({
        continue: true,
        systemMessage: `relaywash: \`${cmd.slice(0, 80)}\` has a structured equivalent (${hint}). Consider using it next time for smaller responses.`,
      }) + '\n',
    );
    process.exit(0);
  }
}

process.stdout.write(JSON.stringify({ continue: true }) + '\n');
