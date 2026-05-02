// relaywash__Build — structured build output: one line on success; parsed errors on failure.

import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { meta } from '../burn/meta.js';

export const buildTool = {
  name: 'relaywash__Build',
  description:
    'Run the project build and return a tiny structured response. Successful builds return one line; failing tsc/cargo/go builds return parsed `errors[]`; other builders return an `errorTail`.',
  inputSchema: {
    type: 'object',
    properties: {
      builder: {
        type: 'string',
        enum: ['auto', 'pnpm', 'npm', 'yarn', 'tsc', 'cargo', 'go', 'vite', 'webpack'],
        default: 'auto',
      },
      target: { type: 'string' },
      errorTailLines: { type: 'integer', default: 50 },
      cwd: { type: 'string' },
    },
    additionalProperties: false,
  },
  handler(args) {
    return runBuild(args || {});
  },
};

const LOG_DIR = join(tmpdir(), 'relaywash-logs');

export function runBuild(args) {
  const cwd = args.cwd || process.cwd();
  const builder = args.builder === 'auto' || !args.builder ? detectBuilder(cwd) : args.builder;
  const cmd = buildCommand(builder, args);
  if (!cmd) {
    return {
      builder,
      success: false,
      duration: 0,
      errorTail: `no command for builder: ${builder}`,
      fullLogPath: null,
      _meta: meta(['Bash:build'], 1),
    };
  }
  const t0 = Date.now();
  const r = spawnSync(cmd[0], cmd.slice(1), {
    cwd,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    env: process.env,
  });
  const duration = Date.now() - t0;
  const raw = (r.stdout || '') + '\n' + (r.stderr || '');
  mkdirSync(LOG_DIR, { recursive: true });
  const fullLogPath = join(LOG_DIR, `build-${Date.now()}.log`);
  writeFileSync(fullLogPath, raw);

  const success = r.status === 0;
  if (success) {
    return { builder, success: true, duration, fullLogPath, _meta: meta(['Bash:build'], 1) };
  }
  const errors = parseErrors(builder, raw);
  if (errors.length) {
    return { builder, success: false, duration, errors, fullLogPath, _meta: meta(['Bash:build'], 1) };
  }
  const tailLines = args.errorTailLines ?? 50;
  const errorTail = raw.split('\n').slice(-tailLines).join('\n');
  return { builder, success: false, duration, errorTail, fullLogPath, _meta: meta(['Bash:build'], 1) };
}

function detectBuilder(cwd) {
  const has = (p) => existsSync(join(cwd, p));
  if (has('Cargo.toml')) return 'cargo';
  if (has('go.mod')) return 'go';
  if (has('tsconfig.json') && !has('package.json')) return 'tsc';
  if (has('pnpm-lock.yaml')) return 'pnpm';
  if (has('yarn.lock')) return 'yarn';
  if (has('package-lock.json') || has('package.json')) return 'npm';
  return 'tsc';
}

function buildCommand(builder, args) {
  const target = args.target;
  switch (builder) {
    case 'pnpm':
      return ['pnpm', 'build', ...(target ? [target] : [])];
    case 'npm':
      return ['npm', 'run', 'build'];
    case 'yarn':
      return ['yarn', 'build'];
    case 'tsc':
      return ['npx', 'tsc', ...(target ? ['-p', target] : [])];
    case 'cargo':
      return ['cargo', 'build'];
    case 'go':
      return ['go', 'build', target || './...'];
    case 'vite':
      return ['npx', 'vite', 'build'];
    case 'webpack':
      return ['npx', 'webpack', 'build'];
    default:
      return null;
  }
}

function parseErrors(builder, raw) {
  if (builder === 'tsc' || builder === 'pnpm' || builder === 'npm' || builder === 'yarn') {
    return parseTscErrors(raw);
  }
  if (builder === 'cargo') return parseCargoErrors(raw);
  if (builder === 'go') return parseGoErrors(raw);
  return [];
}

function parseTscErrors(raw) {
  const out = [];
  const re = /^(.+?)\((\d+),(\d+)\):\s*error\s+TS\d+:\s*(.+)$/gm;
  let m;
  while ((m = re.exec(raw))) {
    out.push({ file: m[1], line: Number(m[2]), col: Number(m[3]), message: m[4] });
  }
  // Also match the "file:line:col - error" style.
  const re2 = /^(.+?):(\d+):(\d+)\s*-\s*error\s+TS\d+:\s*(.+)$/gm;
  while ((m = re2.exec(raw))) {
    out.push({ file: m[1], line: Number(m[2]), col: Number(m[3]), message: m[4] });
  }
  return out;
}

function parseCargoErrors(raw) {
  const out = [];
  const re = /^error(?:\[E\d+\])?:\s*(.+?)\n\s+-->\s*(.+?):(\d+):(\d+)/gm;
  let m;
  while ((m = re.exec(raw))) {
    out.push({ file: m[2], line: Number(m[3]), col: Number(m[4]), message: m[1] });
  }
  return out;
}

function parseGoErrors(raw) {
  const out = [];
  const re = /^(.+?\.go):(\d+):(\d+):\s*(.+)$/gm;
  let m;
  while ((m = re.exec(raw))) {
    out.push({ file: m[1], line: Number(m[2]), col: Number(m[3]), message: m[4] });
  }
  return out;
}
