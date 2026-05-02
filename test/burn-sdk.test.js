import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Ledger, ingest, summary } from '../src/burn/sdk.js';

test('Ledger: round-trips tool_use events', async () => {
  const ledgerHome = mkdtempSync(join(tmpdir(), 'relayburn-'));
  const led = new Ledger({ ledgerHome });
  led.recordToolUse('s1', { tool: 'relaywash__Search', replaces: ['Glob', 'Grep'], collapsedCalls: 6 });
  led.recordToolUse('s1', { tool: 'relaywash__Read', replaces: ['Read'], collapsedCalls: 1 });
  await ingest({ sessionId: 's1', ledgerHome });
  const s = await summary({ session: 's1', ledgerHome });
  assert.equal(s.totalCalls, 2);
  assert.equal(s.collapsedCalls, 7);
  assert.deepEqual(s.replacedTools.sort(), ['Glob', 'Grep', 'Read']);
});

test('summary: empty session returns zeroes', async () => {
  const ledgerHome = mkdtempSync(join(tmpdir(), 'relayburn-empty-'));
  const s = await summary({ session: 'absent', ledgerHome });
  assert.equal(s.totalCalls, 0);
  assert.equal(s.collapsedCalls, 0);
});
