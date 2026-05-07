//! AST-aware signature extraction. TS/JS use real tree-sitter; other languages fall back
//! to the line-regex approach matching the legacy JS implementation.

mod line_regex;
mod tree_sitter_sig;

use crate::language::Language;
use tree_sitter::Parser;

#[derive(Debug, Clone, serde::Serialize)]
pub struct LineMapEntry {
    pub symbol: String,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct Signatures {
    pub content: String,
    pub line_map: Vec<LineMapEntry>,
}

pub fn extract_signatures(text: &str, language: Language) -> Signatures {
    if language.has_tree_sitter() {
        if let Some(s) = tree_sitter_sig::extract(text, language) {
            return s;
        }
    }
    line_regex::extract(text, language)
}

/// Returns true if `text` parses cleanly for `language`. Tree-sitter when available;
/// brace-balance + Python indent otherwise. Used by Edit's post-edit syntax check —
/// rejecting edits that introduce parse errors saves the model an expensive retry loop.
pub fn parses_cleanly(text: &str, language: Language) -> bool {
    if language == Language::Unknown {
        return true;
    }
    if language.has_tree_sitter() {
        if let Some(ok) = tree_sitter_sig::parses_cleanly(text, language) {
            return ok;
        }
    }
    if language == Language::Python && !python_indent_sane(text) {
        return false;
    }
    pairs_balance(text)
}

fn pairs_balance(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut stack: Vec<u8> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' || c == b'\'' || c == b'`' {
            i = skip_string(bytes, i, c);
            continue;
        }
        if c == b'/' && bytes.get(i + 1).copied() == Some(b'/') {
            match memchr(bytes, i, b'\n') {
                Some(end) => i = end,
                None => break,
            }
            continue;
        }
        if c == b'/' && bytes.get(i + 1).copied() == Some(b'*') {
            let Some(end) = find_subseq(bytes, i + 2, b"*/") else {
                return false;
            };
            i = end + 2;
            continue;
        }
        if c == b'#' && (i == 0 || bytes[i - 1] == b'\n') {
            match memchr(bytes, i, b'\n') {
                Some(end) => i = end,
                None => break,
            }
            continue;
        }
        match c {
            b'(' => stack.push(b')'),
            b'[' => stack.push(b']'),
            b'{' => stack.push(b'}'),
            b')' | b']' | b'}' => {
                if stack.pop() != Some(c) {
                    return false;
                }
            }
            _ => {}
        }
        i += 1;
    }
    stack.is_empty()
}

