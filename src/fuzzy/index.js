// Whitespace + Unicode normalization for *matching only*. The user's `newText` is written
// verbatim; we only normalize when locating where to splice.

const UNICODE_MAP = {
  '‘': "'",
  '’': "'",
  '‚': "'",
  '‛': "'",
  '“': '"',
  '”': '"',
  '„': '"',
  '‟': '"',
  '–': '-',
  '—': '-',
  '−': '-',
  ' ': ' ', // NBSP
  ' ': ' ', // narrow NBSP
  ' ': ' ', // thin space
  '​': '', // zero-width space
};

export function normalizeForMatch(s) {
  let out = '';
  for (const ch of s) {
    out += ch in UNICODE_MAP ? UNICODE_MAP[ch] : ch;
  }
  // Collapse runs of whitespace (but keep newlines distinct).
  out = out.replace(/[ \t]+/g, ' ');
  // Trim trailing whitespace on each line.
  out = out
    .split('\n')
    .map((l) => l.replace(/[ \t]+$/, ''))
    .join('\n');
  return out;
}

/**
 * Find `needle` inside `haystack`, returning all match ranges [start, end) on the original
 * haystack indexes. We normalize both sides to make the search whitespace/Unicode-tolerant,
 * but we maintain a back-map from normalized indexes to original haystack indexes so the
 * splice happens against the original text.
 */
export function fuzzyFindAll(haystack, needle) {
  const { normalized, mapBack } = normalizeWithMap(haystack);
  const normNeedle = normalizeForMatch(needle);
  if (!normNeedle) return [];
  const matches = [];
  let from = 0;
  while (true) {
    const idx = normalized.indexOf(normNeedle, from);
    if (idx === -1) break;
    const start = mapBack[idx];
    const endNormIdx = idx + normNeedle.length;
    const end = endNormIdx >= mapBack.length ? haystack.length : mapBack[endNormIdx];
    matches.push([start, end]);
    from = idx + Math.max(1, normNeedle.length);
  }
  return matches;
}

function normalizeWithMap(s) {
  let normalized = '';
  /** mapBack[i] = original index of the character that produced normalized[i]. */
  const mapBack = [];
  let lastWasSpace = false;
  for (let i = 0; i < s.length; i++) {
    let ch = s[i];
    if (ch in UNICODE_MAP) ch = UNICODE_MAP[ch];
    if (ch === '') continue; // zero-width drop
    if (ch === ' ' || ch === '\t') {
      if (lastWasSpace) continue;
      normalized += ' ';
      mapBack.push(i);
      lastWasSpace = true;
      continue;
    }
    if (ch === '\n') {
      // Trim trailing spaces on the line we just finished.
      while (normalized.endsWith(' ')) {
        normalized = normalized.slice(0, -1);
        mapBack.pop();
      }
      normalized += '\n';
      mapBack.push(i);
      lastWasSpace = false;
      continue;
    }
    normalized += ch;
    mapBack.push(i);
    lastWasSpace = false;
  }
  return { normalized, mapBack };
}
