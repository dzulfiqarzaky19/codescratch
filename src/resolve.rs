//! Edge resolution with honest precedence. Every edge carries `reason` + `conf`.
//!
//! Precedence per call site:
//!   1. import-binding  (strong)
//!   2. same-file       (strong)
//!   3. receiver-unknown(weak)
//!   4. unique-global   (weak)
//!   5. unresolved      (weak) — kept open, never faked
//!
//! Heritage (`extends` / `implements`) uses import-binding first, then unique-global
//! before same-file (the historical heritage order).
//!
//! Module resolution (specifier → file) lives in [`crate::module`].

use crate::model::{Edge, ImportBinding, RawCall, Symbol};
use crate::module::{self, ResolveConfig};
use std::collections::{HashMap, HashSet};

/// Back-compat entry: resolve with the default (empty) config. (used by tests + WP-2B)
#[allow(dead_code)]
pub fn resolve(
    symbols: &[Symbol],
    calls: &[RawCall],
    bindings: &[ImportBinding],
    files: &HashSet<String>,
) -> Vec<Edge> {
    resolve_with(symbols, calls, bindings, files, &ResolveConfig::default())
}

/// Full entry, threaded with module-resolution config.
pub fn resolve_with(
    symbols: &[Symbol],
    calls: &[RawCall],
    bindings: &[ImportBinding],
    files: &HashSet<String>,
    cfg: &ResolveConfig,
) -> Vec<Edge> {
    resolve_with_heritage(symbols, calls, bindings, &[], files, cfg)
}

