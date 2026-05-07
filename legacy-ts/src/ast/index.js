// Tree-sitter wrapper. The real implementation lazy-loads grammar files via the `web-tree-sitter`
// or `tree-sitter` Node bindings. To keep the bundle dependency-free we ship a *fallback*
// brace/paren balance check that catches the most common syntactic regressions caused by Edit.
//
// When tree-sitter is installed in the user's environment we'd swap this out — see the
// `tryLoadTreeSitter` hook below.

/**
 * Detect language from a file extension. Returns a string identifier or 'unknown'.
 */
export function detectLanguage(path) {
  const m = /\.([a-zA-Z0-9]+)$/.exec(path);
  if (!m) return 'unknown';
  switch (m[1]) {
    case 'ts':
    case 'tsx':
      return 'typescript';
    case 'js':
    case 'jsx':
    case 'mjs':
    case 'cjs':
      return 'javascript';
    case 'py':
      return 'python';
    case 'go':
      return 'go';
    case 'rs':
      return 'rust';
    default:
      return 'unknown';
  }
}

/**
 * Returns true if `text` parses cleanly (or close enough) for the given language.
 * Uses balance heuristics for now; the public surface stays stable so a real tree-sitter
 * backend can be plugged in later.
 */
export function parsesCleanly(text, language) {
  if (language === 'python') return pythonIndentSane(text) && pairsBalance(text);
  return pairsBalance(text);
}

function pairsBalance(text) {
  const stack = [];
  const pairs = { '(': ')', '[': ']', '{': '}' };
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    // Skip strings and comments — best-effort.
    if (c === '"' || c === "'" || c === '`') {
      i = skipString(text, i, c);
      continue;
    }
    if (c === '/' && text[i + 1] === '/') {
      i = text.indexOf('\n', i);
      if (i === -1) break;
      continue;
    }
    if (c === '/' && text[i + 1] === '*') {
      const end = text.indexOf('*/', i + 2);
      if (end === -1) return false;
      i = end + 2;
      continue;
    }
    if (c === '#' && (i === 0 || text[i - 1] === '\n')) {
      // Python-style line comment.
      i = text.indexOf('\n', i);
      if (i === -1) break;
      continue;
    }
    if (c in pairs) {
      stack.push(pairs[c]);
    } else if (c === ')' || c === ']' || c === '}') {
      if (stack.pop() !== c) return false;
    }
    i++;
  }
  return stack.length === 0;
}

function skipString(text, start, quote) {
  let i = start + 1;
  while (i < text.length) {
    const c = text[i];
    if (c === '\\') {
      i += 2;
      continue;
    }
    if (c === quote) return i + 1;
    if (quote === '`' && c === '$' && text[i + 1] === '{') {
      // Walk through template-literal expression.
      let depth = 1;
      i += 2;
      while (i < text.length && depth) {
        if (text[i] === '{') depth++;
        else if (text[i] === '}') depth--;
        i++;
      }
      continue;
    }
    i++;
  }
  return text.length;
}

function pythonIndentSane(text) {
  // Reject obvious tab/space mixing that breaks Python; otherwise pass.
  const lines = text.split('\n');
  for (const line of lines) {
    if (/^\t+ |^ +\t/.test(line)) return false;
  }
  return true;
}

/**
 * Extract a signatures-mode view of a file: imports, type/class/interface/function declarations,
 * with bodies replaced by `…`. Returns { content, lineMap, languageDetected }.
 *
 * Heuristic, line-based extraction. Good enough to demonstrate behavior; a tree-sitter pass
 * lands later.
 */
export function extractSignatures(text, language) {
  const lines = text.split('\n');
  const lineMap = [];
  const out = [];
  let inBody = 0; // brace depth above 0 = inside a body we elided

  const isHeader = (line) => {
    return (
      /^\s*(import|export\s+(?:default\s+)?(?:async\s+)?(?:function|class|interface|type|const|let|var|enum)\b)/.test(
        line,
      ) ||
      /^\s*(?:public|private|protected|static|async)?\s*(?:function|class|interface|type|enum)\b/.test(
        line,
      ) ||
      /^\s*(?:from\s+\S+\s+)?import\b/.test(line) ||
      /^\s*(?:def|class|async\s+def)\b/.test(line) || // python
      /^\s*(?:func|type|package|import)\b/.test(line) || // go
      /^\s*(?:fn|struct|enum|trait|impl|use|mod|pub\s+(?:fn|struct|enum|trait|mod))\b/.test(line) // rust
    );
  };

  const symbolFromHeader = (line) => {
    const patterns = [
      /(?:function|class|interface|type|enum|const|let|var)\s+([A-Za-z_$][\w$]*)/,
      /(?:def|class)\s+([A-Za-z_][\w]*)/,
      /(?:func|type)\s+([A-Za-z_][\w]*)/,
      /(?:fn|struct|enum|trait|mod)\s+([A-Za-z_][\w]*)/,
    ];
    for (const re of patterns) {
      const m = re.exec(line);
      if (m) return m[1];
    }
    return null;
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (inBody > 0) {
      // Track brace depth to know when we exit the body.
      for (const c of line) {
        if (c === '{') inBody++;
        else if (c === '}') inBody--;
      }
      if (inBody <= 0) {
        out.push('}');
        inBody = 0;
      }
      continue;
    }
    if (isHeader(line)) {
      const sym = symbolFromHeader(line);
      if (sym) lineMap.push({ symbol: sym, line: i + 1 });
      // If the header opens a body (ends with `{` after stripping trailing comment/whitespace),
      // emit the header line + `…` body marker and skip until the matching close.
      const trimmed = line.replace(/\/\/.*$/, '').replace(/\/\*.*\*\//g, '').trimEnd();
      if (trimmed.endsWith('{')) {
        out.push(line + ' …');
        inBody = 1;
        // Count nested braces opened on this same line (e.g. `class Foo { static bar = { … }`).
        let depth = 0;
        for (const c of line) {
          if (c === '{') depth++;
          else if (c === '}') depth--;
        }
        inBody = depth;
        continue;
      }
      // Python-style header (ends with `:`)
      if (/:\s*$/.test(line) && language === 'python') {
        out.push(line + '  # …');
        // For python, find next line with same-or-less indentation to close the body.
        const baseIndent = (/^\s*/.exec(line) || [''])[0].length;
        let j = i + 1;
        while (j < lines.length) {
          const l = lines[j];
          if (l.trim() === '') {
            j++;
            continue;
          }
          const ind = (/^\s*/.exec(l) || [''])[0].length;
          if (ind <= baseIndent) break;
          j++;
        }
        i = j - 1;
        continue;
      }
      out.push(line);
    }
    // Drop everything else.
  }
  return { content: out.join('\n'), lineMap };
}
