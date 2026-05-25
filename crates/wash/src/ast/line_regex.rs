//! Line-regex signature extraction. Faithful port of the legacy JS implementation in
//! `src/ast/index.js`. Used when no tree-sitter grammar is available for the language.

use regex::Regex;
use std::sync::OnceLock;

use crate::language::Language;

use super::{LineMapEntry, Signatures};

pub fn extract(text: &str, language: Language) -> Signatures {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut line_map: Vec<LineMapEntry> = Vec::new();
    // `out` pairs each rendered line with its 1-based source line, or `0` for synthetic
    // lines like the closing `}` we emit after eliding a body.
    let mut out: Vec<(String, u32)> = Vec::new();
    let mut in_body: i32 = 0;

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if in_body > 0 {
            for c in line.chars() {
                if c == '{' {
                    in_body += 1;
                } else if c == '}' {
                    in_body -= 1;
                }
            }
            if in_body <= 0 {
                out.push(("}".into(), 0));
                in_body = 0;
            }
            i += 1;
            continue;
        }
        if is_header(line) {
            if let Some(sym) = symbol_from_header(line) {
                line_map.push(LineMapEntry { symbol: sym, line: i as u32 + 1 });
            }
            let trimmed = strip_inline_comments(line).trim_end().to_string();
            if trimmed.ends_with('{') {
                out.push((format!("{line} …"), i as u32 + 1));
                let mut depth = 0i32;
                for c in line.chars() {
                    if c == '{' {
                        depth += 1;
                    } else if c == '}' {
                        depth -= 1;
                    }
                }
                in_body = depth;
                i += 1;
                continue;
            }
            // Python `:` block. Strip an optional `# comment` so headers like
            // `def foo():  # note` are still detected.
            let py_header = if language == Language::Python {
                line.split('#').next().unwrap_or(line).trim_end()
            } else {
                line.trim_end()
            };
            if language == Language::Python && py_header.ends_with(':') {
                out.push((format!("{line}  # …"), i as u32 + 1));
                let base_indent = leading_ws(line);
                let mut j = i + 1;
                while j < lines.len() {
                    let l = lines[j];
                    if l.trim().is_empty() {
                        j += 1;
                        continue;
                    }
                    if leading_ws(l) <= base_indent {
                        break;
                    }
                    j += 1;
                }
                i = j;
                continue;
            }
            out.push((line.to_string(), i as u32 + 1));
        }
        i += 1;
    }
    // Each `out` entry is a single line; `source_lines` is aligned 1:1.
    let content = out.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>().join("\n");
    let source_lines: Vec<u32> = out.iter().map(|(_, r)| *r).collect();
    Signatures { content, line_map, source_lines }
}

fn header_re() -> &'static [Regex] {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| {
        vec![
            Regex::new(r"^\s*(import|export\s+(?:default\s+)?(?:async\s+)?(?:function|class|interface|type|const|let|var|enum)\b)").unwrap(),
            Regex::new(r"^\s*(?:public|private|protected|static|async)?\s*(?:function|class|interface|type|enum)\b").unwrap(),
            Regex::new(r"^\s*(?:from\s+\S+\s+)?import\b").unwrap(),
            Regex::new(r"^\s*(?:def|class|async\s+def)\b").unwrap(),
            Regex::new(r"^\s*(?:func|type|package|import)\b").unwrap(),
            Regex::new(r"^\s*(?:fn|struct|enum|trait|impl|use|mod|pub\s+(?:fn|struct|enum|trait|mod))\b").unwrap(),
        ]
    })
}

fn symbol_re() -> &'static [Regex] {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| {
        vec![
            Regex::new(r"(?:function|class|interface|type|enum|const|let|var)\s+([A-Za-z_$][\w$]*)").unwrap(),
            Regex::new(r"(?:def|class)\s+([A-Za-z_][\w]*)").unwrap(),
            Regex::new(r"(?:func|type)\s+([A-Za-z_][\w]*)").unwrap(),
            Regex::new(r"(?:fn|struct|enum|trait|mod)\s+([A-Za-z_][\w]*)").unwrap(),
        ]
    })
}

fn is_header(line: &str) -> bool {
    header_re().iter().any(|r| r.is_match(line))
}

fn symbol_from_header(line: &str) -> Option<String> {
    for re in symbol_re() {
        if let Some(c) = re.captures(line) {
            return Some(c[1].to_string());
        }
    }
    None
}

fn strip_inline_comments(s: &str) -> String {
    static BLOCK_RE: OnceLock<Regex> = OnceLock::new();
    let no_line = if let Some(i) = s.find("//") { &s[..i] } else { s };
    let block_re = BLOCK_RE.get_or_init(|| Regex::new(r"/\*.*?\*/").unwrap());
    block_re.replace_all(no_line, "").into_owned()
}

fn leading_ws(s: &str) -> usize {
    s.chars().take_while(|c| c.is_whitespace() && *c != '\n').count()
}