/// Same as [`resolve_with`], plus heritage edges (`extends` / `implements`)
/// resolved with the same honesty precedence as calls.
pub fn resolve_with_heritage(
    symbols: &[Symbol],
    calls: &[RawCall],
    bindings: &[ImportBinding],
    heritage: &[Edge],
    files: &HashSet<String>,
    cfg: &ResolveConfig,
) -> Vec<Edge> {
    let mut global: HashMap<&str, Vec<&Symbol>> = HashMap::new();
    let mut per_file: HashMap<&str, HashMap<&str, Vec<&Symbol>>> = HashMap::new();
    for s in symbols {
        global.entry(s.name.as_str()).or_default().push(s);
        per_file
            .entry(s.file_path.as_str())
            .or_default()
            .entry(s.name.as_str())
            .or_default()
            .push(s);
    }
    let mut binds: HashMap<&str, HashMap<&str, &ImportBinding>> = HashMap::new();
    let mut binds_by_file: HashMap<&str, Vec<&ImportBinding>> = HashMap::new();
    for b in bindings {
        binds
            .entry(b.file_path.as_str())
            .or_default()
            .insert(b.local_name.as_str(), b);
        binds_by_file
            .entry(b.file_path.as_str())
            .or_default()
            .push(b);
    }

    let mut edges: Vec<Edge> = Vec::new();

    // --- contains edges: class -> method ---
    for s in symbols {
        if s.kind == "method" {
            if let Some((class_name, _)) = s.qualified_name.split_once('.') {
                if let Some(cands) = per_file
                    .get(s.file_path.as_str())
                    .and_then(|m| m.get(class_name))
                {
                    if let Some(class) = cands.iter().find(|c| c.kind == "class") {
                        edges.push(Edge {
                            src_id: class.id.clone(),
                            dst_id: Some(s.id.clone()),
                            kind: "contains".into(),
                            raw_name: s.name.clone(),
                            resolved: true,
                            conf: "strong".into(),
                            reason: "same-file".into(),
                            provenance: "ast".into(),
                            file_path: s.file_path.clone(),
                            line: s.start_line,
                        });
                    }
                }
            }
        }
    }

    // --- call edges ---
    for c in calls {
        let mut e = Edge {
            src_id: c.from_id.clone(),
            dst_id: None,
            kind: "calls".into(),
            raw_name: c.name.clone(),
            resolved: false,
            conf: "weak".into(),
            reason: "unresolved".into(),
            provenance: "ast".into(),
            file_path: c.file_path.clone(),
            line: c.line,
        };

        // 1. import-binding (relative / alias / workspace / barrel)
        if !c.member {
            if let Some(b) = binds
                .get(c.file_path.as_str())
                .and_then(|m| m.get(c.name.as_str()))
            {
                match module::resolve_module(&c.file_path, &b.source_module, files, cfg) {
                    Some(target) => {
                        let want = if b.imported_name == "default" {
                            "default"
                        } else {
                            b.imported_name.as_str()
                        };
                        if let Some(dst) =
                            resolve_export(&per_file, &binds_by_file, files, cfg, &target, want, 0)
                        {
                            e.dst_id = Some(dst.id.clone());
                            e.resolved = true;
                            e.conf = "strong".into();
                            e.reason = "import-binding".into();
                            edges.push(e);
                            continue;
                        }
                    }
                    None if b.source_module.starts_with('.') => {}
                    None => {
                        e.reason = "external-import".into();
                        e.conf = "strong".into();
                        edges.push(e);
                        continue;
                    }
                }
            }
        }

        // 2. same-file (non-method, unique)
        if let Some(cands) = per_file
            .get(c.file_path.as_str())
            .and_then(|m| m.get(c.name.as_str()))
        {
            let non_method: Vec<&&Symbol> = cands.iter().filter(|s| s.kind != "method").collect();
            if non_method.len() == 1 {
                e.dst_id = Some(non_method[0].id.clone());
                e.resolved = true;
                e.conf = "strong".into();
                e.reason = "same-file".into();
                edges.push(e);
                continue;
            }
        }

        // 3. receiver-unknown (member call: navigational only)
        if c.member {
            if let Some(cands) = global.get(c.name.as_str()) {
                e.reason = "receiver-unknown".into();
                if cands.len() == 1 {
                    e.dst_id = Some(cands[0].id.clone());
                    e.resolved = true; // navigational — still weak
                }
            } else {
                e.reason = "receiver-unknown".into();
            }
            edges.push(e);
            continue;
        }

        // 4. unique-global
        if let Some(cands) = global.get(c.name.as_str()) {
            if cands.len() == 1 {
                e.dst_id = Some(cands[0].id.clone());
                e.resolved = true;
                e.reason = "unique-global".into();
                edges.push(e);
                continue;
            }
        }

        // 5. unresolved
        edges.push(e);
    }

    for h in heritage {
        if h.kind != "extends" && h.kind != "implements" {
            edges.push(h.clone());
            continue;
        }
        let mut e = h.clone();
        let simple = e.raw_name.rsplit('.').next().unwrap_or(&e.raw_name);
        let lookup = if e.raw_name.contains('.') {
            simple
        } else {
            e.raw_name.as_str()
        };

        // 1. import-binding (local name of the type, then last segment of a dotted name)
        if let Some(b) = binds
            .get(e.file_path.as_str())
            .and_then(|m| m.get(e.raw_name.as_str()).or_else(|| m.get(simple)))
        {
            match module::resolve_module(&e.file_path, &b.source_module, files, cfg) {
                Some(target) => {
                    let want = if b.imported_name == "default" {
                        "default"
                    } else {
                        b.imported_name.as_str()
                    };
                    if let Some(dst) =
                        resolve_export(&per_file, &binds_by_file, files, cfg, &target, want, 0)
                    {
                        e.dst_id = Some(dst.id.clone());
                        e.resolved = true;
                        e.conf = "strong".into();
                        e.reason = "import-binding".into();
                        edges.push(e);
                        continue;
                    }
                }
                None if b.source_module.starts_with('.') => {}
                None => {
                    e.reason = "external-import".into();
                    e.conf = "strong".into();
                    edges.push(e);
                    continue;
                }
            }
        }

        // Preserve the previous heritage order: unique-global before same-file.
        // (Call edges prefer same-file; heritage used unique-global first.)
        if let Some(cands) = global.get(lookup) {
            if cands.len() == 1 {
                e.dst_id = Some(cands[0].id.clone());
                e.resolved = true;
                e.conf = "weak".into();
                e.reason = "unique-global".into();
                edges.push(e);
                continue;
            }
            if let Some(same) = cands.iter().find(|s| s.file_path == e.file_path) {
                e.dst_id = Some(same.id.clone());
                e.resolved = true;
                e.conf = "strong".into();
                e.reason = "same-file".into();
                edges.push(e);
                continue;
            }
        }

        edges.push(e);
    }

    edges
}

