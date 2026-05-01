#!/usr/bin/env node
// `burn compare`-style script: walks the fixture corpus and reports the byte ratio between
// relaywash-tool responses and the vanilla equivalents (raw file content / shell output).
// Used by CI on every PR touching src/tools/.

import { readFileSync, readdirSync, statSync, existsSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { runSearch } from '../src/tools/search.js';
import { runRead, _resetReadCache } from '../src/tools/read.js';
import { spawnSync } from 'node:child_process';

const root = resolve(process.cwd());
const corpusDir = join(root, 'fixtures', 'corpus');
const out = { fixtures: [], totals: { replacementBytes: 0, vanillaBytes: 0 } };

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
  }
}

const ratio = out.totals.vanillaBytes
  ? Math.round((out.totals.replacementBytes / out.totals.vanillaBytes) * 1000) / 1000
  : null;

const summaryLines = [
  `relaywash burn-compare — ${out.fixtures.length} fixtures`,
  `replacement bytes: ${out.totals.replacementBytes}`,
  `vanilla bytes:     ${out.totals.vanillaBytes}`,
  `ratio:             ${ratio} (lower is better)`,
];
console.log(summaryLines.join('\n'));
console.log('');
console.log('per-fixture:');
for (const f of out.fixtures) {
  const r = f.vanillaBytes ? (f.replacementBytes / f.vanillaBytes).toFixed(3) : 'n/a';
  console.log(`  ${f.fixture}/${f.step}: ${f.replacementBytes}b vs ${f.vanillaBytes}b (ratio=${r})`);
}

// JSON output for CI consumption.
const jsonOut = process.argv[2];
if (jsonOut) {
  const { writeFileSync } = await import('node:fs');
  writeFileSync(jsonOut, JSON.stringify({ ...out, ratio }, null, 2));
}

function runStep(step, fixtureDir) {
  if (step.tool === 'Search') {
    const replacement = runSearch({ ...step.args, cwd: fixtureDir });
    const replacementBytes = JSON.stringify(replacement).length;
    const vanillaBytes = vanillaSearchBytes(step.args, fixtureDir);
    return { replacementBytes, vanillaBytes };
  }
  if (step.tool === 'Read') {
    const replacement = runRead({ ...step.args, path: join(fixtureDir, step.args.path) });
    const replacementBytes = JSON.stringify(replacement).length;
    const vanillaBytes = readFileSync(join(fixtureDir, step.args.path), 'utf8').length;
    return { replacementBytes, vanillaBytes };
  }
  return { replacementBytes: 0, vanillaBytes: 0 };
}

function vanillaSearchBytes(args, cwd) {
  const re = args.content || (args.symbol ? `\\b${args.symbol}\\b` : null);
  if (!re) return 0;
  const r = spawnSync(
    'rg',
    ['-e', re, '--no-messages', '--no-heading', '--with-filename', '--no-require-git', '.'],
    { cwd, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 },
  );
  return (r.stdout || '').length;
}
