//! Tree-sitter-backed signature extraction for TypeScript / JavaScript / TSX / JSX.

use tree_sitter::{Node, Parser};

use crate::language::Language;

use super::{LineMapEntry, Signatures};

pub fn extract(text: &str, language: Language) -> Option<Signatures> {
    let lang = grammar(language)?;
    let mut parser = Parser::new();
    parser.set_language(&lang).ok()?;
    let tree = parser.parse(text, None)?;
    let bytes = text.as_bytes();
    let lines: Vec<&str> = text.split('\n').collect();
    // Each `out` chunk pairs rendered text with the 1-based source line where it
    // started (0 = synthetic, e.g., a closing `}` for an elided body). After joining,
    // we expand into a per-line `source_lines` vector aligned with `content`.
    let mut out: Vec<(String, u32)> = Vec::new();
    let mut line_map: Vec<LineMapEntry> = Vec::new();
    walk(tree.root_node(), bytes, &lines, &mut out, &mut line_map);
    let (content, source_lines) = render(&out);
    Some(Signatures { content, line_map, source_lines })
}

fn render(out: &[(String, u32)]) -> (String, Vec<u32>) {
    let mut content = String::new();
    let mut source_lines: Vec<u32> = Vec::new();
    for (i, (chunk, start)) in out.iter().enumerate() {
        if i > 0 {
            content.push('\n');
        }
        // Expand the chunk: first internal line uses `start`; each subsequent line
        // (separated by `\n` inside the chunk) increments the source row, except for
        // synthetic chunks (start = 0) which propagate 0.
        let mut row = *start;
        let mut first = true;
        for line in chunk.split('\n') {
            if !first {
                content.push('\n');
                if row != 0 {
                    row += 1;
                }
            }
            content.push_str(line);
            source_lines.push(row);
            first = false;
        }
    }
    (content, source_lines)
}

fn grammar(language: Language) -> Option<tree_sitter::Language> {
    match language {
        Language::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        Language::Tsx => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        Language::JavaScript | Language::Jsx => Some(tree_sitter_javascript::LANGUAGE.into()),
        _ => None,
    }
}

/// Parse `text` and return `Some(true)` if the syntax tree has no error nodes,
/// `Some(false)` if it does, `None` if the language is unsupported by tree-sitter
/// or parsing failed entirely (caller should fall back to a heuristic check).
pub(crate) fn parses_cleanly(text: &str, language: Language) -> Option<bool> {
    let lang = grammar(language)?;
    let mut parser = Parser::new();
    parser.set_language(&lang).ok()?;
    let tree = parser.parse(text, None)?;
    Some(!tree.root_node().has_error())
}

fn walk(
    node: Node,
    src: &[u8],
    lines: &[&str],
    out: &mut Vec<(String, u32)>,
    line_map: &mut Vec<LineMapEntry>,
) {
    walk_with_outer(node, node.start_byte(), src, lines, out, line_map)
}

/// Like `walk`, but uses `outer_start` instead of the node's own `start_byte()` when
/// slicing header text. The export-statement branch passes the export's start so that
/// `export function foo` keeps its prefix when we elide the body — losing `export ` is
/// what the byte-range fix would otherwise do, since recursion into the inner
/// `function_declaration` only sees from `function` onward.
fn walk_with_outer(
    node: Node,
    outer_start: usize,
    src: &[u8],
    lines: &[&str],
    out: &mut Vec<(String, u32)>,
    line_map: &mut Vec<LineMapEntry>,
) {
    let kind = node.kind();
    let src_row = node.start_position().row as u32 + 1;
    // `outer_start` may sit on a prior line (e.g., the `export` keyword), so derive the
    // outer source row from the byte offset rather than the inner node's start.
    let outer_row = byte_to_row(src, outer_start) + 1;
    match kind {
        "import_statement" | "import_declaration" => {
            push_node_text(outer_start, node.end_byte(), src, outer_row, out);
            return;
        }
        "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
            push_node_text(outer_start, node.end_byte(), src, outer_row, out);
            if let Some(sym) = name_of(node, src) {
                line_map.push(LineMapEntry { symbol: sym, line: src_row });
            }
            return;
        }
        "function_declaration"
        | "function_expression"
        | "method_definition"
        | "generator_function_declaration"
        | "method_signature" => {
            emit_signature(node, outer_start, src, outer_row, out, line_map);
            return;
        }
        "class_declaration" | "abstract_class_declaration" => {
            emit_class(node, outer_start, src, lines, outer_row, out, line_map);
            return;
        }
        "lexical_declaration" | "variable_declaration" => {
            push_node_text(outer_start, node.end_byte(), src, outer_row, out);
            if let Some(sym) = first_named_var(node, src) {
                line_map.push(LineMapEntry { symbol: sym, line: src_row });
            }
            return;
        }
        "export_statement" => {
            if let Some(decl) = node.child_by_field_name("declaration") {
                walk_with_outer(decl, node.start_byte(), src, lines, out, line_map);
                return;
            }
            push_node_text(outer_start, node.end_byte(), src, outer_row, out);
            return;
        }
        _ => {}
    }
    for child in node.children(&mut node.walk()) {
        walk(child, src, lines, out, line_map);
    }
}

