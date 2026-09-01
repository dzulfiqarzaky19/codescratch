//! tree-sitter-python → symbols + call sites + import bindings.
//! Recursive node walk, mirroring extract/mod.rs's TS/JS extractor (kind/field
//! API is stable across tree-sitter versions; avoids the version-fragile Query
//! API on purpose).

use super::walk::{self, push_binding, Ctx};
use crate::model::{Edge, FileFacts, RawCall, Symbol};
use tree_sitter::{Node, Parser};

pub fn extract(path: &str, src: &str) -> FileFacts {
    let mut facts = FileFacts::default();
    let ts_lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return facts;
    }
    let Some(tree) = parser.parse(src, None) else {
        return facts;
    };
    let bytes = src.as_bytes();
    let mut ctx = Ctx {
        file: path,
        bytes,
        enclosing: None,
        class: None,
        facts: &mut facts,
    };
    visit(tree.root_node(), &mut ctx);
    facts
}

fn signature(n: Node, bytes: &[u8]) -> String {
    let full = walk::text(n, bytes);
    let cut = full.find([':', '\n']).unwrap_or(full.len());
    let mut s = full[..cut].trim().to_string();
    if s.len() > 200 {
        s.truncate(200);
    }
    s
}

fn visit(node: Node, ctx: &mut Ctx) {
    match node.kind() {
        "function_definition" => {
            if let Some(name) = node.child_by_field_name("name") {
                // A def is a method iff it's directly nested in an enclosing
                // class's body (ctx.class set) and not further nested inside
                // another function within that class.
                let is_method = ctx.class.is_some() && ctx.enclosing.is_none();
                let (kind, qual) = if is_method {
                    let qual = ctx
                        .class
                        .as_ref()
                        .map(|(c, _)| format!("{c}.{}", walk::text(name, ctx.bytes)));
                    ("method", qual)
                } else {
                    ("function", None)
                };
                // Nested (non-method) functions and methods are not exported;
                // only module-scope defs are.
                let exported = ctx.enclosing.is_none() && ctx.class.is_none();
                let id = push_symbol(ctx, node, name, kind, qual, exported);
                // Descend into the body with this def as the new enclosing
                // symbol; class context is cleared so a def nested inside a
                // method's body isn't mistaken for another method.
                walk::recurse_with(node, ctx, Some(id), None, visit);
                return;
            }
        }
        "class_definition" => {
            if let Some(name) = node.child_by_field_name("name") {
                let nm = walk::text(name, ctx.bytes).to_string();
                let exported = ctx.enclosing.is_none() && ctx.class.is_none();
                let id = push_symbol(ctx, node, name, "class", None, exported);
                record_heritage(node, ctx, &id);
                // Descend with this class as context and no enclosing function,
                // so direct-child defs are recognized as methods.
                walk::recurse_with(node, ctx, None, Some((nm, id)), visit);
                return;
            }
        }
        "call" => {
            record_call(node, ctx);
            // fall through to recurse (nested calls in args / callee object)
        }
        "import_statement" => {
            record_import_statement(node, ctx);
            return;
        }
        "import_from_statement" => {
            record_import_from_statement(node, ctx);
            return;
        }
        _ => {}
    }
    // default: recurse over all children, context unchanged
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(child, ctx);
    }
}

