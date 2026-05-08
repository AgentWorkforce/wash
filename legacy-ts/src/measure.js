// `--measure` mode (Phase 6).
//
// When enabled (RELAYWASH_MEASURE=1), every replacement-tool call is paired with a "what would
// the built-in have returned" shadow read for offline comparison. Both responses are written to
// a JSONL log under `${RELAYBURN_HOME}/measure/`. Off by default; opt-in for benchmarking PRs.

import { spawnSync } from 'node:child_process';
import { mkdirSync, appendFileSync, readFileSync, existsSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';

export const measureEnabled = () => process.env.RELAYWASH_MEASURE === '1';

const home = () => process.env.RELAYBURN_HOME || join(homedir(), '.relayburn');
const measureDir = () => join(home(), 'measure');

export function recordCall(toolName, args, structured) {
  if (!measureEnabled()) return;
  try {
    mkdirSync(measureDir(), { recursive: true });
    const replacementSize = JSON.stringify(structured ?? {}).length;
    const shadow = shadowFor(toolName, args);
    const shadowSize = shadow ? shadow.length : 0;
    const ratio = shadowSize ? Math.round((replacementSize / shadowSize) * 1000) / 1000 : null;
    const line = JSON.stringify({
      ts: Date.now(),
      tool: toolName,
      replacementBytes: replacementSize,
      shadowBytes: shadowSize,
      ratio,
    });
    appendFileSync(join(measureDir(), 'compare.jsonl'), line + '\n');
  } catch {
    // Measurement must never fail the call.
  }
}

function shadowFor(toolName, args) {
  // For each replacement tool, run the closest vanilla equivalent and capture its raw output.
  switch (toolName) {
    case 'relaywash__Read':
      return shadowRead(args);
    case 'relaywash__Search':
      return shadowSearch(args);
    case 'relaywash__GitState':
      return shadowGit(args);
    default:
      return null;
  }
}

function shadowRead(args) {
  const path = args?.path;
  if (!path || !existsSync(path)) return null;
  const s = statSync(path);
  if (!s.isFile()) return null;
  return readFileSync(path, 'utf8');
}

function shadowSearch(args) {
  const re = args?.content || (args?.symbol ? `\\b${args.symbol}\\b` : null);
  if (!re) return null;
  const r = spawnSync('rg', ['-e', re, '--no-messages'], { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 });
  return r.stdout || '';
}

function shadowGit(args) {
  const op = args?.op;
  if (!op) return null;
  const cwd = args?.cwd || process.cwd();
  const r = spawnSync('git', [op], { cwd, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 });
  return r.stdout || '';
}