fn resolve_export<'a>(
    per_file: &'a HashMap<&'a str, HashMap<&'a str, Vec<&'a Symbol>>>,
    binds_by_file: &HashMap<&str, Vec<&ImportBinding>>,
    files: &HashSet<String>,
    cfg: &ResolveConfig,
    module_file: &str,
    imported_name: &str,
    depth: usize,
) -> Option<&'a Symbol> {
    if depth > 6 {
        return None;
    }
    if imported_name == "default" {
        if let Some(s) = pick_in_file(per_file, module_file, None) {
            return Some(s);
        }
    } else if let Some(s) = pick_in_file(per_file, module_file, Some(imported_name)) {
        return Some(s);
    }

    let Some(binds) = binds_by_file.get(module_file) else {
        return None;
    };

    for b in binds {
        if b.kind == "named-reexport"
            && (b.local_name == imported_name || b.imported_name == imported_name)
        {
            if let Some(target) = module::resolve_module(module_file, &b.source_module, files, cfg)
            {
                let want = if b.imported_name == "default" {
                    "default"
                } else {
                    b.imported_name.as_str()
                };
                if let Some(s) = resolve_export(
                    per_file,
                    binds_by_file,
                    files,
                    cfg,
                    &target,
                    want,
                    depth + 1,
                ) {
                    return Some(s);
                }
            }
        }
    }
    for b in binds {
        if b.kind == "star-reexport" {
            if let Some(target) = module::resolve_module(module_file, &b.source_module, files, cfg)
            {
                if let Some(s) = resolve_export(
                    per_file,
                    binds_by_file,
                    files,
                    cfg,
                    &target,
                    imported_name,
                    depth + 1,
                ) {
                    return Some(s);
                }
            }
        }
    }
    None
}

