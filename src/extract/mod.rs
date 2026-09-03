//! tree-sitter TS/JS → symbols + call sites + import bindings.
//! Recursive node walk (kind/field API is stable across tree-sitter versions);
//! avoids the version-fragile Query API on purpose.

use crate::model::{Edge, FileFacts, Lang, RawCall, Symbol};
use tree_sitter::{Node, Parser};

mod ast;
mod heuristics;
mod plugins;
mod python;

use ast::{push_binding, Ctx};

fn language(lang: Lang) -> Option<tree_sitter::Language> {
    Some(match lang {
        Lang::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Lang::Js => tree_sitter_javascript::LANGUAGE.into(),
        Lang::Py => return None, // python::extract owns the walk
    })
}

pub fn extract(path_rel: &str, src: &str) -> FileFacts {
    let mut facts = FileFacts::default();
    let Some(lang) = Lang::from_path(path_rel) else {
        return facts;
    };
    if lang == Lang::Py {
        facts = python::extract(path_rel, src);
    } else if let Some(ts_lang) = language(lang) {
        let mut parser = Parser::new();
        if parser.set_language(&ts_lang).is_ok() {
            if let Some(tree) = parser.parse(src, None) {
                let bytes = src.as_bytes();
                let mut ctx = Ctx {
                    file: path_rel,
                    bytes,
                    enclosing: None,
                    class: None,
                    facts: &mut facts,
                };
                visit(tree.root_node(), &mut ctx);
            }
        }
    }
    enrich(&mut facts, path_rel, src);
    facts
}

/// Heuristic edges + framework routes. Same call as the AST walk so index never
/// names those adapters.
fn enrich(facts: &mut FileFacts, path_rel: &str, src: &str) {
    facts.extra = heuristics::extra_edges(path_rel, src, facts);
    facts.routes = plugins::collect(path_rel, src);
}

fn is_exported(n: Node) -> bool {
    n.parent()
        .map(|p| p.kind() == "export_statement")
        .unwrap_or(false)
}

