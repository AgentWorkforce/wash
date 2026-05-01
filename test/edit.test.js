import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { mkdtempSync, writeFileSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { runEdit } from '../src/tools/edit.js';
import { fuzzyFindAll, normalizeForMatch } from '../src/fuzzy/index.js';

function tmpFile(content, ext = '.ts') {
  const dir = mkdtempSync(join(tmpdir(), 'relaywash-edit-'));
  const path = join(dir, `f${ext}`);
  writeFileSync(path, content);
  return path;
}

test('Edit: applies a single edit and writes verbatim newText', () => {
  const path = tmpFile('export const x = 1;\n');
  const r = runEdit({
    edits: [{ path, oldText: 'const x = 1', newText: 'const x = 42' }],
  });
  assert.equal(r.results[0].ok, true);
  assert.equal(readFileSync(path, 'utf8'), 'export const x = 42;\n');
});

test('Edit: batched edits across multiple files', () => {
  const a = tmpFile('a = 1');
  const b = tmpFile('b = 2');
  const r = runEdit({
    edits: [
      { path: a, oldText: 'a = 1', newText: 'a = 11' },
      { path: b, oldText: 'b = 2', newText: 'b = 22' },
    ],
  });
  assert.equal(r.results.filter((x) => x.ok).length, 2);
  assert.equal(readFileSync(a, 'utf8'), 'a = 11');
  assert.equal(readFileSync(b, 'utf8'), 'b = 22');
});

test('Edit: fuzzy match tolerates whitespace diffs', () => {
  // File uses tabs; old text uses spaces — most common cause of edit-retry loops in vanilla Edit.
  const path = tmpFile('export function foo(x) {\n\treturn x\n}\n');
  const r = runEdit({
    edits: [
      { path, oldText: 'export function foo(x) {\n    return x\n}', newText: 'export function foo(x) {\n  return x + 1\n}' },
    ],
  });
  assert.equal(r.results[0].ok, true, JSON.stringify(r.results[0]));
  assert.equal(readFileSync(path, 'utf8').includes('return x + 1'), true);
});

test('Edit: ambiguous match is rejected', () => {
  const path = tmpFile('foo();\nfoo();\n');
  const r = runEdit({
    edits: [{ path, oldText: 'foo();', newText: 'bar();' }],
  });
  assert.equal(r.results[0].ok, false);
  assert.match(r.results[0].reason, /ambiguous/);
});

test('Edit: rolls back when post-edit syntax check fails', () => {
  const before = 'export function foo() { return 1 }\n';
  const path = tmpFile(before);
  // Remove the closing brace — would break syntax.
  const r = runEdit({
    edits: [{ path, oldText: '{ return 1 }', newText: '{ return 1' }],
  });
  assert.equal(r.results[0].ok, false);
  assert.equal(readFileSync(path, 'utf8'), before);
});

test('fuzzyFindAll: handles smart quotes', () => {
  const haystack = 'const x = "hello"';
  const matches = fuzzyFindAll(haystack, 'const x = “hello”');
  assert.equal(matches.length, 1);
});

test('normalizeForMatch: collapses runs of whitespace', () => {
  assert.equal(normalizeForMatch('a  \t  b'), 'a b');
});
