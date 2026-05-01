#!/usr/bin/env node
// PostToolUse on relaywash__Edit: count single-edit calls per session; nudge if >= 3 in 5 turns.
// State is kept in a tiny per-session JSON file under the relayburn home.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

const ledgerHome = process.env.RELAYBURN_HOME || join(homedir(), '.relayburn');
const stateDir = join(ledgerHome, 'edit-nudge');

let payload = {};
try {
  const raw = readFileSync(0, 'utf8');
  if (raw.trim()) payload = JSON.parse(raw);
} catch {
  process.exit(0);
}

const sessionId = payload.session_id || payload.sessionId || 'unknown';
const editCount = Array.isArray(payload.tool_input?.edits) ? payload.tool_input.edits.length : 1;

mkdirSync(stateDir, { recursive: true });
const file = join(stateDir, `${sessionId}.json`);

let state = { history: [] };
if (existsSync(file)) {
  try {
    state = JSON.parse(readFileSync(file, 'utf8'));
  } catch {}
}

state.history.push({ turn: state.history.length + 1, editCount });
state.history = state.history.slice(-5);

writeFileSync(file, JSON.stringify(state));

const recentSingles = state.history.filter((h) => h.editCount === 1).length;
if (recentSingles >= 3) {
  process.stdout.write(
    JSON.stringify({
      continue: true,
      systemMessage:
        'relaywash: 3+ single-edit calls in the last 5 turns. relaywash__Edit accepts an `edits[]` array — batch them next time for one round-trip.',
    }) + '\n',
  );
  process.exit(0);
}

process.stdout.write(JSON.stringify({ continue: true }) + '\n');
