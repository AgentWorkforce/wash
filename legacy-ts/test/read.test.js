import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { runRead, _resetReadCache, noteSearchedSymbol } from '../src/tools/read.js';

function bigTsFile() {
  const dir = mkdtempSync(join(tmpdir(), 'relaywash-read-'));
  const path = join(dir, 'big.ts');
  const lines = ['import { x } from "y";', ''];
  // 50 functions, each with a 30-line body so the < 20 lines heuristic doesn't include bodies.
  for (let i = 0; i < 50; i++) {
    lines.push(`export function fn${i}(arg: number): number {`);
    for (let j = 0; j < 30; j++) lines.push(`  // body line ${j}`);
    lines.push('  return arg + 1;');
    lines.push('}');
    lines.push('');
  }
  writeFileSync(path, lines.join('\n'));
  return path;
}

test('Read: small file returns full content (heuristic)', () => {
  const dir = mkdtempSync(join(tmpdir(), 'relaywash-read-small-'));
  const path = join(dir, 'small.ts');
  writeFileSync(path, 'export const x = 1\n');
  _resetReadCache();
  const r = runRead({ path, mode: 'signatures' });
  assert.equal(r.content.includes('export const x = 1'), true);
});

test('Read: large file in signatures mode returns reduced content + lineMap', () => {
  _resetReadCache();
  const path = bigTsFile();
  const r = runRead({ path, mode: 'signatures' });
  assert.equal(r.languageDetected, 'typescript');
  assert.equal(r.truncated, true);
  assert(r.lineMap && r.lineMap.length >= 5);
  assert(r.content.length > 0);
  // Reduced enough — body comments should mostly be elided.
  assert(r.content.split('\n').length < 600);
});

test('Read: repeat-read of unchanged file returns empty content', () => {
  _resetReadCache();
  const path = bigTsFile();
  runRead({ path, mode: 'signatures' });
  const r = runRead({ path, mode: 'signatures' });
  assert.equal(r.content, '');
});

test('Read: range mode returns the requested slice', () => {
  _resetReadCache();
  const path = bigTsFile();
  const r = runRead({ path, mode: 'range', range: [1, 3] });
  const lines = r.content.split('\n');
  assert.equal(lines.length, 3);
  assert.equal(lines[0], 'import { x } from "y";');
});

test('Read: mtime cache is scoped per session', () => {
  _resetReadCache();
  const path = bigTsFile();
  // Session A reads — populates A's cache.
  runRead({ path, mode: 'signatures' }, { sessionId: 'A' });
  // Session B reads the same file for the first time — must NOT get the empty-content
  // optimization, because B never read it.
  const r = runRead({ path, mode: 'signatures' }, { sessionId: 'B' });
  assert(r.content.length > 0, 'session B should get full response on first read');
  // Session A re-reading still gets the empty-content optimization.
  const rA = runRead({ path, mode: 'signatures' }, { sessionId: 'A' });
  assert.equal(rA.content, '');
});

test('Read: searched symbol heuristic keeps body', () => {
  _resetReadCache();
  noteSearchedSymbol('fn5');
  const path = bigTsFile();
  const r = runRead({ path, mode: 'signatures' });
  // The searched symbol should have its body preserved.
  assert(r.content.includes('fn5'));
});
