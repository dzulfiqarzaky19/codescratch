//! tree-sitter TS/JS → symbols + call sites + import bindings.
//! Recursive node walk (kind/field API is stable across tree-sitter versions);
//! avoids the version-fragile Query API on purpose.

use crate::model::{Edge, FileFacts, ImportBinding, Lang, RawCall, Symbol};
use tree_sitter::{Node, Parser};

pub mod heuristics;
pub mod plugins;
pub mod python;

fn language(lang: Lang) -> Option<tree_sitter::Language> {
    Some(match lang {
        Lang::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Lang::Js => tree_sitter_javascript::LANGUAGE.into(),
        Lang::Py => return None, // WP-4A adds tree-sitter-python
    })
}

pub fn extract(path_rel: &str, src: &str) -> FileFacts {
    let mut facts = FileFacts { symbols: vec![], calls: vec![], imports: vec![] };
    let Some(lang) = Lang::from_path(path_rel) else {
        return facts;
    };
    if lang == Lang::Py {
        return python::extract(path_rel, src);
    }
    let Some(ts_lang) = language(lang) else {
        return facts; // known language, grammar not yet linked
    };
    let mut parser = Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return facts;
    }
    let Some(tree) = parser.parse(src, None) else {
        return facts;
    };
    let bytes = src.as_bytes();
    let mut ctx = Ctx { file: path_rel, bytes, enclosing: None, class: None, facts: &mut facts };
    visit(tree.root_node(), &mut ctx);
    facts
}

struct Ctx<'a> {
    file: &'a str,
    bytes: &'a [u8],
    enclosing: Option<String>,        // nearest function/method symbol id
    class: Option<(String, String)>,  // (class name, class id)
    facts: &'a mut FileFacts,
}

fn text<'a>(n: Node, bytes: &'a [u8]) -> &'a str {
    n.utf8_text(bytes).unwrap_or("")
}

fn is_exported(n: Node) -> bool {
    n.parent().map(|p| p.kind() == "export_statement").unwrap_or(false)
}

fn signature(n: Node, bytes: &[u8]) -> String {
    let full = text(n, bytes);
    let cut = full.find(['{', '\n']).unwrap_or(full.len());
    let mut s = full[..cut].trim().to_string();
    if s.len() > 200 {
        s.truncate(200);
    }
    s
}

fn visit(node: Node, ctx: &mut Ctx) {
    match node.kind() {
        "function_declaration" | "generator_function_declaration" => {
            if let Some(name) = node.child_by_field_name("name") {
                let id = push_symbol(ctx, node, name, "function", None);
                recurse_with(node, ctx, Some(id), ctx.class.clone());
                return;
            }
        }
        "class_declaration" | "abstract_class_declaration" => {
            if let Some(name) = node.child_by_field_name("name") {
                let nm = text(name, ctx.bytes).to_string();
                let id = push_symbol(ctx, node, name, "class", None);
                recurse_with(node, ctx, ctx.enclosing.clone(), Some((nm, id)));
                return;
            }
        }
        "method_definition" => {
            if let Some(name) = node.child_by_field_name("name") {
                let qual = ctx
                    .class
                    .as_ref()
                    .map(|(c, _)| format!("{c}.{}", text(name, ctx.bytes)));
                let id = push_symbol(ctx, node, name, "method", qual);
                recurse_with(node, ctx, Some(id), ctx.class.clone());
                return;
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            let exported = is_exported(node);
            for i in 0..node.named_child_count() {
                let decl = node.named_child(i).unwrap();
                if decl.kind() != "variable_declarator" {
                    continue;
                }
                let value_is_fn = decl
                    .child_by_field_name("value")
                    .map(|v| matches!(v.kind(), "arrow_function" | "function" | "function_expression"))
                    .unwrap_or(false);
                if value_is_fn {
                    if let Some(name) = decl.child_by_field_name("name") {
                        if name.kind() == "identifier" {
                            let id = push_symbol_exported(ctx, decl, name, "function", None, exported);
                            recurse_with(decl, ctx, Some(id), ctx.class.clone());
                            continue;
                        }
                    }
                }
                visit(decl, ctx);
            }
            return;
        }
        "call_expression" => {
            record_call(node, ctx);
            // fall through to recurse (nested calls in args / callee object)
        }
        "import_statement" => {
            record_imports(node, ctx);
            return;
        }
        "export_statement" => {
            record_reexports(node, ctx);
            // fall through so `export function` / `export class` still visit
        }
        _ => {}
    }
    // default: recurse over all children, context unchanged
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(child, ctx);
    }
}

