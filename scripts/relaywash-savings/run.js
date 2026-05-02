#!/usr/bin/env node
// /relaywash-savings: thin wrapper around relayburn/sdk `summary({ session })`.

import { summary } from '../../src/burn/sdk.js';

const session = process.argv[2] || process.env.CLAUDE_SESSION_ID || 'default';
const stats = await summary({ session });

const lines = [];
lines.push(`relaywash savings — session ${session}`);
lines.push('');
lines.push(`total tool calls: ${stats.totalCalls}`);
lines.push(`collapsed (built-in equivalents avoided): ${stats.collapsedCalls}`);
lines.push(`replaced built-ins: ${stats.replacedTools.join(', ') || '(none)'}`);
lines.push('');
lines.push('by tool:');
for (const [name, info] of Object.entries(stats.byTool).sort()) {
  lines.push(`  ${name.padEnd(28)} calls=${info.calls}  collapsed=${info.collapsedCalls}`);
}
process.stdout.write(lines.join('\n') + '\n');
