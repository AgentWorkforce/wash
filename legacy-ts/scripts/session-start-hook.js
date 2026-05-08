#!/usr/bin/env node
// SessionStart hook: verify Node >= 20.11 and warm up the relayburn ledger directory.

import { mkdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

const [major, minor] = process.versions.node.split('.').map(Number);
if (major < 20 || (major === 20 && minor < 11)) {
  process.stderr.write(
    `relaywash: Node ${process.versions.node} is too old. Need >= 20.11.\n`,
  );
  process.exit(2);
}

const ledgerHome = process.env.RELAYBURN_HOME || join(homedir(), '.relayburn');
try {
  mkdirSync(ledgerHome, { recursive: true });
} catch (e) {
  process.stderr.write(`relaywash: cannot create ledger dir ${ledgerHome}: ${e.message}\n`);
  process.exit(2);
}

process.stdout.write(JSON.stringify({ continue: true }) + '\n');
