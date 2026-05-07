// relaywash__GhPR — structured PR access via the `gh` CLI.

import { spawnSync } from 'node:child_process';
import { meta } from '../burn/meta.js';

export const ghPrTool = {
  name: 'relaywash__GhPR',
  description:
    'Structured PR access (replaces gh pr view/list/diff and gh api repos/.../pulls). Returns a small subset of fields by default; use `fields` to expand. Bodies and diff hunks are truncated.',
  inputSchema: {
    type: 'object',
    properties: {
      op: { type: 'string', enum: ['view', 'list', 'diff', 'comments'] },
      number: { type: 'integer' },
      repo: { type: 'string' },
      fields: { type: 'array', items: { type: 'string' } },
      maxComments: { type: 'integer', default: 20 },
      maxDiffLines: { type: 'integer', default: 200 },
      cwd: { type: 'string' },
    },
    required: ['op'],
    additionalProperties: false,
  },
  handler(args) {
    return runGhPr(args || {});
  },
};

const VIEW_DEFAULT_FIELDS = ['number', 'title', 'state', 'author', 'headRefName', 'baseRefName', 'mergeable', 'isDraft'];
const LIST_FIELDS = ['number', 'title', 'state', 'author', 'updatedAt'];

export function runGhPr(args) {
  const cwd = args.cwd || process.cwd();
  const op = args.op;
  const replaces =
    op === 'view' ? ['Bash:gh-pr-view'] :
    op === 'list' ? ['Bash:gh-pr-list'] :
    op === 'diff' ? ['Bash:gh-pr-diff'] :
    op === 'comments' ? ['Bash:gh-api-pr-comments'] :
    [];

  if (op === 'view') return { ...ghView({ cwd, args }), _meta: meta(replaces, 1) };
  if (op === 'list') return { ...ghList({ cwd, args }), _meta: meta(replaces, 1) };
  if (op === 'diff') return { ...ghDiff({ cwd, args }), _meta: meta(replaces, 1) };
  if (op === 'comments') return { ...ghComments({ cwd, args }), _meta: meta(replaces, 1) };
  throw new Error(`unknown op: ${op}`);
}

function gh(cwd, args) {
  const r = spawnSync('gh', args, { cwd, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 });
  if (r.status !== 0) {
    throw new Error(`gh ${args.join(' ')} failed: ${r.stderr || r.stdout || 'no output'}`);
  }
  return r.stdout;
}

function ghView({ cwd, args }) {
  if (!args.number) throw new Error('GhPR view requires `number`');
  const fields = args.fields && args.fields.length ? args.fields : VIEW_DEFAULT_FIELDS;
  const cmd = ['pr', 'view', String(args.number), '--json', fields.join(',')];
  if (args.repo) cmd.push('--repo', args.repo);
  const json = JSON.parse(gh(cwd, cmd));
  // Normalise author to a string login.
  if (json.author && typeof json.author === 'object') json.author = json.author.login;
  // If `body` requested, truncate hard.
  if (typeof json.body === 'string' && json.body.length > 1500) {
    json.body = json.body.slice(0, 1500) + '\n... (truncated)';
  }
  return json;
}

function ghList({ cwd, args }) {
  const cmd = ['pr', 'list', '--json', LIST_FIELDS.join(','), '--limit', '30'];
  if (args.repo) cmd.push('--repo', args.repo);
  const out = JSON.parse(gh(cwd, cmd));
  return {
    pulls: out.map((p) => ({
      number: p.number,
      title: p.title,
      state: p.state,
      author: p.author?.login || p.author,
      updatedAt: p.updatedAt,
    })),
  };
}

function ghDiff({ cwd, args }) {
  if (!args.number) throw new Error('GhPR diff requires `number`');
  const cmd = ['pr', 'diff', String(args.number)];
  if (args.repo) cmd.push('--repo', args.repo);
  const raw = gh(cwd, cmd);
  const files = parseDiffPerFile(raw, args.maxDiffLines ?? 200);
  return { number: args.number, files, total: files.length };
}

function parseDiffPerFile(raw, maxLines) {
  const blocks = raw.split(/^diff --git /gm).slice(1);
  return blocks.map((b) => {
    const lines = b.split('\n');
    const m = /a\/(.+?) b\/(.+)/.exec(lines[0]);
    const path = m ? m[2] : lines[0];
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
    return { path, added, removed, hunks: body, truncated };
  });
}

function ghComments({ cwd, args }) {
  if (!args.number) throw new Error('GhPR comments requires `number`');
  // Use `gh api` to pull review comments + issue comments. Minimal field set.
  const repoSeg = args.repo ? args.repo : '';
  const baseUrl = repoSeg ? `repos/${repoSeg}/pulls/${args.number}` : `repos/{owner}/{repo}/pulls/${args.number}`;
  const reviewRaw = gh(cwd, ['api', `${baseUrl}/comments`]);
  const issueRaw = gh(cwd, ['api', `repos/${repoSeg || '{owner}/{repo}'}/issues/${args.number}/comments`]);
  const review = JSON.parse(reviewRaw);
  const issues = JSON.parse(issueRaw);
  const max = args.maxComments ?? 20;
  const trim = (s) => (s && s.length > 500 ? s.slice(0, 500) + '\n... (truncated)' : s);
  const comments = [
    ...issues.map((c) => ({
      author: c.user?.login,
      body: trim(c.body || ''),
      createdAt: c.created_at,
    })),
    ...review.map((c) => ({
      author: c.user?.login,
      body: trim(c.body || ''),
      createdAt: c.created_at,
      path: c.path,
      line: c.line || c.original_line,
    })),
  ];
  comments.sort((a, b) => (a.createdAt || '').localeCompare(b.createdAt || ''));
  return { number: args.number, comments: comments.slice(0, max), total: comments.length };
}
