// Copyright (c) 2026 PHINs Group
// SPDX-License-Identifier: MIT OR Apache-2.0

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tree_sitter::{Node, Parser};

use crate::model::NodeKind;

#[derive(Debug, Clone)]
pub struct ParsedSymbol {
    pub kind: NodeKind,
    pub name: String,
    pub qualified_name: String,
    pub start_line: i64,
    pub end_line: i64,
    pub signature: String,
    pub doc_summary: Option<String>,
    pub snippet: String,
}

#[derive(Debug, Clone)]
pub struct ParsedImport {
    pub source: String,
    pub line: i64,
    pub bindings: Vec<ParsedImportBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedImportBinding {
    pub imported: String,
    pub local: String,
}

#[derive(Debug, Clone)]
pub struct ParsedCall {
    pub name: String,
    pub qualifier: Option<String>,
    pub line: i64,
}

#[derive(Debug, Clone)]
pub struct ParsedRoute {
    pub method: String,
    pub path: String,
    pub handler: Option<String>,
    pub line: i64,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedFile {
    pub symbols: Vec<ParsedSymbol>,
    pub imports: Vec<ParsedImport>,
    pub calls: Vec<ParsedCall>,
    pub routes: Vec<ParsedRoute>,
}

pub fn parse_file(language: Option<&str>, rel_path: &str, source: &str) -> Result<ParsedFile> {
    let Some(language) = language else {
        return Ok(ParsedFile::default());
    };

    let imports = extract_imports(language, source);
    let routes = extract_routes(language, source);
    let (symbols, calls) = match language {
        "javascript" | "typescript" | "rust" => parse_tree_sitter(language, rel_path, source)?,
        _ => (Vec::new(), Vec::new()),
    };
    Ok(ParsedFile {
        symbols,
        imports,
        calls,
        routes,
    })
}

pub fn symbol_metadata(
    signature: &str,
    calls: &[String],
    doc_summary: Option<&str>,
) -> serde_json::Value {
    json!({ "signature": signature, "calls": calls, "doc_summary": doc_summary })
}

fn parse_tree_sitter(
    language: &str,
    rel_path: &str,
    source: &str,
) -> Result<(Vec<ParsedSymbol>, Vec<ParsedCall>)> {
    let mut parser = Parser::new();
    match language {
        "javascript" => {
            let language = tree_sitter_javascript::LANGUAGE;
            parser.set_language(&language.into())?;
        }
        "typescript" => {
            let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
            parser.set_language(&language.into())?;
        }
        "rust" => {
            let language = tree_sitter_rust::LANGUAGE;
            parser.set_language(&language.into())?;
        }
        _ => return Ok((Vec::new(), Vec::new())),
    }

    let tree = parser
        .parse(source, None)
        .context("tree-sitter failed to parse source")?;
    let mut symbols = Vec::new();
    let mut calls = Vec::new();
    collect_ast_facts(
        tree.root_node(),
        language,
        rel_path,
        source,
        &mut symbols,
        &mut calls,
    );
    Ok((symbols, calls))
}

fn collect_ast_facts(
    node: Node<'_>,
    language: &str,
    rel_path: &str,
    source: &str,
    symbols: &mut Vec<ParsedSymbol>,
    calls: &mut Vec<ParsedCall>,
) {
    if let Some((kind, name)) = classify_symbol(node, language, source) {
        let start_line = node.start_position().row as i64 + 1;
        let end_line = node.end_position().row as i64 + 1;
        let snippet = node_text(node, source);
        let signature = signature_from_snippet(&snippet);
        let symbol_kind = if is_test_symbol(&name, rel_path) {
            NodeKind::Test
        } else {
            kind
        };
        symbols.push(ParsedSymbol {
            kind: symbol_kind,
            qualified_name: format!("{}::{}", rel_path, name),
            name,
            start_line,
            end_line,
            signature,
            doc_summary: leading_doc_summary(source, start_line),
            snippet: trim_snippet(&snippet, 4000),
        });
    }

    if node.kind() == "call_expression" {
        if let Some((qualifier, name)) = call_name(node, source) {
            calls.push(ParsedCall {
                name,
                qualifier,
                line: node.start_position().row as i64 + 1,
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_ast_facts(child, language, rel_path, source, symbols, calls);
    }
}

fn classify_symbol(node: Node<'_>, language: &str, source: &str) -> Option<(NodeKind, String)> {
    let kind = node.kind();
    match language {
        "javascript" | "typescript" => match kind {
            "function_declaration" | "generator_function_declaration" => {
                node_name(node, source).map(|name| (NodeKind::Function, name))
            }
            "method_definition" | "abstract_method_signature" | "method_signature" => {
                node_name(node, source).map(|name| (NodeKind::Method, name))
            }
            "class_declaration" | "abstract_class_declaration" => {
                node_name(node, source).map(|name| (NodeKind::Class, name))
            }
            "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
                node_name(node, source).map(|name| (NodeKind::Type, name))
            }
            "variable_declarator" => {
                let value = node.child_by_field_name("value")?;
                if matches!(
                    value.kind(),
                    "arrow_function" | "function" | "function_expression"
                ) {
                    node_name(node, source).map(|name| (NodeKind::Function, name))
                } else {
                    None
                }
            }
            _ => None,
        },
        "rust" => match kind {
            "function_item" => {
                let node_kind = if has_parent_kind(node, "impl_item") {
                    NodeKind::Method
                } else {
                    NodeKind::Function
                };
                node_name(node, source).map(|name| (node_kind, name))
            }
            "struct_item" | "enum_item" | "trait_item" | "type_item" => {
                node_name(node, source).map(|name| (NodeKind::Type, name))
            }
            _ => None,
        },
        _ => None,
    }
}

fn node_name(node: Node<'_>, source: &str) -> Option<String> {
    node.child_by_field_name("name")
        .map(|name| node_text(name, source))
        .filter(|name| !name.is_empty())
}

fn node_text(node: Node<'_>, source: &str) -> String {
    node.utf8_text(source.as_bytes())
        .unwrap_or_default()
        .to_string()
}

fn call_name(node: Node<'_>, source: &str) -> Option<(Option<String>, String)> {
    let function = node.child_by_field_name("function")?;
    let text = node_text(function, source);
    simple_callee_parts(&text)
}

fn simple_callee_name(text: &str) -> Option<String> {
    simple_callee_parts(text).map(|(_, name)| name)
}

fn simple_callee_parts(text: &str) -> Option<(Option<String>, String)> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut parts: Vec<String> = trimmed
        .split(['.', ':'])
        .map(|part| {
            part.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_')
                .to_string()
        })
        .filter(|part| !part.is_empty())
        .collect();
    let name = parts.pop()?;
    if name.chars().all(|ch| ch.is_ascii_digit()) {
        None
    } else {
        Some((parts.pop(), name))
    }
}

fn has_parent_kind(node: Node<'_>, kind: &str) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == kind {
            return true;
        }
        current = parent.parent();
    }
    false
}

fn signature_from_snippet(snippet: &str) -> String {
    let first_line = snippet.lines().next().unwrap_or_default().trim();
    if first_line.len() <= 240 {
        first_line.to_string()
    } else {
        format!("{}...", &first_line[..240])
    }
}

fn trim_snippet(snippet: &str, max_chars: usize) -> String {
    if snippet.len() <= max_chars {
        snippet.to_string()
    } else {
        format!("{}...", &snippet[..max_chars])
    }
}

fn leading_doc_summary(source: &str, start_line: i64) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let mut docs = Vec::new();
    let mut idx = start_line as isize - 2;
    while idx >= 0 {
        let line = lines.get(idx as usize)?.trim();
        if line.is_empty() {
            if docs.is_empty() {
                idx -= 1;
                continue;
            }
            break;
        }
        let doc = line
            .strip_prefix("///")
            .or_else(|| line.strip_prefix("//"))
            .or_else(|| line.strip_prefix("*"))
            .map(str::trim)
            .map(|line| line.trim_start_matches('/').trim())
            .filter(|line| !line.is_empty() && *line != "*/");
        let Some(doc) = doc else {
            break;
        };
        docs.push(doc.to_string());
        idx -= 1;
    }
    docs.reverse();
    let summary = docs.join(" ");
    if summary.is_empty() {
        None
    } else {
        Some(summary.chars().take(240).collect())
    }
}