fn push_symbol(
    ctx: &mut Ctx,
    def: Node,
    name: Node,
    kind: &str,
    qual: Option<String>,
    exported: bool,
) -> String {
    walk::push_symbol(
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
        "identifier" => (walk::text(callee, ctx.bytes).to_string(), false),
        "attribute" => match callee.child_by_field_name("attribute") {
            Some(p) => (walk::text(p, ctx.bytes).to_string(), true),
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

/// `import X`, `import X as Y`, `import X.Y`, `import X, Y as Z`.
/// Each entry is `dotted_name` or `aliased_import`, in a `name` field slot.
fn record_import_statement(node: Node, ctx: &mut Ctx) {
    let mut cursor = node.walk();
    for child in node.children_by_field_name("name", &mut cursor) {
        match child.kind() {
            "dotted_name" => {
                let module = walk::text(child, ctx.bytes).to_string();
                push_binding(ctx, &module, &module, "*", "namespace");
            }
            "aliased_import" => {
                let Some(name) = child.child_by_field_name("name") else {
                    continue;
                };
                let Some(alias) = child.child_by_field_name("alias") else {
                    continue;
                };
                let module = walk::text(name, ctx.bytes).to_string();
                let local = walk::text(alias, ctx.bytes).to_string();
                push_binding(ctx, &module, &local, "*", "namespace");
            }
            _ => {}
        }
    }
}

/// `from M import a, b as c`, `from M import (a, b)`, `from M import *`,
/// `from .relmod import x`.
fn record_import_from_statement(node: Node, ctx: &mut Ctx) {
    let Some(module_node) = node.child_by_field_name("module_name") else {
        return;
    };
    let module = walk::text(module_node, ctx.bytes).to_string();
    if module.is_empty() {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children_by_field_name("name", &mut cursor) {
        match child.kind() {
            "dotted_name" => {
                let nm = walk::text(child, ctx.bytes).to_string();
                push_binding(ctx, &module, &nm, &nm, "named");
            }
            "aliased_import" => {
                let Some(name) = child.child_by_field_name("name") else {
                    continue;
                };
                let Some(alias) = child.child_by_field_name("alias") else {
                    continue;
                };
                let orig = walk::text(name, ctx.bytes).to_string();
                let local = walk::text(alias, ctx.bytes).to_string();
                push_binding(ctx, &module, &local, &orig, "named");
            }
            _ => {}
        }
    }
    // `from M import *` — no `name`-field children to iterate; record explicitly.
    let mut cursor2 = node.walk();
    if node
        .children(&mut cursor2)
        .any(|c| c.kind() == "wildcard_import")
    {
        push_binding(ctx, &module, "*", "*", "star-reexport");
    }
}

/// `class Cat(Animal, pkg.Pet):` — superclasses live in the `superclasses` field.
fn record_heritage(node: Node, ctx: &mut Ctx, class_id: &str) {
    let Some(supers) = node.child_by_field_name("superclasses") else {
        return;
    };
    let mut cursor = supers.walk();
    for child in supers.children(&mut cursor) {
        let raw = match child.kind() {
            "identifier" | "attribute" | "dotted_name" => walk::text(child, ctx.bytes),
            _ => continue,
        };
        if raw.is_empty() {
            continue;
        }
        ctx.facts.heritage.push(Edge {
            src_id: class_id.to_string(),
            dst_id: None,
            kind: "extends".into(),
            raw_name: raw.to_string(),
            resolved: false,
            conf: "weak".into(),
            reason: "unresolved".into(),
            provenance: "ast".into(),
            file_path: ctx.file.to_string(),
            line: child.start_position().row + 1,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_module_function() {
        let src = r#"
def greet(name):
    return name
"#;
        let f = extract("src/a.py", src);
        assert!(
            f.symbols.iter().any(|s| s.name == "greet"
                && s.kind == "function"
                && s.exported
                && s.qualified_name == "greet"),
            "module-level function greet: {:?}",
            f.symbols
        );
    }

    #[test]
    fn class_heritage_is_collected_in_the_same_walk() {
        let src = "class Cat(Animal, Pet):\n    pass\n";
        let f = extract("src/a.py", src);
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
                .any(|e| e.kind == "extends" && e.raw_name == "Pet"),
            "extends Pet: {:?}",
            f.heritage
        );
    }

    #[test]
    fn class_and_method_qualified_name() {
        let src = r#"
class Box:
    def open(self):
        return 1
"#;
        let f = extract("src/a.py", src);
        assert!(
            f.symbols
                .iter()
                .any(|s| s.name == "Box" && s.kind == "class" && s.exported),
            "class Box: {:?}",
            f.symbols
        );
        assert!(
            f.symbols
                .iter()
                .any(|s| s.qualified_name == "Box.open" && s.kind == "method" && !s.exported),
            "method Box.open (not exported): {:?}",
            f.symbols
        );
    }

    #[test]
    fn calls_member_and_non_member() {
        let src = r#"
def run():
    helper()
    obj.method()
"#;
        let f = extract("src/a.py", src);
        assert!(
            f.calls.iter().any(|c| c.name == "helper" && !c.member),
            "non-member call helper: {:?}",
            f.calls
        );
        assert!(
            f.calls.iter().any(|c| c.name == "method" && c.member),
            "member call obj.method: {:?}",
            f.calls
        );
    }

    #[test]
    fn from_import_binding() {
        let src = "from pkg.util import helper as h, other\n";
        let f = extract("src/a.py", src);
        assert!(
            f.imports.iter().any(|b| b.local_name == "h"
                && b.imported_name == "helper"
                && b.source_module == "pkg.util"
                && b.kind == "named"),
            "aliased from-import: {:?}",
            f.imports
        );
        assert!(
            f.imports.iter().any(|b| b.local_name == "other"
                && b.imported_name == "other"
                && b.source_module == "pkg.util"
                && b.kind == "named"),
            "plain from-import: {:?}",
            f.imports
        );
    }

    #[test]
    fn plain_import_and_aliased_import() {
        let src = "import os\nimport numpy as np\n";
        let f = extract("src/a.py", src);
        assert!(
            f.imports.iter().any(|b| b.local_name == "os"
                && b.source_module == "os"
                && b.imported_name == "*"
                && b.kind == "namespace"),
            "plain import os: {:?}",
            f.imports
        );
        assert!(
            f.imports.iter().any(|b| b.local_name == "np"
                && b.source_module == "numpy"
                && b.imported_name == "*"
                && b.kind == "namespace"),
            "aliased import numpy as np: {:?}",
            f.imports
        );
    }
}
