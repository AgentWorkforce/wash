// relaywash__Search — collapses Glob + Grep + Read into one ranked-snippet response.
//
// Backend: prefer `rg` (ripgrep) when present, otherwise a JS scanner. Both honour `.gitignore`
// transitively (rg natively; the JS fallback consults a small built-in matcher).

import { spawnSync } from 'node:child_process';
import { readFileSync, statSync, readdirSync, existsSync } from 'node:fs';
import { join, relative, resolve, dirname } from 'node:path';
import { meta } from '../burn/meta.js';

export const searchTool = {
  name: 'relaywash__Search',
  description:
    'Combined glob + grep + read. Returns ranked snippets across matched files. Use this instead of chaining Glob → Grep → Read. Always returns snippets only, never full file contents.',
  inputSchema: {
    type: 'object',
    properties: {
      paths: {
        type: 'array',
        items: { type: 'string' },
        description: 'Glob patterns. Default: ["**/*"] minus .gitignore.',
      },
      content: { type: 'string', description: 'Regex to match in file contents.' },
      symbol: {
        type: 'string',
        description: 'Identifier to find (word-boundary search). Use this OR `content`.',
      },
      maxResults: { type: 'integer', minimum: 1, default: 50 },
      contextLines: { type: 'integer', minimum: 0, default: 2 },
      rank: { type: 'string', enum: ['matches', 'mtime', 'path-depth'], default: 'matches' },
      cwd: { type: 'string', description: 'Search root. Defaults to process.cwd().' },
    },
    additionalProperties: false,
  },
  handler(args) {
    return runSearch(args || {});
  },
};

export function runSearch(args) {
  const cwd = args.cwd || process.cwd();
  const maxResults = args.maxResults ?? 50;
  const contextLines = args.contextLines ?? 2;
  const rank = args.rank || 'matches';
  const paths = (args.paths && args.paths.length ? args.paths : ['**/*']).slice();

  let pattern = args.content;
  let isSymbolSearch = false;
  if (!pattern && args.symbol) {
    // Word-boundary identifier search.
    pattern = `\\b${escapeRegex(args.symbol)}\\b`;
    isSymbolSearch = true;
  }

  const fileMatches = pattern
    ? scanContent({ cwd, pattern, paths, contextLines })
    : scanGlobOnly({ cwd, paths });

  const ranked = rankResults(fileMatches, rank, cwd);
  const truncated = ranked.length > maxResults;
  const results = ranked.slice(0, maxResults);

  // _meta.replaces depends on whether we returned snippets (Read collapsed in too) or just paths.
  const replaces = pattern
    ? results.some((r) => r.snippet)
      ? ['Glob', 'Grep', 'Read']
      : ['Glob', 'Grep']
    : ['Glob'];

  return {
    results,
    truncated,
    _meta: meta(replaces, Math.max(1, results.length * (replaces.length))),
  };
}

