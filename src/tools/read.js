// relaywash__Read — AST-aware read with signatures mode, mtime cache, and heuristics
// to suppress signatures where they backfire (small files / small functions / recently-searched
// symbols).

import { readFileSync, statSync } from 'node:fs';
import { detectLanguage, extractSignatures } from '../ast/index.js';
import { meta } from '../burn/meta.js';

const SMALL_FILE_LINES = 200;
const SMALL_FUNCTION_LINES = 20;

/** @type {Map<string, { path: string; mtime: number }>} */
const sessionMtimeCache = new Map();

/** Most-recent search query per session (set by the Search tool). */
let lastSearchSymbol = null;

export function noteSearchedSymbol(sym) {
  lastSearchSymbol = sym || null;
}

export function _resetReadCache() {
  sessionMtimeCache.clear();
  lastSearchSymbol = null;
}

export const readTool = {
  name: 'relaywash__Read',
  description:
    'AST-aware read. Default mode "signatures" returns imports + declarations + signatures (bodies elided) plus a `lineMap` so you can issue precise `mode: "range"` follow-ups. Small files come back fully. Repeated reads of an unchanged file in the same session return empty content.',
  inputSchema: {
    type: 'object',
    properties: {
      path: { type: 'string' },
      mode: { type: 'string', enum: ['signatures', 'range', 'full'] },
      range: {
        type: 'array',
        items: { type: 'integer' },
        minItems: 2,
        maxItems: 2,
        description: '1-based inclusive [start, end] line range.',
      },
    },
    required: ['path'],
    additionalProperties: false,
  },
  handler(args) {
    return runRead(args || {});
  },
};

export function runRead({ path, mode, range }) {
  const language = detectLanguage(path);
  const stat = statSync(path);
  const cached = sessionMtimeCache.get(path);
  const unchanged = cached && cached.mtime === stat.mtimeMs;

  if (unchanged && !range) {
    return {
      content: '',
      truncated: false,
      languageDetected: language,
      _meta: meta(['Read'], 1),
    };
  }

  const text = readFileSync(path, 'utf8');
  sessionMtimeCache.set(path, { path, mtime: stat.mtimeMs });

  if (mode === 'range' || range) {
    if (!range || range.length !== 2) {
      throw new Error('mode: "range" requires `range: [start, end]`');
    }
    const [start, end] = range;
    const lines = text.split('\n');
    const slice = lines.slice(Math.max(0, start - 1), Math.min(lines.length, end)).join('\n');
    return {
      content: slice,
      truncated: false,
      languageDetected: language,
      _meta: meta(['Read'], 1),
    };
  }

  if (mode === 'full' || language === 'unknown') {
    return {
      content: text,
      truncated: false,
      languageDetected: language,
      _meta: meta(['Read'], 1),
    };
  }

  // mode === 'signatures' (default for known languages).
  const lines = text.split('\n');
  if (lines.length <= SMALL_FILE_LINES) {
    return {
      content: text,
      truncated: false,
      languageDetected: language,
      _meta: meta(['Read'], 1),
    };
  }

  const { content, lineMap } = extractSignatures(text, language);
  // Heuristic: if a symbol matches the most-recent Search query OR a function in the file
  // is small (< SMALL_FUNCTION_LINES), include its body in the output.
  const augmented = augmentWithSmallBodies(text, content, lineMap, lastSearchSymbol);

  return {
    content: augmented,
    truncated: true,
    languageDetected: language,
    lineMap,
    _meta: meta(['Read'], 1),
  };
}

function augmentWithSmallBodies(fullText, sigContent, lineMap, searchedSymbol) {
  // For each entry in lineMap, look up its starting line in fullText and figure out the body
  // span. If the body is < SMALL_FUNCTION_LINES OR the symbol matches `searchedSymbol`, splice
  // the full body back into sigContent, replacing the `… body` placeholder for that header.
  if (!lineMap || !lineMap.length) return sigContent;
  const fullLines = fullText.split('\n');
  const sigLines = sigContent.split('\n');
  const sigLineIdxByText = new Map();
  sigLines.forEach((l, i) => sigLineIdxByText.set(l, i));

  for (const entry of lineMap) {
    const headerIdx = entry.line - 1;
    if (headerIdx < 0 || headerIdx >= fullLines.length) continue;
    const bodyEnd = findBodyEnd(fullLines, headerIdx);
    const bodyLen = bodyEnd - headerIdx;
    const matchesSearched =
      searchedSymbol && entry.symbol && entry.symbol.toLowerCase() === searchedSymbol.toLowerCase();
    if (!matchesSearched && bodyLen > SMALL_FUNCTION_LINES) continue;
    // Locate header in sig content and replace the elided body marker with the full body.
    const headerLine = fullLines[headerIdx];
    const sigIdx = sigLines.findIndex((l) => l.startsWith(headerLine));
    if (sigIdx === -1) continue;
    // Replace next 1 line ('}' or '… body line') with full body slice.
    const fullBody = fullLines.slice(headerIdx, bodyEnd + 1);
    sigLines.splice(sigIdx, 2, ...fullBody);
  }
  return sigLines.join('\n');
}

function findBodyEnd(lines, headerIdx) {
  const headerLine = lines[headerIdx];
  if (headerLine.replace(/\/\/.*$/, '').trimEnd().endsWith('{')) {
    let depth = 0;
    for (let i = headerIdx; i < lines.length; i++) {
      for (const c of lines[i]) {
        if (c === '{') depth++;
        else if (c === '}') depth--;
      }
      if (depth <= 0 && i > headerIdx) return i;
    }
    return lines.length - 1;
  }
  if (/:\s*$/.test(headerLine)) {
    const baseIndent = (/^\s*/.exec(headerLine) || [''])[0].length;
    for (let i = headerIdx + 1; i < lines.length; i++) {
      if (lines[i].trim() === '') continue;
      const ind = (/^\s*/.exec(lines[i]) || [''])[0].length;
      if (ind <= baseIndent) return i - 1;
    }
    return lines.length - 1;
  }
  return headerIdx;
}