fn is_test_symbol(name: &str, rel_path: &str) -> bool {
    let lower_name = name.to_ascii_lowercase();
    let lower_path = rel_path.to_ascii_lowercase();
    lower_name.starts_with("test_")
        || lower_name.ends_with("_test")
        || lower_name.contains("should")
        || lower_path.contains("test")
        || lower_path.contains(".spec.")
}

fn extract_imports(language: &str, source: &str) -> Vec<ParsedImport> {
    source
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let trimmed = line.trim();
            let mut import = match language {
                "javascript" | "typescript" => extract_js_import(trimmed),
                "rust" => extract_rust_import(trimmed).map(|source| ParsedImport {
                    source,
                    line: idx as i64 + 1,
                    bindings: Vec::new(),
                }),
                _ => None,
            }?;
            import.line = idx as i64 + 1;
            Some(import)
        })
        .collect()
}

fn extract_routes(language: &str, source: &str) -> Vec<ParsedRoute> {
    if !matches!(language, "javascript" | "typescript") {
        return Vec::new();
    }
    source
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| extract_route_line(line.trim(), idx as i64 + 1))
        .collect()
}

fn extract_route_line(line: &str, line_number: i64) -> Option<ParsedRoute> {
    for method in ["get", "post", "put", "patch", "delete"] {
        let marker = format!(".{}(", method);
        let Some(start) = line.find(&marker) else {
            continue;
        };
        let args = &line[start + marker.len()..];
        let route_path = quoted_after(args)?;
        let handler = route_handler_name(args);
        return Some(ParsedRoute {
            method: method.to_ascii_uppercase(),
            path: route_path,
            handler,
            line: line_number,
        });
    }
    None
}

