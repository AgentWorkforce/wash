// relaywash__TestRun — structured runner output: counts + failed test summaries.
// Streams raw output to a temp file; returns only the structured summary.

import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { meta } from '../burn/meta.js';

export const testRunTool = {
  name: 'relaywash__TestRun',
  description:
    'Run tests and return structured counts + failure summaries. Use `failuresOnly` (default true) to elide passing-test noise. Use `getFailureLog: <name>` to fetch the log slice for a single failure from a previous run.',
  inputSchema: {
    type: 'object',
    properties: {
      runner: {
        type: 'string',
        enum: ['auto', 'pnpm', 'npm', 'yarn', 'jest', 'pytest', 'go', 'cargo', 'node'],
        default: 'auto',
      },
      pattern: { type: 'string', description: 'Test name filter passed to the runner.' },
      paths: { type: 'array', items: { type: 'string' } },
      failuresOnly: { type: 'boolean', default: true },
      maxFailures: { type: 'integer', default: 10 },
      getFailureLog: {
        type: 'string',
        description: 'Fetch the log slice for one named failure (from a previous run).',
      },
      cwd: { type: 'string' },
    },
    additionalProperties: false,
  },
  handler(args) {
    return runTestRun(args || {});
  },
};

const LOG_DIR = join(tmpdir(), 'relaywash-logs');

