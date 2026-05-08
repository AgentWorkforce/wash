#!/usr/bin/env node
// After `cargo build --release` (locally or in CI), copy the produced binary
// into the matching platform package's `bin/` so `npm pack` / `npm publish`
// picks it up.
//
// In CI this runs once per build matrix entry. Locally it copies into the
// host platform's package only — that's enough for `node bin/wash.mjs` to
// resolve via the platform-package path during dev testing.

import { existsSync, mkdirSync, copyFileSync, chmodSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, '..');

const platform = process.env.WASH_TARGET_PLATFORM ?? process.platform;
const arch = process.env.WASH_TARGET_ARCH ?? process.arch;
const isWindows = platform === 'win32';
const binName = isWindows ? 'wash.exe' : 'wash';

const sourceBin = process.env.WASH_BIN_PATH ?? join(repoRoot, 'target', 'release', binName);
const destDir = join(repoRoot, 'packages', `wash-${platform}-${arch}`, 'bin');
const destBin = join(destDir, binName);

if (!existsSync(sourceBin)) {
  console.error(`copy-binary: source not found at ${sourceBin}`);
  console.error(`Set WASH_BIN_PATH to override, or run 'cargo build --release' first.`);
  process.exit(1);
}

mkdirSync(destDir, { recursive: true });
copyFileSync(sourceBin, destBin);
if (!isWindows) {
  chmodSync(destBin, 0o755);
}
console.log(`copy-binary: ${sourceBin} -> ${destBin}`);