fn route_handler_name(args: &str) -> Option<String> {
    let first_quote = args.find(['"', '\''])?;
    let quote = args.as_bytes()[first_quote] as char;
    let after_path = &args[first_quote + 1..];
    let path_end = after_path.find(quote)?;
    let rest = after_path[path_end + 1..].trim_start();
    let rest = rest.strip_prefix(',')?.trim_start();
    let handler = rest.split([',', ')']).next().unwrap_or_default().trim();
    simple_callee_name(handler)
}

fn extract_js_import(line: &str) -> Option<ParsedImport> {
    if !line.starts_with("import ") && !line.starts_with("export ") {
        return None;
    }
    if line.starts_with("export ") && !line.contains(" from ") {
        return None;
    }
    let line_before_from = line.find(" from ").map(|idx| &line[..idx]).unwrap_or(line);
    let source = if let Some(idx) = line.find(" from ") {
        quoted_after(&line[idx + 6..])?
    } else {
        quoted_after(line)?
    };
    Some(ParsedImport {
        source,
        line: 0,
        bindings: extract_js_import_bindings(line_before_from),
    })
}

fn extract_rust_import(line: &str) -> Option<String> {
    let line = line
        .strip_prefix("use ")
        .or_else(|| line.strip_prefix("mod "))?;
    Some(
        line.trim_end_matches(';')
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string(),
    )
    .filter(|value| !value.is_empty())
}