export function runTestRun(args) {
  const cwd = args.cwd || process.cwd();
  if (args.getFailureLog) return fetchFailureSlice(args.getFailureLog, cwd);

  const runner = args.runner === 'auto' || !args.runner ? detectRunner(cwd) : args.runner;
  const cmd = buildCommand(runner, args);
  if (!cmd) {
    return {
      runner,
      passed: 0,
      failed: 0,
      skipped: 0,
      duration: 0,
      failures: [],
      fullLogPath: null,
      error: `no command for runner: ${runner}`,
      _meta: meta(['Bash:test'], 1),
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
  const raw = (r.stdout || '') + (r.stderr || '');
  mkdirSync(LOG_DIR, { recursive: true });
  const fullLogPath = join(LOG_DIR, `testrun-${Date.now()}.log`);
  writeFileSync(fullLogPath, raw);

  const parsed = parseRunnerOutput(runner, raw);
  const failures = parsed.failures.slice(0, args.maxFailures ?? 10);

  return {
    runner,
    passed: parsed.passed,
    failed: parsed.failed,
    skipped: parsed.skipped,
    duration,
    failures: args.failuresOnly === false ? parsed.failures : failures,
    fullLogPath,
    _meta: meta(['Bash:test'], 1),
  };
}

function detectRunner(cwd) {
  const has = (p) => existsSync(join(cwd, p));
  if (has('Cargo.toml')) return 'cargo';
  if (has('go.mod')) return 'go';
  if (has('pytest.ini') || has('pyproject.toml')) return 'pytest';
  if (has('jest.config.js') || has('jest.config.ts') || has('jest.config.cjs')) return 'jest';
  if (has('pnpm-lock.yaml')) return 'pnpm';
  if (has('yarn.lock')) return 'yarn';
  if (has('package-lock.json')) return 'npm';
  if (has('package.json')) return 'npm';
  return 'node';
}

function buildCommand(runner, args) {
  const pattern = args.pattern;
  const paths = args.paths || [];
  switch (runner) {
    case 'pnpm':
      return ['pnpm', 'test', ...(pattern ? ['--', '-t', pattern] : []), ...paths];
    case 'npm':
      return ['npm', 'test', '--', ...(pattern ? ['-t', pattern] : []), ...paths];
    case 'yarn':
      return ['yarn', 'test', ...(pattern ? ['-t', pattern] : []), ...paths];
    case 'jest':
      return ['npx', 'jest', ...(pattern ? ['-t', pattern] : []), ...paths];
    case 'pytest':
      return ['pytest', ...(pattern ? ['-k', pattern] : []), ...paths];
    case 'go':
      return ['go', 'test', './...', ...(pattern ? ['-run', pattern] : [])];
    case 'cargo':
      return ['cargo', 'test', ...(pattern ? [pattern] : [])];
    case 'node':
      return ['node', '--test', ...(paths.length ? paths : ['test/'])];
    default:
      return null;
  }
}

function parseRunnerOutput(runner, raw) {
  if (runner === 'pytest') return parsePytest(raw);
  if (runner === 'go') return parseGoTest(raw);
  if (runner === 'cargo') return parseCargoTest(raw);
  if (runner === 'node') return parseNodeTest(raw);
  return parseJest(raw); // jest / pnpm / npm / yarn typically render via jest or vitest
}

function parsePytest(raw) {
  const m = /=+\s*(\d+)\s+failed,?\s*(?:(\d+)\s+passed)?(?:.*?(\d+)\s+skipped)?/i.exec(raw) ||
    /=+\s*(\d+)\s+passed(?:.*?(\d+)\s+skipped)?/i.exec(raw);
  let passed = 0;
  let failed = 0;
  let skipped = 0;
  const m2 = /(\d+)\s+passed/.exec(raw);
  const m3 = /(\d+)\s+failed/.exec(raw);
  const m4 = /(\d+)\s+skipped/.exec(raw);
  if (m2) passed = Number(m2[1]);
  if (m3) failed = Number(m3[1]);
  if (m4) skipped = Number(m4[1]);
  const failures = [];
  const failBlock = /FAILED\s+(\S+)::(\S+)/g;
  let fm;
  while ((fm = failBlock.exec(raw))) {
    failures.push({ name: fm[2], file: fm[1], message: '' });
  }
  return { passed, failed, skipped, failures };
}

function parseGoTest(raw) {
  let passed = 0;
  let failed = 0;
  const skipped = 0;
  const failures = [];
  for (const line of raw.split('\n')) {
    if (/^---\s*PASS:\s/.test(line)) passed++;
    else if (/^---\s*FAIL:\s/.test(line)) {
      failed++;
      const m = /^---\s*FAIL:\s+(\S+)/.exec(line);
      if (m) failures.push({ name: m[1], file: '', message: '' });
    }
  }
  return { passed, failed, skipped, failures };
}

function parseCargoTest(raw) {
  const m = /test result: (?:ok|FAILED)\.\s+(\d+)\s+passed;\s+(\d+)\s+failed;\s+(\d+)\s+ignored/i.exec(
    raw,
  );
  const passed = m ? Number(m[1]) : 0;
  const failed = m ? Number(m[2]) : 0;
  const skipped = m ? Number(m[3]) : 0;
  const failures = [];
  const fm = /failures:\n\n((?:\s+.+\n)+)/.exec(raw);
  if (fm) {
    for (const line of fm[1].split('\n')) {
      const t = line.trim();
      if (t) failures.push({ name: t, file: '', message: '' });
    }
  }
  return { passed, failed, skipped, failures };
}

function parseNodeTest(raw) {
  let passed = 0;
  let failed = 0;
  let skipped = 0;
  const failures = [];
  const m = /# pass\s+(\d+)/.exec(raw);
  const f = /# fail\s+(\d+)/.exec(raw);
  const s = /# skipped\s+(\d+)/.exec(raw);
  if (m) passed = Number(m[1]);
  if (f) failed = Number(f[1]);
  if (s) skipped = Number(s[1]);
  // Parse "not ok N - name" lines.
  const re = /^not ok \d+ - (.+)$/gm;
  let fm;
  while ((fm = re.exec(raw))) {
    failures.push({ name: fm[1].trim(), file: '', message: '' });
  }
  return { passed, failed, skipped, failures };
}

function parseJest(raw) {
  let passed = 0;
  let failed = 0;
  let skipped = 0;
  const m = /Tests?:\s*(?:(\d+)\s+failed,\s*)?(?:(\d+)\s+skipped,\s*)?(\d+)\s+passed/.exec(raw);
  if (m) {
    failed = m[1] ? Number(m[1]) : 0;
    skipped = m[2] ? Number(m[2]) : 0;
    passed = Number(m[3] || 0);
  }
  const failures = [];
  const re = /●\s+(.+?)\n\n([\s\S]*?)(?=\n●|\nTest Suites:|\nTests:|$)/g;
  let fm;
  while ((fm = re.exec(raw))) {
    failures.push({ name: fm[1].trim(), file: '', message: fm[2].slice(0, 1000) });
  }
  return { passed, failed, skipped, failures };
}

function fetchFailureSlice(name, _cwd) {
  // Find the most recent log file and return the slice around the named failure.
  if (!existsSync(LOG_DIR)) return { found: false };
  const fs = readFileSync(join(LOG_DIR, latestLog())).toString();
  const idx = fs.indexOf(name);
  if (idx === -1) return { found: false };
  const start = Math.max(0, idx - 500);
  const end = Math.min(fs.length, idx + 2000);
  return { found: true, slice: fs.slice(start, end), _meta: meta(['Bash:test'], 1) };
}

function latestLog() {
  // Return the alphabetically last log filename — timestamps sort lexicographically.
  const files = readdirSync(LOG_DIR).filter((f) => f.endsWith('.log'));
  files.sort();
  return files[files.length - 1];
}
