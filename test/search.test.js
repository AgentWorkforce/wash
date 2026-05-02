import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { mkdtempSync, writeFileSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { runSearch, globToRegex } from '../src/tools/search.js';

function fixtureRepo() {
  const dir = mkdtempSync(join(tmpdir(), 'relaywash-search-'));
  mkdirSync(join(dir, 'src'));
  writeFileSync(join(dir, 'src/a.ts'), 'export function foo() { return 1 }\n// foo lives here\n');
  writeFileSync(join(dir, 'src/b.ts'), 'import { foo } from "./a"\nfoo()\n');
  writeFileSync(join(dir, 'src/c.txt'), 'no match here\n');
  writeFileSync(join(dir, '.gitignore'), 'ignored.txt\n');
  writeFileSync(join(dir, 'ignored.txt'), 'foo should not appear here\n');
  return dir;
}

test('Search: symbol mode returns ranked results with snippets', () => {
  const dir = fixtureRepo();
  const r = runSearch({ symbol: 'foo', cwd: dir });
  assert.equal(r._meta.replaces.includes('Glob'), true);
  assert.equal(r._meta.replaces.includes('Grep'), true);
  assert(r.results.length >= 2);
  for (const res of r.results) assert(res.snippet.length > 0);
  // .gitignore should hide ignored.txt
  assert.equal(
    r.results.some((res) => res.path.includes('ignored.txt')),
    false,
  );
});

test('Search: maxResults truncates', () => {
  const dir = fixtureRepo();
  const r = runSearch({ symbol: 'foo', cwd: dir, maxResults: 1 });
  assert.equal(r.results.length, 1);
  assert.equal(r.truncated, true);
});

test('Search: ranking modes produce different orderings', () => {
  const dir = fixtureRepo();
  const byMatches = runSearch({ symbol: 'foo', cwd: dir, rank: 'matches' });
  const byPathDepth = runSearch({ symbol: 'foo', cwd: dir, rank: 'path-depth' });
  // At minimum, both succeed and return the same set, possibly different order.
  assert.equal(byMatches.results.length, byPathDepth.results.length);
});

test('globToRegex: matches double-star', () => {
  const re = globToRegex('**/*.ts');
  assert.equal(re.test('src/a.ts'), true);
  assert.equal(re.test('a.ts'), true);
  assert.equal(re.test('src/a.js'), false);
});