fn pick_in_file<'a>(
    per_file: &'a HashMap<&'a str, HashMap<&'a str, Vec<&'a Symbol>>>,
    file: &str,
    want: Option<&str>,
) -> Option<&'a Symbol> {
    let by_name = per_file.get(file)?;
    match want {
        Some(name) => by_name.get(name).and_then(|v| v.first()).copied(),
        None => {
            let exported: Vec<&Symbol> = by_name
                .values()
                .flatten()
                .filter(|s| s.exported)
                .copied()
                .collect();
            if exported.len() == 1 {
                Some(exported[0])
            } else {
                by_name.get("default").and_then(|v| v.first()).copied()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ImportBinding, RawCall, Symbol};
    use crate::module::{self, ResolveConfig};

    fn sym(id: &str, name: &str, file: &str, kind: &str, exported: bool) -> Symbol {
        Symbol {
            id: id.into(),
            kind: kind.into(),
            name: name.into(),
            qualified_name: name.into(),
            file_path: file.into(),
            start_line: 1,
            end_line: 2,
            exported,
            signature: String::new(),
        }
    }
    fn call(from: &str, name: &str, member: bool, file: &str) -> RawCall {
        RawCall {
            from_id: from.into(),
            name: name.into(),
            member,
            line: 1,
            file_path: file.into(),
        }
    }
    fn call_edge(edges: &[Edge]) -> &Edge {
        edges
            .iter()
            .find(|e| e.kind == "calls")
            .expect("a call edge")
    }

    #[test]
    fn same_file_beats_global() {
        let syms = vec![
            sym("a.ts#foo@1", "foo", "a.ts", "function", false),
            sym("a.ts#c@1", "c", "a.ts", "function", false),
        ];
        let calls = vec![call("a.ts#c@1", "foo", false, "a.ts")];
        let e = &resolve(&syms, &calls, &[], &HashSet::new())[..];
        let e = call_edge(e);
        assert_eq!(e.reason, "same-file");
        assert_eq!(e.conf, "strong");
        assert_eq!(e.dst_id.as_deref(), Some("a.ts#foo@1"));
    }

    #[test]
    fn import_binding_resolves_cross_file() {
        let syms = vec![
            sym("util.ts#helper@1", "helper", "util.ts", "function", true),
            sym("a.ts#c@1", "c", "a.ts", "function", false),
        ];
        let calls = vec![call("a.ts#c@1", "helper", false, "a.ts")];
        let binds = vec![ImportBinding {
            file_path: "a.ts".into(),
            local_name: "helper".into(),
            source_module: "./util".into(),
            imported_name: "helper".into(),
            kind: "named".into(),
        }];
        let mut files = HashSet::new();
        files.insert("util.ts".to_string());
        files.insert("a.ts".to_string());
        let edges = resolve(&syms, &calls, &binds, &files);
        let e = call_edge(&edges);
        assert_eq!(e.reason, "import-binding");
        assert_eq!(e.conf, "strong");
        assert_eq!(e.dst_id.as_deref(), Some("util.ts#helper@1"));
    }

    #[test]
    fn receiver_unknown_is_weak() {
        let syms = vec![
            sym("a.ts#save@1", "save", "a.ts", "method", false),
            sym("a.ts#c@1", "c", "a.ts", "function", false),
        ];
        let calls = vec![call("a.ts#c@1", "save", true, "a.ts")];
        let edges = resolve(&syms, &calls, &[], &HashSet::new());
        let e = call_edge(&edges);
        assert_eq!(e.reason, "receiver-unknown");
        assert_eq!(e.conf, "weak");
    }

    #[test]
    fn unresolved_stays_open_never_faked() {
        let syms = vec![sym("a.ts#c@1", "c", "a.ts", "function", false)];
        let calls = vec![call("a.ts#c@1", "mystery", false, "a.ts")];
        let edges = resolve(&syms, &calls, &[], &HashSet::new());
        let e = call_edge(&edges);
        assert_eq!(e.reason, "unresolved");
        assert!(!e.resolved);
    }

    #[test]
    fn contains_edge_links_class_to_method() {
        let syms = vec![
            sym("a.ts#Box@1", "Box", "a.ts", "class", true),
            Symbol {
                qualified_name: "Box.open".into(),
                ..sym("a.ts#open@2", "open", "a.ts", "method", false)
            },
        ];
        let edges = resolve(&syms, &[], &[], &HashSet::new());
        assert!(edges.iter().any(|e| e.kind == "contains"
            && e.src_id == "a.ts#Box@1"
            && e.dst_id.as_deref() == Some("a.ts#open@2")));
    }

    #[test]
    fn relative_resolution_finds_index() {
        let mut files = HashSet::new();
        files.insert("src/util/index.ts".to_string());
        assert_eq!(
            module::resolve_relative("src/a.ts", "./util", &files).as_deref(),
            Some("src/util/index.ts")
        );
    }

    #[test]
    fn normalize_collapses_dotdot() {
        assert_eq!(module::normalize("src/foo/../bar"), "src/bar");
    }

    #[test]
    fn js_specifier_remaps_to_ts() {
        let mut files = HashSet::new();
        files.insert("src/lib/math.ts".to_string());
        assert_eq!(
            module::resolve_relative("src/a.ts", "./lib/math.js", &files).as_deref(),
            Some("src/lib/math.ts")
        );
    }

    #[test]
    fn alias_path_resolves() {
        let mut cfg = ResolveConfig::default();
        cfg.tsconfig_paths
            .insert("@/*".into(), vec!["src/*".into()]);
        let mut files = HashSet::new();
        files.insert("src/lib/math.ts".into());
        files.insert("src/a.ts".into());
        assert_eq!(
            module::resolve_module("src/a.ts", "@/lib/math.js", &files, &cfg).as_deref(),
            Some("src/lib/math.ts")
        );
    }

    #[test]
    fn workspace_pkg_resolves_src_index() {
        let mut cfg = ResolveConfig::default();
        cfg.workspace_pkgs
            .insert("@medium/core".into(), "packages/core".into());
        let mut files = HashSet::new();
        files.insert("packages/core/src/index.ts".into());
        assert_eq!(
            module::resolve_module("src/a.ts", "@medium/core", &files, &cfg).as_deref(),
            Some("packages/core/src/index.ts")
        );
    }

    #[test]
    fn star_reexport_barrel_follows_to_source() {
        let mut files = HashSet::new();
        files.insert("src/lib/math.ts".into());
        files.insert("src/lib/barrel.ts".into());
        files.insert("src/a.ts".into());
        let syms = vec![
            sym(
                "src/lib/math.ts#add@1",
                "add",
                "src/lib/math.ts",
                "function",
                true,
            ),
            sym("src/a.ts#c@1", "c", "src/a.ts", "function", false),
        ];
        let calls = vec![call("src/a.ts#c@1", "plus", false, "src/a.ts")];
        let binds = vec![
            ImportBinding {
                file_path: "src/lib/barrel.ts".into(),
                local_name: "*".into(),
                source_module: "./math.js".into(),
                imported_name: "*".into(),
                kind: "star-reexport".into(),
            },
            ImportBinding {
                file_path: "src/a.ts".into(),
                local_name: "plus".into(),
                source_module: "./lib/barrel.js".into(),
                imported_name: "add".into(),
                kind: "named".into(),
            },
        ];
        let edges = resolve(&syms, &calls, &binds, &files);
        let e = call_edge(&edges);
        assert_eq!(e.reason, "import-binding");
        assert_eq!(e.conf, "strong");
        assert_eq!(e.dst_id.as_deref(), Some("src/lib/math.ts#add@1"));
    }

    #[test]
    fn alias_import_binding_is_strong() {
        let mut cfg = ResolveConfig::default();
        cfg.tsconfig_paths
            .insert("@/*".into(), vec!["src/*".into()]);
        let mut files = HashSet::new();
        files.insert("src/lib/math.ts".into());
        files.insert("src/a.ts".into());
        let syms = vec![
            sym(
                "src/lib/math.ts#add@1",
                "add",
                "src/lib/math.ts",
                "function",
                true,
            ),
            sym("src/a.ts#c@1", "c", "src/a.ts", "function", false),
        ];
        let calls = vec![call("src/a.ts#c@1", "sum", false, "src/a.ts")];
        let binds = vec![ImportBinding {
            file_path: "src/a.ts".into(),
            local_name: "sum".into(),
            source_module: "@/lib/math.js".into(),
            imported_name: "add".into(),
            kind: "named".into(),
        }];
        let edges = resolve_with(&syms, &calls, &binds, &files, &cfg);
        let e = call_edge(&edges);
        assert_eq!(e.reason, "import-binding");
        assert_eq!(e.dst_id.as_deref(), Some("src/lib/math.ts#add@1"));
    }

    fn heritage(src: &str, raw: &str, file: &str) -> Edge {
        Edge {
            src_id: src.into(),
            dst_id: None,
            kind: "extends".into(),
            raw_name: raw.into(),
            resolved: false,
            conf: "weak".into(),
            reason: "unresolved".into(),
            provenance: "ast".into(),
            file_path: file.into(),
            line: 1,
        }
    }

    fn heritage_edge(edges: &[Edge]) -> &Edge {
        edges
            .iter()
            .find(|e| e.kind == "extends")
            .expect("an extends edge")
    }

    #[test]
    fn heritage_unique_global_beats_same_file_when_unique() {
        let files = HashSet::from(["a.ts".into()]);
        let edges = resolve_with_heritage(
            &[sym("a.ts#Animal@1", "Animal", "a.ts", "class", true)],
            &[],
            &[],
            &[heritage("a.ts#Cat@1", "Animal", "a.ts")],
            &files,
            &ResolveConfig::default(),
        );
        let e = heritage_edge(&edges);
        assert_eq!(e.reason, "unique-global");
        assert_eq!(e.conf, "weak");
        assert_eq!(e.dst_id.as_deref(), Some("a.ts#Animal@1"));
    }

    #[test]
    fn heritage_same_file_when_name_is_not_unique() {
        let files = HashSet::from(["a.ts".into(), "b.ts".into()]);
        let edges = resolve_with_heritage(
            &[
                sym("a.ts#Animal@1", "Animal", "a.ts", "class", true),
                sym("b.ts#Animal@1", "Animal", "b.ts", "class", true),
            ],
            &[],
            &[],
            &[heritage("a.ts#Cat@1", "Animal", "a.ts")],
            &files,
            &ResolveConfig::default(),
        );
        let e = heritage_edge(&edges);
        assert_eq!(e.reason, "same-file");
        assert_eq!(e.conf, "strong");
        assert_eq!(e.dst_id.as_deref(), Some("a.ts#Animal@1"));
    }

    #[test]
    fn heritage_import_binding_beats_unique_global() {
        let files = HashSet::from(["a.ts".into(), "lib.ts".into()]);
        let binds = vec![ImportBinding {
            file_path: "a.ts".into(),
            local_name: "Animal".into(),
            source_module: "./lib".into(),
            imported_name: "Animal".into(),
            kind: "named".into(),
        }];
        let edges = resolve_with_heritage(
            &[
                sym("lib.ts#Animal@1", "Animal", "lib.ts", "class", true),
                // a same-named local class would previously unique-global-miss;
                // import-binding must still win.
            ],
            &[],
            &binds,
            &[heritage("a.ts#Cat@1", "Animal", "a.ts")],
            &files,
            &ResolveConfig::default(),
        );
        let e = heritage_edge(&edges);
        assert_eq!(e.reason, "import-binding");
        assert_eq!(e.conf, "strong");
        assert_eq!(e.dst_id.as_deref(), Some("lib.ts#Animal@1"));
    }
}