fn emit_signature(
    node: Node,
    outer_start: usize,
    src: &[u8],
    outer_row: u32,
    out: &mut Vec<(String, u32)>,
    line_map: &mut Vec<LineMapEntry>,
) {
    let src_row = node.start_position().row as u32 + 1;
    if let Some(sym) = name_of(node, src) {
        line_map.push(LineMapEntry { symbol: sym, line: src_row });
    }
    let body = node.child_by_field_name("body");
    if let Some(body) = body {
        // Slice header by *byte offsets*, not whole lines — `foo() { return 1 }` on a
        // single row would otherwise leak the body text. The header runs from
        // `outer_start` (which may sit before the function node, e.g., the `export`
        // keyword) to immediately before the body's opening brace.
        let header = bytes_text(src, outer_start, body.start_byte());
        let mut combined = header.trim_end().to_string();
        combined.push_str(" { …");
        out.push((combined, outer_row));
        // The closing `}` is synthetic — no source line maps to it (use 0).
        out.push(("}".into(), 0));
    } else {
        out.push((bytes_text(src, outer_start, node.end_byte()), outer_row));
    }
}

fn emit_class(
    node: Node,
    outer_start: usize,
    src: &[u8],
    lines: &[&str],
    outer_row: u32,
    out: &mut Vec<(String, u32)>,
    line_map: &mut Vec<LineMapEntry>,
) {
    let src_row = node.start_position().row as u32 + 1;
    if let Some(sym) = name_of(node, src) {
        line_map.push(LineMapEntry { symbol: sym, line: src_row });
    }
    let body = match node.child_by_field_name("body") {
        Some(b) => b,
        None => {
            push_node_text(outer_start, node.end_byte(), src, outer_row, out);
            return;
        }
    };
    let header = bytes_text(src, outer_start, body.start_byte());
    let mut header = header.trim_end().to_string();
    header.push_str(" {");
    out.push((header, outer_row));
    for child in body.children(&mut body.walk()) {
        match child.kind() {
            "method_definition" | "method_signature" => {
                let child_row = child.start_position().row as u32 + 1;
                emit_signature(child, child.start_byte(), src, child_row, out, line_map);
            }
            "public_field_definition" | "field_definition" | "property_signature" => {
                push_node_lines(child, lines, out);
            }
            _ => {}
        }
    }
    out.push(("}".into(), 0));
}

fn slice_lines(lines: &[&str], start_row: usize, end_row: usize) -> String {
    let end = end_row.min(lines.len().saturating_sub(1));
    if start_row > end {
        return String::new();
    }
    lines[start_row..=end].join("\n")
}

fn push_node_lines(node: Node, lines: &[&str], out: &mut Vec<(String, u32)>) {
    let start_row = node.start_position().row;
    let s = slice_lines(lines, start_row, node.end_position().row);
    out.push((s, start_row as u32 + 1));
}

fn push_node_text(
    start: usize,
    end: usize,
    src: &[u8],
    start_row: u32,
    out: &mut Vec<(String, u32)>,
) {
    out.push((bytes_text(src, start, end), start_row));
}

fn byte_to_row(src: &[u8], byte: usize) -> u32 {
    let limit = byte.min(src.len());
    src[..limit].iter().filter(|&&b| b == b'\n').count() as u32
}

fn bytes_text(src: &[u8], start: usize, end: usize) -> String {
    let s = start.min(src.len());
    let e = end.min(src.len()).max(s);
    String::from_utf8_lossy(&src[s..e]).into_owned()
}

fn name_of(node: Node, src: &[u8]) -> Option<String> {
    let n = node.child_by_field_name("name")?;
    n.utf8_text(src).ok().map(|s| s.to_string())
}

fn first_named_var(node: Node, src: &[u8]) -> Option<String> {
    for child in node.children(&mut node.walk()) {
        if child.kind() == "variable_declarator" {
            if let Some(name) = child.child_by_field_name("name") {
                return name.utf8_text(src).ok().map(|s| s.to_string());
            }
        }
    }
    None
}
