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
    let mut out: Vec<String> = Vec::new();
    let mut line_map: Vec<LineMapEntry> = Vec::new();
    walk(tree.root_node(), bytes, &lines, &mut out, &mut line_map);
    Some(Signatures { content: out.join("\n"), line_map })
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

fn walk(node: Node, src: &[u8], lines: &[&str], out: &mut Vec<String>, line_map: &mut Vec<LineMapEntry>) {
    let kind = node.kind();
    match kind {
        "import_statement" | "import_declaration" => {
            push_node_lines(node, lines, out);
            return;
        }
        "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
            push_node_lines(node, lines, out);
            if let Some(sym) = name_of(node, src) {
                line_map.push(LineMapEntry {
                    symbol: sym,
                    line: node.start_position().row as u32 + 1,
                });
            }
            return;
        }
        "function_declaration"
        | "function_expression"
        | "method_definition"
        | "generator_function_declaration"
        | "method_signature" => {
            emit_signature(node, src, lines, out, line_map);
            return;
        }
        "class_declaration" | "abstract_class_declaration" => {
            emit_class(node, src, lines, out, line_map);
            return;
        }
        "lexical_declaration" | "variable_declaration" => {
            push_node_lines(node, lines, out);
            if let Some(sym) = first_named_var(node, src) {
                line_map.push(LineMapEntry {
                    symbol: sym,
                    line: node.start_position().row as u32 + 1,
                });
            }
            return;
        }
        "export_statement" => {
            if let Some(decl) = node.child_by_field_name("declaration") {
                walk(decl, src, lines, out, line_map);
                return;
            }
            push_node_lines(node, lines, out);
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
    src: &[u8],
    lines: &[&str],
    out: &mut Vec<String>,
    line_map: &mut Vec<LineMapEntry>,
) {
    if let Some(sym) = name_of(node, src) {
        line_map.push(LineMapEntry {
            symbol: sym,
            line: node.start_position().row as u32 + 1,
        });
    }
    let body = node.child_by_field_name("body");
    let header_end_row = body
        .map(|b| b.start_position().row)
        .unwrap_or_else(|| node.end_position().row);
    let start_row = node.start_position().row;
    let header = slice_lines(lines, start_row, header_end_row);
    if body.is_some() {
        let mut combined = header.trim_end().to_string();
        if !combined.ends_with('{') {
            combined.push_str(" {");
        }
        combined.push_str(" …");
        out.push(combined);
        out.push("}".into());
    } else {
        out.push(header);
    }
}

fn emit_class(
    node: Node,
    src: &[u8],
    lines: &[&str],
    out: &mut Vec<String>,
    line_map: &mut Vec<LineMapEntry>,
) {
    if let Some(sym) = name_of(node, src) {
        line_map.push(LineMapEntry {
            symbol: sym,
            line: node.start_position().row as u32 + 1,
        });
    }
    let body = match node.child_by_field_name("body") {
        Some(b) => b,
        None => {
            push_node_lines(node, lines, out);
            return;
        }
    };
    let header = slice_lines(lines, node.start_position().row, body.start_position().row);
    let mut header = header.trim_end().to_string();
    if !header.ends_with('{') {
        header.push_str(" {");
    }
    out.push(header);
    for child in body.children(&mut body.walk()) {
        match child.kind() {
            "method_definition" | "method_signature" => {
                emit_signature(child, src, lines, out, line_map);
            }
            "public_field_definition" | "field_definition" | "property_signature" => {
                push_node_lines(child, lines, out);
            }
            _ => {}
        }
    }
    out.push("}".into());
}

fn slice_lines(lines: &[&str], start_row: usize, end_row: usize) -> String {
    let end = end_row.min(lines.len().saturating_sub(1));
    if start_row > end {
        return String::new();
    }
    lines[start_row..=end].join("\n")
}

fn push_node_lines(node: Node, lines: &[&str], out: &mut Vec<String>) {
    let s = slice_lines(lines, node.start_position().row, node.end_position().row);
    out.push(s);
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