fn quoted_after(text: &str) -> Option<String> {
    let start = text.find(['"', '\''])?;
    let quote = text.as_bytes()[start] as char;
    let rest = &text[start + 1..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

fn extract_js_import_bindings(line_before_from: &str) -> Vec<ParsedImportBinding> {
    let mut bindings = Vec::new();
    if let (Some(start), Some(end)) = (line_before_from.find('{'), line_before_from.rfind('}')) {
        for item in line_before_from[start + 1..end].split(',') {
            let item = item.trim().trim_start_matches("type ").trim();
            if item.is_empty() {
                continue;
            }
            let parts: Vec<&str> = item.split_whitespace().collect();
            let (imported, local) = if parts.len() == 3 && parts[1] == "as" {
                (parts[0], parts[2])
            } else {
                (parts[0], parts[0])
            };
            bindings.push(ParsedImportBinding {
                imported: clean_binding_name(imported),
                local: clean_binding_name(local),
            });
        }
        return bindings;
    }

    if line_before_from.starts_with("import ") {
        if let Some(namespace) = line_before_from
            .trim_start_matches("import ")
            .strip_prefix("* as ")
        {
            let local = clean_binding_name(namespace);
            if !local.is_empty() {
                bindings.push(ParsedImportBinding {
                    imported: "*".to_string(),
                    local,
                });
            }
            return bindings;
        }
        let default_part = line_before_from
            .trim_start_matches("import ")
            .split(',')
            .next()
            .unwrap_or_default()
            .trim();
        if !default_part.is_empty() && !default_part.starts_with('*') {
            let local = clean_binding_name(default_part);
            bindings.push(ParsedImportBinding {
                imported: local.clone(),
                local,
            });
        }
    }
    bindings
}

fn clean_binding_name(name: &str) -> String {
    name.trim()
        .trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typescript_symbols_and_imports() -> Result<()> {
        let source = r#"
            import { upload } from "./upload";
            export class AvatarService {
              saveAvatar(file: File) { return upload(file); }
            }
            export const helper = () => 1;
        "#;
        let parsed = parse_file(Some("typescript"), "src/avatar.ts", source)?;
        assert!(parsed
            .symbols
            .iter()
            .any(|symbol| symbol.name == "AvatarService"));
        assert!(parsed.symbols.iter().any(|symbol| symbol.name == "helper"));
        assert!(parsed.calls.iter().any(|call| call.name == "upload"));
        assert_eq!(parsed.imports[0].source, "./upload");
        assert_eq!(parsed.imports[0].bindings[0].local, "upload");
        Ok(())
    }

    #[test]
    fn parses_route_entrypoints() -> Result<()> {
        let source = "router.post('/avatar', saveAvatar);\n";
        let parsed = parse_file(Some("typescript"), "src/routes.ts", source)?;
        assert_eq!(parsed.routes[0].method, "POST");
        assert_eq!(parsed.routes[0].path, "/avatar");
        assert_eq!(parsed.routes[0].handler.as_deref(), Some("saveAvatar"));
        Ok(())
    }

    #[test]
    fn parses_namespace_import_and_qualified_call() -> Result<()> {
        let source = "import * as uploads from './upload';\nexport function saveAvatar() { return uploads.upload(); }\n";
        let parsed = parse_file(Some("typescript"), "src/avatar.ts", source)?;
        assert_eq!(parsed.imports[0].bindings[0].imported, "*");
        assert_eq!(parsed.imports[0].bindings[0].local, "uploads");
        let call = parsed
            .calls
            .iter()
            .find(|call| call.name == "upload")
            .unwrap();
        assert_eq!(call.qualifier.as_deref(), Some("uploads"));
        Ok(())
    }

    #[test]
    fn extracts_leading_doc_summary() -> Result<()> {
        let source =
            "// Uploads the avatar image.\nexport function uploadAvatar() { return true; }\n";
        let parsed = parse_file(Some("typescript"), "src/avatar.ts", source)?;
        let symbol = parsed
            .symbols
            .iter()
            .find(|symbol| symbol.name == "uploadAvatar")
            .unwrap();
        assert_eq!(
            symbol.doc_summary.as_deref(),
            Some("Uploads the avatar image.")
        );
        Ok(())
    }

    #[test]
    fn does_not_treat_export_string_literal_as_import() -> Result<()> {
        let source = "export function upload() { return \"ok\"; }\n";
        let parsed = parse_file(Some("typescript"), "src/upload.ts", source)?;
        assert!(parsed.imports.is_empty());
        Ok(())
    }
}