fn signature(n: Node, bytes: &[u8]) -> String {
    let full = ast::text(n, bytes);
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
                ast::recurse_with(node, ctx, Some(id), ctx.class.clone(), visit);
                return;
            }
        }
        "class_declaration" | "abstract_class_declaration" => {
            if let Some(name) = node.child_by_field_name("name") {
                let nm = ast::text(name, ctx.bytes).to_string();
                let id = push_symbol(ctx, node, name, "class", None);
                record_heritage(node, ctx, &id);
                ast::recurse_with(node, ctx, ctx.enclosing.clone(), Some((nm, id)), visit);
                return;
            }
        }
        "method_definition" => {
            if let Some(name) = node.child_by_field_name("name") {
                let qual = ctx
                    .class
                    .as_ref()
                    .map(|(c, _)| format!("{c}.{}", ast::text(name, ctx.bytes)));
                let id = push_symbol(ctx, node, name, "method", qual);
                ast::recurse_with(node, ctx, Some(id), ctx.class.clone(), visit);
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
                    .map(|v| {
                        matches!(
                            v.kind(),
                            "arrow_function" | "function" | "function_expression"
                        )
                    })
                    .unwrap_or(false);
                if value_is_fn {
                    if let Some(name) = decl.child_by_field_name("name") {
                        if name.kind() == "identifier" {
                            let id =
                                push_symbol_exported(ctx, decl, name, "function", None, exported);
                            ast::recurse_with(decl, ctx, Some(id), ctx.class.clone(), visit);
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

fn push_symbol(ctx: &mut Ctx, def: Node, name: Node, kind: &str, qual: Option<String>) -> String {
    push_symbol_exported(ctx, def, name, kind, qual, is_exported(def))
}

fn push_symbol_exported(
    ctx: &mut Ctx,
    def: Node,
    name: Node,
    kind: &str,
    qual: Option<String>,
    exported: bool,
) -> String {
    ast::push_symbol(
        ctx,
        def,
        name,
        kind,
        qual,
        exported,
        signature(def, ctx.bytes),
    )
}

fn record_call(node: Node, ctx: &mut Ctx) {
    let Some(callee) = node.child_by_field_name("function") else {
        return;
    };
    let (name, member) = match callee.kind() {
        "identifier" => (ast::text(callee, ctx.bytes).to_string(), false),
        "member_expression" => match callee.child_by_field_name("property") {
            Some(p) => (ast::text(p, ctx.bytes).to_string(), true),
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
    let module = ast::text(src, ctx.bytes)
        .trim_matches(['"', '\'', '`'])
        .to_string();
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
                "identifier" => push_binding(
                    ctx,
                    &module,
                    ast::text(part, ctx.bytes),
                    "default",
                    "default",
                ),
                "namespace_import" => {
                    if let Some(id) = part.named_child(0) {
                        push_binding(ctx, &module, ast::text(id, ctx.bytes), "*", "namespace");
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
                            .map(|n| ast::text(n, ctx.bytes).to_string())
                            .unwrap_or_default();
                        let local = spec
                            .child_by_field_name("alias")
                            .map(|n| ast::text(n, ctx.bytes).to_string())
                            .unwrap_or_else(|| orig.clone());
                        push_binding(ctx, &module, &local, &orig, "named");
                    }
                }
                _ => {}
            }
        }
    }
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
    let module = ast::text(src, ctx.bytes)
        .trim_matches(['"', '\'', '`'])
        .to_string();
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
                        .map(|n| ast::text(n, ctx.bytes).to_string())
                        .unwrap_or_default();
                    let local = spec
                        .child_by_field_name("alias")
                        .map(|n| ast::text(n, ctx.bytes).to_string())
                        .unwrap_or_else(|| orig.clone());
                    push_binding(ctx, &module, &local, &orig, "named-reexport");
                }
            }
            "namespace_export" => {
                saw_named = true;
                if let Some(id) = child.named_child(0) {
                    push_binding(
                        ctx,
                        &module,
                        ast::text(id, ctx.bytes),
                        "*",
                        "namespace-reexport",
                    );
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

/// Direct `extends` / `implements` clauses on this class node. Nested classes
/// hit this from their own visit arm — no second parse.
///
/// TS wraps both clauses in `class_heritage`; walk one level in so `implements`
/// is not lumped with `extends`.
fn record_heritage(node: Node, ctx: &mut Ctx, class_id: &str) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_heritage" => {
                let mut inner = child.walk();
                for gc in child.children(&mut inner) {
                    emit_heritage_clause(gc, ctx, class_id);
                }
            }
            _ => emit_heritage_clause(child, ctx, class_id),
        }
    }
}

fn emit_heritage_clause(node: Node, ctx: &mut Ctx, class_id: &str) {
    let kind = match node.kind() {
        "extends_clause" | "extends_type_clause" => "extends",
        "implements_clause" => "implements",
        _ => return,
    };
    collect_type_names(node, ctx.bytes, |raw, line| {
        ctx.facts.heritage.push(Edge {
            src_id: class_id.to_string(),
            dst_id: None,
            kind: kind.into(),
            raw_name: raw.to_string(),
            resolved: false,
            conf: "weak".into(),
            reason: "unresolved".into(),
            provenance: "ast".into(),
            file_path: ctx.file.to_string(),
            line,
        });
    });
}

fn collect_type_names(node: Node, bytes: &[u8], mut f: impl FnMut(&str, usize)) {
    fn rec(node: Node, bytes: &[u8], f: &mut impl FnMut(&str, usize)) {
        match node.kind() {
            "identifier" | "type_identifier" => {
                let t = ast::text(node, bytes);
                if !t.is_empty() {
                    f(t, node.start_position().row + 1);
                }
            }
            "member_expression" => {
                let t = ast::text(node, bytes);
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
            f.symbols
                .iter()
                .any(|s| s.name == "greet" && s.exported && s.kind == "function"),
            "exported function greet"
        );
        assert!(
            f.symbols
                .iter()
                .any(|s| s.name == "Box" && s.kind == "class"),
            "class Box"
        );
        assert!(
            f.symbols
                .iter()
                .any(|s| s.qualified_name == "Box.open" && s.kind == "method"),
            "method Box.open"
        );
        assert!(
            f.calls.iter().any(|c| c.name == "helper" && !c.member),
            "call helper"
        );
        assert!(
            f.imports.iter().any(|b| b.local_name == "helper"
                && b.source_module == "./util"
                && b.kind == "named"),
            "named import helper"
        );
    }

    #[test]
    fn arrow_const_is_a_function_symbol() {
        let f = extract(
            "src/a.ts",
            "export const add = (a: number, b: number) => a + b;",
        );
        assert!(f
            .symbols
            .iter()
            .any(|s| s.name == "add" && s.kind == "function" && s.exported));
    }

    #[test]
    fn star_reexport_binding() {
        let f = extract("src/lib/barrel.ts", "export * from \"./math.js\";");
        assert!(
            f.imports
                .iter()
                .any(|b| b.kind == "star-reexport" && b.source_module.contains("math")),
            "star-reexport binding: {:?}",
            f.imports
        );
    }

    #[test]
    fn class_heritage_is_collected_in_the_same_walk() {
        let src = "class Cat extends Animal implements Pet, Named {}";
        let f = extract("src/a.ts", src);
        let cat = f.symbols.iter().find(|s| s.name == "Cat").expect("Cat");
        assert!(
            f.heritage.iter().any(|e| {
                e.src_id == cat.id
                    && e.kind == "extends"
                    && e.raw_name == "Animal"
                    && e.provenance == "ast"
            }),
            "extends Animal: {:?}",
            f.heritage
        );
        assert!(
            f.heritage
                .iter()
                .any(|e| e.kind == "implements" && e.raw_name == "Pet"),
            "implements Pet: {:?}",
            f.heritage
        );
        assert!(
            f.heritage
                .iter()
                .any(|e| e.kind == "implements" && e.raw_name == "Named"),
            "implements Named: {:?}",
            f.heritage
        );
    }
}
