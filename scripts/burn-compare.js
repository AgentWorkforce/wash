#!/usr/bin/env node
//
// burn-compare: walks the fixture corpus and reports the byte ratio between relaywash-tool
// responses and the vanilla equivalents.
//
// Manifest format (see fixtures/corpus/<name>/manifest.json):
//
//   { steps: [{ name, tool, args, vanilla?: [{ type, ...op }] }] }
//
// Each step represents ONE relaywash call. The `vanilla` array lists the sequence of
// vanilla Claude Code tool calls that step replaces — this matters because relaywash's
// biggest lever is collapsing 9 vanilla calls into 1 (see issue #1). When `vanilla` is
// omitted the script falls back to the 1:1 mapping used by older fixtures.
//
// Byte accounting:
//   replacementBytes = JSON.stringify(replaceResponse).length + PER_CALL_OVERHEAD
//   vanillaBytes     = sum(JSON-wrapped output of each vanilla op) + N * PER_CALL_OVERHEAD
//
// PER_CALL_OVERHEAD is a coarse model of the JSON-RPC + tool-result framing every call
// pays in real Claude Code sessions (see comment on PER_CALL_OVERHEAD below).

import { readFileSync, readdirSync, statSync, existsSync, writeFileSync } from 'node:fs';
import { join, resolve, dirname } from 'node:path';
import { spawnSync } from 'node:child_process';
import { runSearch } from '../src/tools/search.js';
import { runRead, _resetReadCache } from '../src/tools/read.js';

// Per-call framing model. Tool calls in MCP/Claude Code carry a per-call cost beyond the
// payload bytes: tool name, args echo, result wrapper, JSON-RPC framing, and (importantly)
// re-processing of all earlier tool results in subsequent turns. We model this as a flat
// overhead per call. ~120 bytes is conservative — real overhead is higher because the
// model re-reads earlier results on every subsequent turn, which we don't account for here.
const PER_CALL_OVERHEAD = 120;

const root = resolve(process.cwd());
const corpusDir = join(root, 'fixtures', 'corpus');
const out = { fixtures: [], totals: { replacementBytes: 0, vanillaBytes: 0, replacementCalls: 0, vanillaCalls: 0 } };

if (!existsSync(corpusDir)) {
  console.error('No fixture corpus at', corpusDir);
  process.exit(1);
}

for (const entry of readdirSync(corpusDir)) {
  const dir = join(corpusDir, entry);
  if (!statSync(dir).isDirectory()) continue;
  const manifestPath = join(dir, 'manifest.json');
  if (!existsSync(manifestPath)) continue;
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  for (const step of manifest.steps) {
    _resetReadCache();
    const r = runStep(step, dir);
    out.fixtures.push({ fixture: entry, step: step.name, ...r });
    out.totals.replacementBytes += r.replacementBytes;
    out.totals.vanillaBytes += r.vanillaBytes;
    out.totals.replacementCalls += r.replacementCalls;
    out.totals.vanillaCalls += r.vanillaCalls;
  }
}

const ratio = out.totals.vanillaBytes
  ? Math.round((out.totals.replacementBytes / out.totals.vanillaBytes) * 1000) / 1000
  : null;

console.log(`relaywash burn-compare — ${out.fixtures.length} fixtures`);
console.log(`replacement bytes: ${out.totals.replacementBytes} (${out.totals.replacementCalls} calls)`);
console.log(`vanilla bytes:     ${out.totals.vanillaBytes} (${out.totals.vanillaCalls} calls)`);
console.log(`call collapse:     ${out.totals.vanillaCalls}→${out.totals.replacementCalls} (${callRatio(out.totals)}x fewer)`);
console.log(`ratio:             ${ratio} (lower is better)`);
console.log('');
console.log('per-fixture:');
for (const f of out.fixtures) {
  const r = f.vanillaBytes ? (f.replacementBytes / f.vanillaBytes).toFixed(3) : 'n/a';
  console.log(
    `  ${f.fixture}/${f.step}: ${f.replacementBytes}b/${f.replacementCalls}call vs ${f.vanillaBytes}b/${f.vanillaCalls}call (ratio=${r})`,
  );
}

const jsonOut = process.argv[2];
if (jsonOut) {
  writeFileSync(jsonOut, JSON.stringify({ ...out, ratio }, null, 2));
}

function callRatio(t) {
  if (!t.replacementCalls) return 'n/a';
  return (t.vanillaCalls / t.replacementCalls).toFixed(1);
}

function runStep(step, fixtureDir) {
  // Replacement side: invoke the relaywash tool, JSON-encode the response, add per-call overhead.
  const replacementResponse = runReplacement(step, fixtureDir);
  const replacementBytes = JSON.stringify(replacementResponse).length + PER_CALL_OVERHEAD;
  const replacementCalls = 1;

  // Vanilla side: each entry in `step.vanilla` is one vanilla tool call.
  const vanillaOps = step.vanilla || legacyVanillaFor(step);
  let vanillaBytes = 0;
  for (const op of vanillaOps) {
    vanillaBytes += vanillaBytesFor(op, fixtureDir) + PER_CALL_OVERHEAD;
  }
  const vanillaCalls = vanillaOps.length;

  return { replacementBytes, vanillaBytes, replacementCalls, vanillaCalls };
}

function runReplacement(step, fixtureDir) {
  if (step.tool === 'Search') {
    return runSearch({ ...step.args, cwd: fixtureDir });
  }
  if (step.tool === 'Read') {
    return runRead({ ...step.args, path: join(fixtureDir, step.args.path) });
  }
  return {};
}

function legacyVanillaFor(step) {
  // Older manifests (without `vanilla:`) had a 1:1 mapping. Preserve their behavior.
  if (step.tool === 'Search') {
    const pattern = step.args.content || step.args.symbol;
    return pattern ? [{ type: 'grep', pattern }] : [];
  }
  if (step.tool === 'Read') {
    return [{ type: 'read', path: step.args.path }];
  }
  return [];
}

// Vanilla-op runners. Each returns the bytes that vanilla Claude Code would have received
// as the tool result (raw output), JSON-wrapped to mirror what the harness actually sends.

function vanillaBytesFor(op, fixtureDir) {
  switch (op.type) {
    case 'read':
      return wrapResult(readFile(join(fixtureDir, op.path))).length;
    case 'grep':
      return wrapResult(runGrep(op.pattern, fixtureDir)).length;
    case 'glob':
      return wrapResult(runGlob(op.pattern || '**/*', fixtureDir)).length;
    default:
      return 0;
  }
}

function wrapResult(raw) {
  // Mirror what Claude Code's harness would send back as a tool_result content block —
  // a single text block, JSON-encoded inside the response envelope. The `content` array +
  // type field add a few dozen bytes per call regardless of payload size.
  return JSON.stringify({ content: [{ type: 'text', text: raw }] });
}

function readFile(path) {
  try {
    return readFileSync(path, 'utf8');
  } catch {
    return '';
  }
}

function runGrep(pattern, cwd) {
  const r = spawnSync(
    'rg',
    ['-e', pattern, '--no-messages', '--no-heading', '--with-filename', '--no-require-git', '.'],
    { cwd, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 },
  );
  return r.stdout || '';
}

function runGlob(pattern, cwd) {
  // Vanilla Glob returns a newline-separated list of matched paths. Use rg --files +
  // --glob to mimic this without depending on a globbing CLI.
  const r = spawnSync('rg', ['--files', '--no-messages', '--no-require-git', '-g', pattern, '.'], {
    cwd,
    encoding: 'utf8',
    maxBuffer: 32 * 1024 * 1024,
  });
  return r.stdout || '';
}