function escapeRegex(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function scanContent({ cwd, pattern, paths, contextLines }) {
  const rg = which('rg');
  if (rg) return scanWithRipgrep({ cwd, pattern, paths, contextLines });
  return scanWithJs({ cwd, pattern, paths, contextLines });
}

function which(bin) {
  const r = spawnSync(process.platform === 'win32' ? 'where' : 'which', [bin], {
    encoding: 'utf8',
  });
  return r.status === 0 ? r.stdout.trim().split('\n')[0] : null;
}

function scanWithRipgrep({ cwd, pattern, paths, contextLines }) {
  const args = [
    '--json',
    '--no-messages',
    '--no-require-git', // honor .gitignore even when there's no .git directory.
    '-C',
    String(contextLines),
    '-e',
    pattern,
  ];
  for (const p of paths) {
    if (p === '**/*' || p === '**') continue;
    args.push('-g', p);
  }
  // Explicit search root — rg won't search cwd reliably when invoked without a TTY.
  args.push('.');
  const res = spawnSync('rg', args, { cwd, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  if (res.status !== 0 && res.status !== 1) {
    // 1 = no matches; >=2 = error. Fall back to JS scanner on error.
    return scanWithJs({ cwd, pattern, paths, contextLines });
  }
  return parseRipgrepJson(res.stdout, contextLines);
}

function parseRipgrepJson(stdout, contextLines) {
  /** @type {Map<string, { lines: Map<number, string>; matchLines: Set<number> }>} */
  const byFile = new Map();
  for (const raw of stdout.split('\n')) {
    if (!raw) continue;
    let evt;
    try {
      evt = JSON.parse(raw);
    } catch {
      continue;
    }
    if (evt.type !== 'match' && evt.type !== 'context') continue;
    const path = evt.data?.path?.text;
    if (!path) continue;
    if (!byFile.has(path)) byFile.set(path, { lines: new Map(), matchLines: new Set() });
    const entry = byFile.get(path);
    const lineNumber = evt.data.line_number;
    const text = (evt.data.lines?.text || '').replace(/\n$/, '');
    entry.lines.set(lineNumber, text);
    if (evt.type === 'match') entry.matchLines.add(lineNumber);
  }

  const results = [];
  for (const [path, { lines, matchLines }] of byFile) {
    // Group consecutive (or near-consecutive within contextLines) lines into snippets.
    const sortedNums = [...lines.keys()].sort((a, b) => a - b);
    let group = [];
    const flush = () => {
      if (!group.length) return;
      const start = group[0];
      const end = group[group.length - 1];
      const snippet = group
        .map((n) => `${n.toString().padStart(4, ' ')}  ${lines.get(n)}`)
        .join('\n');
      const matchCount = group.filter((n) => matchLines.has(n)).length;
      if (matchCount > 0) {
        results.push({ path, lineStart: start, lineEnd: end, snippet, matchCount });
      }
    };
    for (const n of sortedNums) {
      if (!group.length || n - group[group.length - 1] <= contextLines + 1) {
        group.push(n);
      } else {
        flush();
        group = [n];
      }
    }
    flush();
  }
  return results;
}

function scanWithJs({ cwd, pattern, paths, contextLines }) {
  const re = new RegExp(pattern, 'g');
  const files = listFiles(cwd, paths);
  const out = [];
  for (const file of files) {
    let text;
    try {
      text = readFileSync(file, 'utf8');
    } catch {
      continue;
    }
    if (!re.test(text)) {
      re.lastIndex = 0;
      continue;
    }
    re.lastIndex = 0;
    const lines = text.split('\n');
    /** @type {Set<number>} */
    const matchLines = new Set();
    for (let i = 0; i < lines.length; i++) {
      const local = new RegExp(pattern, 'g');
      if (local.test(lines[i])) matchLines.add(i + 1);
    }
    if (!matchLines.size) continue;
    const sorted = [...matchLines].sort((a, b) => a - b);
    let group = null;
    const flushGroup = () => {
      if (!group) return;
      const start = Math.max(1, group.first - contextLines);
      const end = Math.min(lines.length, group.last + contextLines);
      const snippetLines = [];
      for (let n = start; n <= end; n++) {
        snippetLines.push(`${String(n).padStart(4, ' ')}  ${lines[n - 1]}`);
      }
      out.push({
        path: relative(cwd, file),
        lineStart: start,
        lineEnd: end,
        snippet: snippetLines.join('\n'),
        matchCount: group.count,
      });
      group = null;
    };
    for (const n of sorted) {
      if (group && n - group.last <= contextLines * 2 + 1) {
        group.last = n;
        group.count++;
      } else {
        flushGroup();
        group = { first: n, last: n, count: 1 };
      }
    }
    flushGroup();
  }
  return out;
}

function scanGlobOnly({ cwd, paths }) {
  const files = listFiles(cwd, paths);
  return files.map((f) => ({
    path: relative(cwd, f),
    lineStart: 0,
    lineEnd: 0,
    snippet: '',
    matchCount: 0,
  }));
}

// ---- Path/glob walking with .gitignore support (minimal) ----

function listFiles(cwd, patterns) {
  const ignore = loadGitignore(cwd);
  /** @type {string[]} */
  const out = [];
  walk(cwd, cwd, ignore, out);
  if (patterns.length === 1 && (patterns[0] === '**/*' || patterns[0] === '**')) return out;
  const regexes = patterns.map(globToRegex);
  return out.filter((abs) => {
    const rel = relative(cwd, abs).split(/[\\/]/).join('/');
    return regexes.some((re) => re.test(rel));
  });
}

function walk(root, dir, ignore, out) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const e of entries) {
    if (e.name === '.git' || e.name === 'node_modules') continue;
    const abs = join(dir, e.name);
    const rel = relative(root, abs).split(/[\\/]/).join('/');
    if (ignore(rel, e.isDirectory())) continue;
    if (e.isDirectory()) walk(root, abs, ignore, out);
    else if (e.isFile()) out.push(abs);
  }
}

function loadGitignore(cwd) {
  const patterns = [];
  let cur = cwd;
  // Walk up looking for .gitignore files (just current dir for simplicity).
  const f = join(cur, '.gitignore');
  if (existsSync(f)) {
    for (const line of readFileSync(f, 'utf8').split('\n')) {
      const trimmed = line.trim();
      if (!trimmed || trimmed.startsWith('#')) continue;
      patterns.push(trimmed);
    }
  }
  const compiled = patterns.map((p) => ({
    negate: p.startsWith('!'),
    re: globToRegex(p.replace(/^!/, '').replace(/\/$/, '')),
    dirOnly: p.endsWith('/'),
  }));
  return (rel, isDir) => {
    let ignored = false;
    for (const { negate, re, dirOnly } of compiled) {
      if (dirOnly && !isDir) continue;
      if (re.test(rel)) ignored = !negate;
    }
    return ignored;
  };
}

export function globToRegex(glob) {
  // Convert a gitignore/glob pattern to a regex matching forward-slash relative paths.
  let g = glob;
  if (g.startsWith('/')) g = g.slice(1);
  // Escape regex metacharacters except for glob ones.
  let re = '';
  let i = 0;
  while (i < g.length) {
    const c = g[i];
    if (c === '*') {
      if (g[i + 1] === '*') {
        // ** — match anything including /
        re += '.*';
        i += 2;
        if (g[i] === '/') i++;
        continue;
      }
      re += '[^/]*';
      i++;
      continue;
    }
    if (c === '?') {
      re += '[^/]';
      i++;
      continue;
    }
    if ('.+^${}()|[]\\'.includes(c)) {
      re += '\\' + c;
      i++;
      continue;
    }
    re += c;
    i++;
  }
  return new RegExp('^(?:.*/)?' + re + '(?:/.*)?$');
}

function rankResults(results, mode, cwd) {
  if (mode === 'matches') {
    return [...results].sort((a, b) => b.matchCount - a.matchCount || a.path.localeCompare(b.path));
  }
  if (mode === 'mtime') {
    const withMtime = results.map((r) => {
      let mtime = 0;
      try {
        // r.path is relative to the search cwd; resolve against cwd, not process.cwd().
        mtime = statSync(resolve(cwd || process.cwd(), r.path)).mtimeMs;
      } catch {}
      return { r, mtime };
    });
    return withMtime.sort((a, b) => b.mtime - a.mtime).map((x) => x.r);
  }
  if (mode === 'path-depth') {
    return [...results].sort(
      (a, b) => a.path.split('/').length - b.path.split('/').length || a.path.localeCompare(b.path),
    );
  }
  return results;
}
