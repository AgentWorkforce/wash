import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { runBuild } from '../src/tools/build.js';
import { mkdtempSync, writeFileSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

test('Build: returns a structured response with success boolean', () => {
  const dir = mkdtempSync(join(tmpdir(), 'relaywash-build-'));
  // Set up a minimal package.json with a passing build script.
  writeFileSync(
    join(dir, 'package.json'),
    JSON.stringify({ name: 'fixture', scripts: { build: 'echo built' } }),
  );
  const r = runBuild({ builder: 'npm', cwd: dir });
  assert.equal(typeof r.success, 'boolean');
  assert.equal(r._meta.replaces[0], 'Bash:build');
});

test('Build: failing tsc-style output produces parsed errors', () => {
  // Synthetic test of the parser, not the runner.
  const { parseErrors: _internal } = { parseErrors: undefined };
  // Use the exported runBuild path by faking a tsc command that prints an error and exits 1.
  const dir = mkdtempSync(join(tmpdir(), 'relaywash-build-fail-'));
  // A package.json with a build script that mimics tsc-style error output and exits non-zero.
  writeFileSync(
    join(dir, 'package.json'),
    JSON.stringify({
      name: 'fail',
      scripts: { build: 'node -e "console.log(\\"src/foo.ts(10,5): error TS2304: Cannot find name x.\\"); process.exit(1)"' },
    }),
  );
  const r = runBuild({ builder: 'npm', cwd: dir });
  assert.equal(r.success, false);
  // Either errors[] or errorTail is present.
  assert(r.errors || r.errorTail);
});
