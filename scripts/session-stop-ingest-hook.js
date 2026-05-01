#!/usr/bin/env node
// Stop hook: ingest the just-ended session into the local relayburn ledger.

import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { ingest } from '../src/burn/sdk.js';

let payload = {};
try {
  const raw = readFileSync(0, 'utf8');
  if (raw.trim()) payload = JSON.parse(raw);
} catch {}

const sessionId = payload.session_id || payload.sessionId || `session-${Date.now()}`;
const transcriptPath = payload.transcript_path || payload.transcriptPath || null;
const ledgerHome = process.env.RELAYBURN_HOME || join(homedir(), '.relayburn');

try {
  await ingest({ sessionId, transcriptPath, ledgerHome });
} catch (e) {
  // Don't fail the session on ingestion errors.
  process.stderr.write(`relaywash: ingest failed: ${e.message}\n`);
}

process.stdout.write(JSON.stringify({ continue: true }) + '\n');
