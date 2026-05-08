// relaywash__Edit — batched multi-file edits with fuzzy matching and tree-sitter post-check.

import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { fuzzyFindAll, normalizeForMatch } from '../fuzzy/index.js';
import { detectLanguage, parsesCleanly } from '../ast/index.js';
import { meta } from '../burn/meta.js';

export const editTool = {
  name: 'relaywash__Edit',
  description:
    'Batched multi-file edit with fuzzy matching and post-edit syntax check. Pass an array of edits and they apply atomically per-file. Whitespace and visually-equivalent Unicode differences in `oldText` are tolerated for matching only.',
  inputSchema: {
    type: 'object',
    properties: {
      edits: {
        type: 'array',
        minItems: 1,
        items: {
          type: 'object',
          properties: {
            path: { type: 'string' },
            oldText: { type: 'string' },
            newText: { type: 'string' },
            fuzzy: { type: 'boolean', default: true },
          },
          required: ['path', 'oldText', 'newText'],
          additionalProperties: false,
        },
      },
    },
    required: ['edits'],
    additionalProperties: false,
  },
  handler(args) {
    return runEdit(args || {});
  },
};

export function runEdit({ edits }) {
  /** @type {Map<string, Array<{ oldText: string; newText: string; fuzzy: boolean }>>} */
  const grouped = new Map();
  for (const e of edits) {
    if (!grouped.has(e.path)) grouped.set(e.path, []);
    grouped.get(e.path).push({ oldText: e.oldText, newText: e.newText, fuzzy: e.fuzzy !== false });
  }

  const results = [];
  for (const [path, perFileEdits] of grouped) {
    results.push(...applyToFile(path, perFileEdits));
  }
  return {
    results,
    _meta: meta(['Edit'], edits.length),
  };
}

function applyToFile(path, edits) {
  if (!existsSync(path)) {
    return edits.map(() => ({ path, ok: false, reason: 'file does not exist' }));
  }
  const original = readFileSync(path, 'utf8');
  const language = detectLanguage(path);
  const cleanBefore = language === 'unknown' ? true : parsesCleanly(original, language);

  let current = original;
  /** @type {Array<{ ok: boolean; reason?: string }>} */
  const perEditResults = [];

  for (const edit of edits) {
    const matches = locate(current, edit);
    if (matches.length === 0) {
      perEditResults.push({ ok: false, reason: 'oldText not found' });
      // Atomic: stop applying further edits on this file.
      return rollback(path, edits, perEditResults, original);
    }
    if (matches.length > 1) {
      perEditResults.push({
        ok: false,
        reason: `ambiguous match (${matches.length} occurrences) — disambiguate by including more context`,
      });
      return rollback(path, edits, perEditResults, original);
    }
    const [start, end] = matches[0];
    current = current.slice(0, start) + edit.newText + current.slice(end);
    perEditResults.push({ ok: true });
  }

  // Post-edit syntax check.
  if (cleanBefore && language !== 'unknown' && !parsesCleanly(current, language)) {
    return rollback(path, edits, perEditResults, original, 'post-edit syntax check failed');
  }

  writeFileSync(path, current);
  return perEditResults.map((r) => ({ path, ...r }));
}

function locate(text, edit) {
  if (edit.fuzzy === false) {
    const out = [];
    let i = 0;
    while (true) {
      const idx = text.indexOf(edit.oldText, i);
      if (idx === -1) break;
      out.push([idx, idx + edit.oldText.length]);
      i = idx + Math.max(1, edit.oldText.length);
    }
    return out;
  }
  // Try exact first; fall back to fuzzy.
  const exact = [];
  let i = 0;
  while (true) {
    const idx = text.indexOf(edit.oldText, i);
    if (idx === -1) break;
    exact.push([idx, idx + edit.oldText.length]);
    i = idx + Math.max(1, edit.oldText.length);
  }
  if (exact.length > 0) return exact;
  return fuzzyFindAll(text, edit.oldText);
}

function rollback(path, edits, partialResults, _original, reason) {
  // Mark all results — those past the failure get a "rolled back" reason.
  const results = [];
  for (let i = 0; i < edits.length; i++) {
    if (i < partialResults.length) {
      const r = partialResults[i];
      results.push({ path, ...r });
    } else {
      results.push({ path, ok: false, reason: 'rolled back' });
    }
  }
  if (reason) {
    // Prepend a note explaining why.
    const idx = results.findIndex((r) => r.ok);
    if (idx !== -1) results[idx] = { ...results[idx], ok: false, reason };
  }
  return results;
}

// Re-exported for tests
export { normalizeForMatch };