fn recurse_with(
    node: Node,
    ctx: &mut Ctx,
    enclosing: Option<String>,
    class: Option<(String, String)>,
) {
    let (pe, pc) = (ctx.enclosing.take(), ctx.class.take());
    ctx.enclosing = enclosing;
    ctx.class = class;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(child, ctx);
    }
    ctx.enclosing = pe;
    ctx.class = pc;
}

fn push_symbol(ctx: &mut Ctx, def: Node, name: Node, kind: &str, qual: Option<String>) -> String {
    let exported = is_exported(def);
    push_symbol_exported(ctx, def, name, kind, qual, exported)
}

fn push_symbol_exported(
    ctx: &mut Ctx,
    def: Node,
    name: Node,
    kind: &str,
    qual: Option<String>,
    exported: bool,
) -> String {
    let nm = text(name, ctx.bytes).to_string();
    let start = def.start_position().row + 1;
    let end = def.end_position().row + 1;
    let id = Symbol::make_id(ctx.file, &nm, start);
    ctx.facts.symbols.push(Symbol {
        id: id.clone(),
        kind: kind.to_string(),
        name: nm.clone(),
        qualified_name: qual.unwrap_or(nm),
        file_path: ctx.file.to_string(),
        start_line: start,
        end_line: end,
        exported,
        signature: signature(def, ctx.bytes),
    });
    id
}

fn record_call(node: Node, ctx: &mut Ctx) {
    let Some(callee) = node.child_by_field_name("function") else {
        return;
    };
    let (name, member) = match callee.kind() {
        "identifier" => (text(callee, ctx.bytes).to_string(), false),
        "member_expression" => match callee.child_by_field_name("property") {
            Some(p) => (text(p, ctx.bytes).to_string(), true),
            None => return,
        },
        _ => return,
    };
    if name.is_empty() {
        return;
    }
    let from_id = ctx
        .enclosing
        .clone()
        .unwrap_or_else(|| Symbol::module_id(ctx.file));
    ctx.facts.calls.push(RawCall {
        from_id,
        name,
        member,
        line: node.start_position().row + 1,
        file_path: ctx.file.to_string(),
    });
}

fn record_imports(node: Node, ctx: &mut Ctx) {
    let Some(src) = node.child_by_field_name("source") else {
        return;
    };
    let module = text(src, ctx.bytes).trim_matches(['"', '\'', '`']).to_string();
    if module.is_empty() {
        return;
    }
    // Walk the import clause for default / namespace / named specifiers.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "import_clause" {
            continue;
        }
        let mut c2 = child.walk();
        for part in child.children(&mut c2) {
            match part.kind() {
                "identifier" => push_binding(ctx, &module, text(part, ctx.bytes), "default", "default"),
                "namespace_import" => {
                    if let Some(id) = part.named_child(0) {
                        push_binding(ctx, &module, text(id, ctx.bytes), "*", "namespace");
                    }
                }
                "named_imports" => {
                    let mut c3 = part.walk();
                    for spec in part.children(&mut c3) {
                        if spec.kind() != "import_specifier" {
                            continue;
                        }
                        let orig = spec
                            .child_by_field_name("name")
                            .map(|n| text(n, ctx.bytes).to_string())
                            .unwrap_or_default();
                        let local = spec
                            .child_by_field_name("alias")
                            .map(|n| text(n, ctx.bytes).to_string())
                            .unwrap_or_else(|| orig.clone());
                        push_binding(ctx, &module, &local, &orig, "named");
                    }
                }
                _ => {}
            }
        }
    }
}

fn push_binding(ctx: &mut Ctx, module: &str, local: &str, imported: &str, kind: &str) {
    if local.is_empty() {
        return;
    }
    ctx.facts.imports.push(ImportBinding {
        file_path: ctx.file.to_string(),
        local_name: local.to_string(),
        source_module: module.to_string(),
        imported_name: imported.to_string(),
        kind: kind.to_string(),
    });
}

fn record_reexports(node: Node, ctx: &mut Ctx) {
    let src = node.child_by_field_name("source").or_else(|| {
        (0..node.named_child_count()).find_map(|i| {
            let n = node.named_child(i)?;
            if n.kind() == "string" {
                Some(n)
            } else {
                None
            }
        })
    });
    let Some(src) = src else {
        return;
    };
    let module = text(src, ctx.bytes).trim_matches(['"', '\'', '`']).to_string();
    if module.is_empty() {
        return;
    }
    let mut saw_named = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "export_clause" => {
                saw_named = true;
                let mut c2 = child.walk();
                for spec in child.children(&mut c2) {
                    if spec.kind() != "export_specifier" {
                        continue;
                    }
                    let orig = spec
                        .child_by_field_name("name")
                        .map(|n| text(n, ctx.bytes).to_string())
                        .unwrap_or_default();
                    let local = spec
                        .child_by_field_name("alias")
                        .map(|n| text(n, ctx.bytes).to_string())
                        .unwrap_or_else(|| orig.clone());
                    push_binding(ctx, &module, &local, &orig, "named-reexport");
                }
            }
            "namespace_export" => {
                saw_named = true;
                if let Some(id) = child.named_child(0) {
                    push_binding(ctx, &module, text(id, ctx.bytes), "*", "namespace-reexport");
                }
            }
            _ => {}
        }
    }
    if !saw_named {
        // `export * from './mod'`
        push_binding(ctx, &module, "*", "*", "star-reexport");
    }
}