fn skip_string(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\\' {
            i += 2;
            continue;
        }
        if c == quote {
            return i + 1;
        }
        if quote == b'`' && c == b'$' && bytes.get(i + 1).copied() == Some(b'{') {
            // Walk through template-literal expression. Strings within `${...}` can
            // themselves contain unbalanced braces (e.g., `` `${foo + "}"}` ``); skip
            // over them so a brace inside a string literal doesn't end the expression
            // early.
            let mut depth = 1i32;
            i += 2;
            while i < bytes.len() && depth > 0 {
                match bytes[i] {
                    b'"' | b'\'' | b'`' => {
                        i = skip_string(bytes, i, bytes[i]);
                        continue;
                    }
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    bytes.len()
}

fn memchr(bytes: &[u8], from: usize, target: u8) -> Option<usize> {
    bytes[from..].iter().position(|&b| b == target).map(|p| from + p)
}

fn find_subseq(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    bytes[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

fn python_indent_sane(text: &str) -> bool {
    for line in text.lines() {
        let bytes = line.as_bytes();
        let mut saw_tab = false;
        let mut saw_space_after_tab = false;
        let mut saw_space = false;
        let mut saw_tab_after_space = false;
        for &b in bytes {
            match b {
                b'\t' => {
                    if saw_space {
                        saw_tab_after_space = true;
                    }
                    saw_tab = true;
                }
                b' ' => {
                    if saw_tab {
                        saw_space_after_tab = true;
                    }
                    saw_space = true;
                }
                _ => break,
            }
        }
        if saw_space_after_tab || saw_tab_after_space {
            return false;
        }
    }
    true
}

#[allow(dead_code)]
fn _types(_: Parser) {}

/// Find the inclusive end line index of the body opened by the header at `header_idx`,
/// using brace balance for `{`-bodies and indentation for Python `:`-bodies.
pub fn find_body_end(lines: &[&str], header_idx: usize) -> usize {
    let header = lines[header_idx];
    let stripped = strip_line_comment(header).trim_end();
    if stripped.ends_with('{') {
        let mut depth = 0i32;
        for (i, l) in lines.iter().enumerate().skip(header_idx) {
            for c in l.chars() {
                if c == '{' {
                    depth += 1;
                } else if c == '}' {
                    depth -= 1;
                }
            }
            if depth <= 0 && i > header_idx {
                return i;
            }
        }
        return lines.len().saturating_sub(1);
    }
    // Python `:` body. Strip an optional `# comment` first so headers like
    // `def foo():  # note` are still detected.
    let py_stripped = header.split('#').next().unwrap_or(header).trim_end();
    if py_stripped.ends_with(':') {
        let base_indent = leading_ws(header);
        for i in (header_idx + 1)..lines.len() {
            let l = lines[i];
            if l.trim().is_empty() {
                continue;
            }
            if leading_ws(l) <= base_indent {
                return i.saturating_sub(1);
            }
        }
        return lines.len().saturating_sub(1);
    }
    header_idx
}

fn strip_line_comment(s: &str) -> &str {
    if let Some(i) = s.find("//") { &s[..i] } else { s }
}

fn leading_ws(s: &str) -> usize {
    s.chars().take_while(|c| c.is_whitespace() && *c != '\n').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typescript_signatures_via_tree_sitter() {
        let src = r#"import { foo } from "bar";

export function add(a: number, b: number): number {
  const total = a + b;
  return total;
}

export class Greeter {
  greet(name: string): string {
    return "hello " + name;
  }
}

export type Pair = [number, number];
"#;
        let s = extract_signatures(src, Language::TypeScript);
        assert!(s.content.contains("import { foo } from"), "import preserved");
        assert!(s.content.contains("export function add"), "function header preserved");
        assert!(s.content.contains("…"), "body elided with marker");
        assert!(s.content.contains("export class Greeter"), "class header preserved");
        assert!(s.content.contains("greet"), "method name preserved");
        assert!(s.content.contains("export type Pair"), "type alias preserved");
        // No body bytes leaked.
        assert!(!s.content.contains("const total = a + b"));
        assert!(!s.content.contains("\"hello \""));
        // line_map captures the function and class.
        let symbols: Vec<&str> = s.line_map.iter().map(|e| e.symbol.as_str()).collect();
        assert!(symbols.contains(&"add"));
        assert!(symbols.contains(&"Greeter"));
        assert!(symbols.contains(&"Pair"));
    }

    #[test]
    fn unknown_language_via_line_regex() {
        let src = "fn main() {\n    println!(\"hi\");\n}\n";
        let s = extract_signatures(src, Language::Rust);
        assert!(s.content.contains("fn main"));
        assert!(!s.content.contains("println"));
    }

    #[test]
    fn brace_body_end() {
        let lines = vec!["fn foo() {", "    let x = 1;", "    bar();", "}", "next"];
        assert_eq!(find_body_end(&lines, 0), 3);
    }

    #[test]
    fn one_liner_body_extracted_via_byte_range() {
        // Tree-sitter row-only slicing leaked body text for one-liners; this is the
        // regression test for the byte-range fix in tree_sitter_sig::emit_signature.
        let src = "export function add(a: number, b: number): number { return a + b; }\n";
        let s = extract_signatures(src, Language::TypeScript);
        assert!(s.content.contains("export function add"), "header preserved");
        assert!(s.content.contains("…"), "body elided with marker");
        assert!(
            !s.content.contains("return a + b"),
            "body must not leak into signature: {}",
            s.content
        );
    }

    #[test]
    fn python_body_detected_with_inline_comment() {
        let src = "def foo():  # note\n    return 1\nnext\n";
        let lines: Vec<&str> = src.split('\n').collect();
        // Header is line 0; body should span line 1; line 2 is outside.
        assert_eq!(find_body_end(&lines, 0), 1);
    }
}
