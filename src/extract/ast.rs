//! Shared tree walk: Ctx, recurse, push. Language visit arms stay adapters.

use crate::model::{FileFacts, ImportBinding, Symbol};
use tree_sitter::Node;

pub struct Ctx<'a> {
    pub file: &'a str,
    pub bytes: &'a [u8],
    pub enclosing: Option<String>, // nearest function/method symbol id
    pub class: Option<(String, String)>, // (class name, class id)
    pub facts: &'a mut FileFacts,
}

pub fn text<'a>(n: Node, bytes: &'a [u8]) -> &'a str {
    n.utf8_text(bytes).unwrap_or("")
}

pub fn recurse_with(
    node: Node,
    ctx: &mut Ctx,
    enclosing: Option<String>,
    class: Option<(String, String)>,
    visit: fn(Node, &mut Ctx),
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

pub fn push_symbol(
    ctx: &mut Ctx,
    def: Node,
    name: Node,
    kind: &str,
    qual: Option<String>,
    exported: bool,
    signature: String,
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
        signature,
    });
    id
}

pub fn push_binding(ctx: &mut Ctx, module: &str, local: &str, imported: &str, kind: &str) {
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