/// Heritage edges (`extends` / `implements`) as AST extras. FileFacts stays stable.
pub fn extra_ast_edges(path_rel: &str, src: &str, facts: &FileFacts) -> Vec<Edge> {
    let mut out = Vec::new();
    let Some(lang) = Lang::from_path(path_rel) else {
        return out;
    };
    let Some(ts_lang) = language(lang) else {
        return out;
    };
    let mut parser = Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return out;
    }
    let Some(tree) = parser.parse(src, None) else {
        return out;
    };
    let bytes = src.as_bytes();
    walk_heritage(tree.root_node(), bytes, path_rel, facts, &mut out);
    out
}

fn walk_heritage(node: Node, bytes: &[u8], file: &str, facts: &FileFacts, out: &mut Vec<Edge>) {
    match node.kind() {
        "class_declaration" | "abstract_class_declaration" | "interface_declaration" => {
            if let Some(name) = node.child_by_field_name("name") {
                let nm = text(name, bytes);
                let start = node.start_position().row + 1;
                if let Some(src_sym) = facts.symbols.iter().find(|s| s.name == nm && s.start_line == start) {
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        let kind = match child.kind() {
                            "class_heritage" | "extends_clause" | "extends_type_clause" => Some("extends"),
                            "implements_clause" => Some("implements"),
                            _ => None,
                        };
                        if let Some(ekind) = kind {
                            collect_type_names(child, bytes, |raw, line| {
                                out.push(Edge {
                                    src_id: src_sym.id.clone(),
                                    dst_id: None,
                                    kind: ekind.into(),
                                    raw_name: raw.to_string(),
                                    resolved: false,
                                    conf: "weak".into(),
                                    reason: "unresolved".into(),
                                    provenance: "ast".into(),
                                    file_path: file.to_string(),
                                    line,
                                });
                            });
                        }
                    }
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_heritage(child, bytes, file, facts, out);
    }
}

fn collect_type_names(node: Node, bytes: &[u8], mut f: impl FnMut(&str, usize)) {
    fn rec(node: Node, bytes: &[u8], f: &mut impl FnMut(&str, usize)) {
        match node.kind() {
            "identifier" | "type_identifier" => {
                let t = text(node, bytes);
                if !t.is_empty() {
                    f(t, node.start_position().row + 1);
                }
            }
            "member_expression" => {
                let t = text(node, bytes);
                if !t.is_empty() {
                    f(t, node.start_position().row + 1);
                }
            }
            _ => {
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    rec(child, bytes, f);
                }
            }
        }
    }
    rec(node, bytes, &mut f);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_symbols_calls_imports() {
        let src = r#"
import { helper } from "./util";
export function greet(name: string) {
  return helper(name);
}
class Box {
  open() { return greet("x"); }
}
"#;
        let f = extract("src/a.ts", src);
        assert!(
            f.symbols.iter().any(|s| s.name == "greet" && s.exported && s.kind == "function"),
            "exported function greet"
        );
        assert!(f.symbols.iter().any(|s| s.name == "Box" && s.kind == "class"), "class Box");
        assert!(
            f.symbols.iter().any(|s| s.qualified_name == "Box.open" && s.kind == "method"),
            "method Box.open"
        );
        assert!(f.calls.iter().any(|c| c.name == "helper" && !c.member), "call helper");
        assert!(
            f.imports.iter().any(|b| b.local_name == "helper"
                && b.source_module == "./util"
                && b.kind == "named"),
            "named import helper"
        );
    }

    #[test]
    fn arrow_const_is_a_function_symbol() {
        let f = extract("src/a.ts", "export const add = (a: number, b: number) => a + b;");
        assert!(f.symbols.iter().any(|s| s.name == "add" && s.kind == "function" && s.exported));
    }

    #[test]
    fn star_reexport_binding() {
        let f = extract("src/lib/barrel.ts", "export * from \"./math.js\";");
        assert!(
            f.imports.iter().any(|b| b.kind == "star-reexport" && b.source_module.contains("math")),
            "star-reexport binding: {:?}",
            f.imports
        );
    }
}
