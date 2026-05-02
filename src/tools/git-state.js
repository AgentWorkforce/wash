// relaywash__GitState — structured git status/diff/log/show.

import { spawnSync } from 'node:child_process';
import { meta } from '../burn/meta.js';

export const gitStateTool = {
  name: 'relaywash__GitState',
  description:
    'Structured git status/diff/log/show. Returns file lists + summary stats; per-file diffs are truncated. Use this instead of raw `git status`/`git diff`/`git log`/`git show` Bash calls.',
  inputSchema: {
    type: 'object',
    properties: {
      op: { type: 'string', enum: ['status', 'diff', 'log', 'show'] },
      paths: { type: 'array', items: { type: 'string' } },
      revision: { type: 'string' },
      base: { type: 'string' },
      maxFiles: { type: 'integer', default: 50 },
      maxLines: { type: 'integer', default: 200, description: 'Max diff lines per file.' },
      withBody: { type: 'boolean', default: false, description: '`log` only — include commit body.' },
      cwd: { type: 'string' },
    },
    required: ['op'],
    additionalProperties: false,
  },
  handler(args) {
    return runGitState(args || {});
  },
};

export function runGitState(args) {
  const op = args.op;
  const cwd = args.cwd || process.cwd();
  const maxFiles = args.maxFiles ?? 50;
  const maxLines = args.maxLines ?? 200;
  const replaces = [`Bash:git-${op}`];

  if (op === 'status') return { ...gitStatus({ cwd, paths: args.paths }), _meta: meta(replaces, 1) };
  if (op === 'log')
    return {
      ...gitLog({ cwd, paths: args.paths, revision: args.revision, withBody: args.withBody, maxFiles }),
      _meta: meta(replaces, 1),
    };
  if (op === 'diff' || op === 'show')
    return {
      ...gitDiff({ cwd, op, paths: args.paths, revision: args.revision, base: args.base, maxFiles, maxLines }),
      _meta: meta(replaces, 1),
    };
  throw new Error(`unknown op: ${op}`);
}

function git(cwd, args) {
  const r = spawnSync('git', args, { cwd, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  if (r.status !== 0) {
    throw new Error(`git ${args.join(' ')} failed: ${r.stderr || r.stdout || 'no output'}`);
  }
  return r.stdout;
}

function gitStatus({ cwd, paths }) {
  const branch = git(cwd, ['rev-parse', '--abbrev-ref', 'HEAD']).trim();
  let ahead = 0;
  let behind = 0;
  try {
    const upstream = git(cwd, ['rev-list', '--left-right', '--count', '@{u}...HEAD']).trim();
    const [b, a] = upstream.split(/\s+/).map(Number);
    behind = b || 0;
    ahead = a || 0;
  } catch {
    // No upstream — leave at 0.
  }
  const cmd = ['status', '--porcelain=v1'];
  if (paths && paths.length) cmd.push('--', ...paths);
  const raw = git(cwd, cmd);
  const files = raw
    .split('\n')
    .filter(Boolean)
    .map((line) => {
      const code = line.slice(0, 2);
      const path = line.slice(3);
      return { path, change: codeToChange(code) };
    });
  return { branch, ahead, behind, files };
}

function codeToChange(code) {
  const c = code.replace(' ', '');
  switch (c) {
    case 'M':
    case 'MM':
      return 'modified';
    case 'A':
      return 'added';
    case 'D':
      return 'deleted';
    case 'R':
      return 'renamed';
    case '??':
      return 'untracked';
    default:
      return code.trim();
  }
}

function gitLog({ cwd, paths, revision, withBody, maxFiles }) {
  // Use `--pretty=format:%H%x1f%an%x1f%ad%x1f%s%x1f%b%x1e` so we can parse robustly.
  const sep1 = '\x1f';
  const sep2 = '\x1e';
  const args = [
    'log',
    `--pretty=format:%H${sep1}%an${sep1}%ad${sep1}%s${sep1}%b${sep2}`,
    '--date=iso-strict',
    `-n`,
    String(maxFiles),
  ];
  if (revision) args.push(revision);
  if (paths && paths.length) args.push('--', ...paths);
  const raw = git(cwd, args);
  const commits = [];
  for (const block of raw.split(sep2)) {
    const trimmed = block.trim();
    if (!trimmed) continue;
    const [sha, author, date, subject, body] = trimmed.split(sep1);
    const c = { sha: sha?.slice(0, 12), author, date, subject };
    if (withBody && body) c.body = body.trim();
    commits.push(c);
  }
  return { commits };
}

function gitDiff({ cwd, op, paths, revision, base, maxFiles, maxLines }) {
  const baseArgs = op === 'show' ? ['show', '--stat=200', revision || 'HEAD'] : ['diff'];
  if (op === 'diff') {
    if (base && revision) baseArgs.push(`${base}..${revision}`);
    else if (revision) baseArgs.push(revision);
  }
  const stat = git(cwd, [...baseArgs, '--stat=200', ...(paths ? ['--', ...paths] : [])]);
  // Per-file diffs (truncated):
  const diffArgs = op === 'show' ? ['show', revision || 'HEAD', '--no-color'] : ['diff', '--no-color'];
  if (op === 'diff') {
    if (base && revision) diffArgs.push(`${base}..${revision}`);
    else if (revision) diffArgs.push(revision);
  }
  if (paths && paths.length) diffArgs.push('--', ...paths);
  const raw = git(cwd, diffArgs);
  const perFile = parsePerFileDiffs(raw, maxLines);
  const limited = perFile.slice(0, maxFiles);
  return {
    summary: stat.trim().split('\n').slice(-2).join('\n'), // "N files changed, …" tail
    files: limited,
    truncated: perFile.length > maxFiles,
  };
}

function parsePerFileDiffs(raw, maxLines) {
  const files = [];
  const blocks = raw.split(/^diff --git /gm).slice(1);
  for (const b of blocks) {
    const lines = b.split('\n');
    const header = lines[0];
    const m = /a\/(.+?) b\/(.+)/.exec(header);
    const path = m ? m[2] : header;
    let added = 0;
    let removed = 0;
    const hunkLines = [];
    let inHunk = false;
    for (const l of lines.slice(1)) {
      if (l.startsWith('@@')) {
        inHunk = true;
        hunkLines.push(l);
        continue;
      }
      if (!inHunk) continue;
      hunkLines.push(l);
      if (l.startsWith('+') && !l.startsWith('+++')) added++;
      else if (l.startsWith('-') && !l.startsWith('---')) removed++;
    }
    let body = hunkLines.join('\n');
    let truncated = false;
    if (hunkLines.length > maxLines) {
      const head = hunkLines.slice(0, Math.floor(maxLines / 2));
      const tail = hunkLines.slice(-Math.floor(maxLines / 2));
      body = `${head.join('\n')}\n... (${hunkLines.length - maxLines} lines truncated) ...\n${tail.join('\n')}`;
      truncated = true;
    }
    files.push({ path, added, removed, hunks: body, truncated });
  }
  return files;
}
