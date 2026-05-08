#!/usr/bin/env node
// Launcher for the wash native binary.
//
// Resolves the platform-specific `@relaywash/wash-<platform>-<arch>` package via Node's
// own require resolution (so `npx -y relaywash …` and `npm install -g relaywash` both
// work) and execs the binary inside. Modeled after agent-relay's `broker-path.ts`.
//
// Lookup order:
//   1. RELAYWASH_BIN env var — explicit override (used in dev).
//   2. The platform package's `bin/wash` (or `wash.exe` on Windows), resolved via
//      `require.resolve` from this script's location AND from `process.argv[1]`
//      so bundled installs work too.
//   3. A locally-built `target/release/wash` two levels up — supports `node bin/wash.mjs`
//      from a checkout where the user has run `cargo build --release`.
//
// Exits with the binary's exit code, or 1 with a helpful message if no binary was found.

import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const __dirname = dirname(fileURLToPath(import.meta.url));
const isWindows = process.platform === 'win32';
const BIN_NAME = isWindows ? 'wash.exe' : 'wash';
const PKG_NAME = `@relaywash/wash-${process.platform}-${process.arch}`;

function envBinary() {
  const explicit = process.env.RELAYWASH_BIN;
  return explicit && existsSync(explicit) ? explicit : null;
}

function resolveFromPlatformPackage() {
  const candidates = uniq([
    join(__dirname, '..'),
    process.argv[1] ? dirname(process.argv[1]) : null,
  ].filter(Boolean));

  for (const fromDir of candidates) {
    const fromFile = join(fromDir, 'package.json');
    if (!existsSync(fromFile)) continue;
    try {
      const req = createRequire(fromFile);
      const manifest = req.resolve(`${PKG_NAME}/package.json`);
      const pkgRoot = dirname(manifest);
      const bin = join(pkgRoot, 'bin', BIN_NAME);
      if (existsSync(bin)) return bin;
    } catch {
      // Not installed at this resolution root — try the next.
    }
  }
  return null;
}

function resolveFromLocalCheckout() {
  // bin/wash.mjs lives at <repo>/bin/wash.mjs in a dev checkout.
  // Cargo writes <repo>/target/release/wash.
  const localBin = resolve(__dirname, '..', 'target', 'release', BIN_NAME);
  return existsSync(localBin) ? localBin : null;
}

function uniq(items) {
  return Array.from(new Set(items));
}

const binPath =
  envBinary() ?? resolveFromPlatformPackage() ?? resolveFromLocalCheckout();

if (!binPath) {
  process.stderr.write(
    [
      `relaywash: could not locate the wash native binary.`,
      `Looked for the platform package: ${PKG_NAME}`,
      `Tried (in order):`,
      `  $RELAYWASH_BIN env var`,
      `  ${PKG_NAME}/bin/${BIN_NAME} via npm resolution`,
      `  <repo>/target/release/${BIN_NAME} (dev checkout)`,
      ``,
      `If you installed via 'npm install -g relaywash' or 'npx relaywash',`,
      `please file an issue: https://github.com/AgentWorkforce/wash/issues`,
      `If you're hacking locally, run 'cargo build --release' first.`,
      ``,
    ].join('\n'),
  );
  process.exit(1);
}

const child = spawn(binPath, process.argv.slice(2), {
  stdio: 'inherit',
  windowsHide: true,
});
child.on('error', (err) => {
  process.stderr.write(`relaywash: failed to spawn ${binPath}: ${err.message}\n`);
  process.exit(1);
});
child.on('exit', (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 1);
});
