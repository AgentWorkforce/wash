// "Build" step: bundle src/ into a single servers/relaywash-server.js by concatenating
// the entry import. We don't have a real bundler dep — node loads the source tree directly.
// This script's job is to (a) ensure the entry file is syntactically valid and (b) stamp
// servers/relaywash-server.js with a thin loader that re-exports src/index.js. That keeps
// .mcp.json's path stable while letting the source live under src/.

import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import { writeFileSync } from 'node:fs';

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = resolve(__dirname, '..');

// Sanity: load the entry to surface any syntax errors at build time.
await import(resolve(root, 'src/index.js'));

const loader = `#!/usr/bin/env node
// relaywash MCP server entry. Source lives under ../src/.
import('../src/index.js');
`;

writeFileSync(resolve(root, 'servers/relaywash-server.js'), loader);
console.log('Built servers/relaywash-server.js');
