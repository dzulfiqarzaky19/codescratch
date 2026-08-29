//! Core value types shared across extract → resolve → store → query.
//! Ported in spirit from codescratch `src/models.ts` — same honesty fields.

/// A named symbol (function / class / method / arrow-const).
#[derive(Debug, Clone)]
pub struct Symbol {
    /// Stable within a file version: `{file}#{name}@{start_line}`.
    pub id: String,
    pub kind: String, // function | class | method
    pub name: String,
    pub qualified_name: String, // Class.method, else == name
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub exported: bool,
    pub signature: String,
}

impl Symbol {
    pub fn make_id(file: &str, name: &str, start_line: usize) -> String {
        format!("{file}#{name}@{start_line}")
    }
    /// Pseudo-node for module-scope call sites (no enclosing symbol).
    pub fn module_id(file: &str) -> String {
        format!("{file}#<module>")
    }
}

/// An unresolved call site, captured at extract time.
#[derive(Debug, Clone)]
pub struct RawCall {
    pub from_id: String, // enclosing symbol id, or Symbol::module_id
    pub name: String,    // callee identifier or member property
    pub member: bool,    // true for `recv.name(...)`
    pub line: usize,
    pub file_path: String,
}

/// One import binding row — the substrate for `import-binding` resolution.
#[derive(Debug, Clone)]
pub struct ImportBinding {
    pub file_path: String,
    pub local_name: String,     // name in scope
    pub source_module: String,  // raw specifier, e.g. "./foo" or "react"
    pub imported_name: String,  // "default" | "*" | original export name
    pub kind: String,           // named | default | namespace
}

/// What one file yields when extracted.
pub struct FileFacts {
    pub symbols: Vec<Symbol>,
    pub calls: Vec<RawCall>,
    pub imports: Vec<ImportBinding>,
}

/// A resolved (or deliberately unresolved) graph edge.
/// `reason` + `conf` are the codescratch signature — never faked.
#[derive(Debug, Clone)]
pub struct Edge {
    pub src_id: String,
    pub dst_id: Option<String>,
    pub kind: String,       // calls | imports | contains | extends
    pub raw_name: String,
    pub resolved: bool,
    pub conf: String,       // strong | weak
    pub reason: String,     // same-file | import-binding | unique-global | receiver-unknown | ...
    pub provenance: String, // ast | heuristic
    pub file_path: String,
    pub line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Lang {
    Ts,
    Tsx,
    Js,
    Py,
}

impl Lang {
    pub fn as_str(self) -> &'static str {
        match self {
            Lang::Ts => "ts",
            Lang::Tsx => "tsx",
            Lang::Js => "js",
            Lang::Py => "py",
        }
    }
    pub fn from_path(path: &str) -> Option<Lang> {
        let p = path.to_ascii_lowercase();
        if p.ends_with(".ts") || p.ends_with(".mts") || p.ends_with(".cts") {
            Some(Lang::Ts)
        } else if p.ends_with(".tsx") {
            Some(Lang::Tsx)
        } else if p.ends_with(".js") || p.ends_with(".jsx") || p.ends_with(".mjs") || p.ends_with(".cjs") {
            Some(Lang::Js)
        } else if p.ends_with(".py") {
            Some(Lang::Py)
        } else {
            None
        }
    }
}

/// Edge/label provenance. `ast` = tree-sitter fact; `heuristic` = pattern guess,
/// always surfaced to the agent with a human label, never faked as AST.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Provenance {
    #[allow(dead_code)]
    Ast,
    Heuristic,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Provenance::Ast => "ast",
            Provenance::Heuristic => "heuristic",
        }
    }
}

/// A framework route materialized at index time (v0.3 route plugins → `route` node).
#[derive(Debug, Clone)]
pub struct RouteFact {
    pub method: String,   // GET | POST | ... | ANY
    pub path: String,     // "/users/:id"
    pub handler_id: String, // node id of the handler symbol
    pub file_path: String,
    pub line: usize,
}

/// A precomputed call chain entrypoint→leaf (v0.4 → `process` node + `step_in` edges).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ProcessFact {
    pub name: String,
    pub steps: Vec<String>, // ordered node ids
}

/// A graph community (v0.4 Leiden → `community` node + `member_of` edges).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Community {
    pub id: String,
    pub label: String,
    pub members: Vec<String>, // node ids
}
