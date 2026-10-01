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
//! Specifier → file (relative, tsconfig paths, workspace packages) lives in this
//! same module: it is how `import-binding` is decided, not a second behaviour.

use crate::model::{Edge, ImportBinding, RawCall, RawRef, Symbol};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Module-resolution config. Empty = relative + `index` only.
#[derive(Debug, Default, Clone)]
pub struct ResolveConfig {
    pub tsconfig_paths: HashMap<String, Vec<String>>, // alias glob -> targets
    pub base_url: Option<String>,
    pub workspace_pkgs: HashMap<String, String>, // pkg name -> dir
}

/// Load tsconfig `paths`/`baseUrl`(+`extends`) and workspace package names.
pub fn load_config(root: &Path, _files: &HashSet<String>) -> ResolveConfig {
    let mut cfg = ResolveConfig::default();
    load_tsconfig(root, "tsconfig.json", &mut cfg, 0);
    load_workspace_pkgs(root, &mut cfg);
    cfg
}

/// Heritage edges (`extends` / `implements`) resolved with the same honesty
/// precedence as calls. Pass `heritage = &[]` when there are none.
pub fn resolve_with_heritage(
    symbols: &[Symbol],
    calls: &[RawCall],
    bindings: &[ImportBinding],
    heritage: &[Edge],
    files: &HashSet<String>,
    cfg: &ResolveConfig,
) -> Vec<Edge> {
    let (global, per_file) = symbol_maps(symbols.iter());
    // Call sites never target a type: `const User` + `type User` must stay unique.
    let (call_global, call_per_file) =
        symbol_maps(symbols.iter().filter(|s| !is_type_only(&s.kind)));
    let (binds, binds_by_file) = bind_maps(bindings);

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
                match resolve_module(&c.file_path, &b.source_module, files, cfg) {
                    Some(target) => {
                        let want = if b.imported_name == "default" {
                            "default"
                        } else {
                            b.imported_name.as_str()
                        };
                        if let Some(dst) =
                            resolve_export(&call_per_file, &binds_by_file, files, cfg, &target, want, 0)
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
        if let Some(cands) = call_per_file
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
            if let Some(cands) = call_global.get(c.name.as_str()) {
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
        if let Some(cands) = call_global.get(c.name.as_str()) {
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
            match resolve_module(&e.file_path, &b.source_module, files, cfg) {
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

/// `references` edges. Only the two strong reasons apply: the name is an import
/// binding of the file, or a declaration in it. Anything else is a local or a
/// global, and yields no edge rather than a guess.
pub fn resolve_refs(
    symbols: &[Symbol],
    refs: &[RawRef],
    bindings: &[ImportBinding],
    files: &HashSet<String>,
    cfg: &ResolveConfig,
) -> Vec<Edge> {
    let (_, per_file) = symbol_maps(symbols.iter());
    let (binds, binds_by_file) = bind_maps(bindings);

    let mut seen: HashSet<(&str, &str, usize)> = HashSet::new();
    let mut edges = Vec::new();
    for r in refs {
        let imported = binds
            .get(r.file_path.as_str())
            .and_then(|m| m.get(r.name.as_str()))
            .and_then(|b| {
                let target = resolve_module(&r.file_path, &b.source_module, files, cfg)?;
                let want = if b.imported_name == "default" {
                    "default"
                } else {
                    b.imported_name.as_str()
                };
                resolve_export(&per_file, &binds_by_file, files, cfg, &target, want, 0)
            })
            .map(|s| (s, "import-binding"));
        let local = || {
            per_file
                .get(r.file_path.as_str())
                .and_then(|m| m.get(r.name.as_str()))
                .and_then(|c| c.iter().find(|s| s.kind != "method"))
                .map(|s| (*s, "same-file"))
        };
        let Some((dst, reason)) = imported.or_else(local) else {
            continue;
        };
        if dst.id == r.from_id || !seen.insert((r.from_id.as_str(), dst.id.as_str(), r.line)) {
            continue;
        }
        edges.push(Edge {
            src_id: r.from_id.clone(),
            dst_id: Some(dst.id.clone()),
            kind: "references".into(),
            raw_name: r.name.clone(),
            resolved: true,
            conf: "strong".into(),
            reason: reason.into(),
            provenance: "ast".into(),
            file_path: r.file_path.clone(),
            line: r.line,
        });
    }
    edges
}

type BindMaps<'a> = (
    HashMap<&'a str, HashMap<&'a str, &'a ImportBinding>>,
    HashMap<&'a str, Vec<&'a ImportBinding>>,
);

/// `(file → local name → binding, file → bindings)`.
fn bind_maps(bindings: &[ImportBinding]) -> BindMaps<'_> {
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
    (binds, binds_by_file)
}

type SymbolMaps<'a> = (
    HashMap<&'a str, Vec<&'a Symbol>>,
    HashMap<&'a str, HashMap<&'a str, Vec<&'a Symbol>>>,
);

/// `(by name, by file → by name)` over `symbols`.
fn symbol_maps<'a>(symbols: impl Iterator<Item = &'a Symbol>) -> SymbolMaps<'a> {
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
    (global, per_file)
}

/// Kinds that exist only at the type level (or can never be called).
fn is_type_only(kind: &str) -> bool {
    matches!(kind, "interface" | "type" | "enum")
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
            if let Some(target) = resolve_module(module_file, &b.source_module, files, cfg)
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
    // `import { X } from "./a"; export { X };` — the module hands on what it imported.
    for b in binds {
        if matches!(b.kind.as_str(), "named" | "default") && b.local_name == imported_name {
            if let Some(target) = resolve_module(module_file, &b.source_module, files, cfg)
            {
                if let Some(s) = resolve_export(
                    per_file,
                    binds_by_file,
                    files,
                    cfg,
                    &target,
                    b.imported_name.as_str(),
                    depth + 1,
                ) {
                    return Some(s);
                }
            }
        }
    }
    for b in binds {
        if b.kind == "star-reexport" {
            if let Some(target) = resolve_module(module_file, &b.source_module, files, cfg)
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
                // Sole exported function/class stands in for `default`; exported
                // types and plain consts beside it must not break that guess.
                .filter(|s| s.exported && matches!(s.kind.as_str(), "function" | "class"))
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

/// Resolve any specifier: relative, alias, baseUrl, workspace package.
fn resolve_module(
    from_file: &str,
    spec: &str,
    files: &HashSet<String>,
    cfg: &ResolveConfig,
) -> Option<String> {
    if spec.starts_with('.') || spec.starts_with('/') {
        return resolve_relative(from_file, spec, files);
    }
    if let Some(h) = resolve_alias(spec, cfg, files) {
        return Some(h);
    }
    if let Some(b) = &cfg.base_url {
        let b = b.trim_end_matches('/');
        let candidate = if b.is_empty() || b == "." {
            spec.to_string()
        } else {
            format!("{b}/{spec}")
        };
        if let Some(h) = hit_file(&candidate, files) {
            return Some(h);
        }
    }
    resolve_package(spec, cfg, files)
}

fn resolve_alias(spec: &str, cfg: &ResolveConfig, files: &HashSet<String>) -> Option<String> {
    let mut aliases: Vec<(&String, &Vec<String>)> = cfg.tsconfig_paths.iter().collect();
    aliases.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    for (pattern, targets) in aliases {
        let star = pattern.ends_with("/*");
        let prefix = if star {
            &pattern[..pattern.len() - 1] // "@/*" → "@/"
        } else {
            pattern.as_str()
        };
        let rest: &str = if star {
            if !spec.starts_with(prefix) {
                continue;
            }
            &spec[prefix.len()..]
        } else if spec == prefix {
            ""
        } else if let Some(r) = spec.strip_prefix(prefix).and_then(|s| s.strip_prefix('/')) {
            r
        } else {
            continue;
        };
        for target0 in targets {
            let mut target = target0.clone();
            if star && target.ends_with("/*") {
                target.truncate(target.len() - 2);
            } else if star && target.ends_with('*') {
                target.pop();
            }
            target = target
                .trim_start_matches("./")
                .trim_end_matches('/')
                .to_string();
            if let Some(b) = &cfg.base_url {
                let b = b.trim_end_matches('/');
                if !b.is_empty() && b != "." && !target.starts_with('/') {
                    target = format!("{b}/{target}");
                }
            }
            let candidate = if rest.is_empty() {
                target
            } else if target.is_empty() {
                rest.to_string()
            } else {
                format!("{target}/{rest}")
            };
            if let Some(h) = hit_file(&candidate, files) {
                return Some(h);
            }
        }
    }
    None
}

fn resolve_package(spec: &str, cfg: &ResolveConfig, files: &HashSet<String>) -> Option<String> {
    let (pkg, sub) = split_pkg(spec);
    let dir = cfg.workspace_pkgs.get(&pkg)?;
    if sub.is_empty() {
        for c in ["src/index", "index", "dist/index", "lib/index"] {
            let cand = if dir == "." {
                c.to_string()
            } else {
                format!("{dir}/{c}")
            };
            if let Some(h) = hit_file(&cand, files) {
                return Some(h);
            }
        }
        None
    } else {
        let cand = if dir == "." {
            sub
        } else {
            format!("{dir}/{sub}")
        };
        hit_file(&cand, files)
    }
}

fn split_pkg(spec: &str) -> (String, String) {
    if spec.starts_with('@') {
        let mut parts = spec.splitn(3, '/');
        let scope = parts.next().unwrap_or("");
        let name = parts.next().unwrap_or("");
        let sub = parts.next().unwrap_or("").to_string();
        (format!("{scope}/{name}"), sub)
    } else if let Some(i) = spec.find('/') {
        (spec[..i].to_string(), spec[i + 1..].to_string())
    } else {
        (spec.to_string(), String::new())
    }
}

/// Relative + `index` resolution against the known file set. Remaps `.js` → `.ts`.
fn resolve_relative(from_file: &str, spec: &str, files: &HashSet<String>) -> Option<String> {
    let dir = match from_file.rfind('/') {
        Some(i) => &from_file[..i],
        None => "",
    };
    let joined = if dir.is_empty() {
        spec.trim_start_matches("./").to_string()
    } else {
        format!("{dir}/{spec}")
    };
    hit_file(&normalize(&joined), files)
}

fn hit_file(rel: &str, files: &HashSet<String>) -> Option<String> {
    let rel = normalize(rel.trim_start_matches("./"));
    if files.contains(&rel) {
        return Some(rel);
    }
    let stem = strip_known_ext(&rel);
    let exts = ["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"];
    let mut cands: Vec<String> = Vec::new();
    for e in exts {
        cands.push(format!("{stem}.{e}"));
    }
    for e in exts {
        cands.push(format!("{stem}/index.{e}"));
    }
    cands.into_iter().find(|c| files.contains(c))
}

fn strip_known_ext(p: &str) -> &str {
    for e in [".tsx", ".ts", ".jsx", ".mjs", ".cjs", ".js", ".mts", ".cts"] {
        if let Some(s) = p.strip_suffix(e) {
            return s;
        }
    }
    p
}

fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    out.join("/")
}

// --- config loaders ----------------------------------------------------------

#[derive(Deserialize, Default)]
struct Tsconfig {
    #[serde(rename = "compilerOptions")]
    compiler_options: Option<CompilerOptions>,
    extends: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
struct CompilerOptions {
    #[serde(rename = "baseUrl")]
    base_url: Option<String>,
    paths: Option<HashMap<String, Vec<String>>>,
}

fn load_tsconfig(root: &Path, rel: &str, cfg: &mut ResolveConfig, depth: usize) {
    if depth > 8 {
        return;
    }
    let abs = root.join(rel);
    let Ok(raw) = std::fs::read_to_string(&abs) else {
        return;
    };
    let stripped = strip_jsonc(&raw);
    let Ok(ts) = serde_json::from_str::<Tsconfig>(&stripped) else {
        return;
    };
    if let Some(parent) = ts.extends.as_deref() {
        let dir = Path::new(rel).parent().unwrap_or(Path::new(""));
        let parent_rel = dir.join(parent);
        let parent_rel = parent_rel.to_string_lossy().replace('\\', "/");
        load_tsconfig(root, &parent_rel, cfg, depth + 1);
    }
    if let Some(opt) = ts.compiler_options {
        if let Some(b) = opt.base_url {
            cfg.base_url = Some(b.trim_end_matches('/').to_string());
        }
        if let Some(paths) = opt.paths {
            for (k, v) in paths {
                cfg.tsconfig_paths.insert(k, v);
            }
        }
    }
}

/// JSONC → JSON: drop `//` and `/* */` comments and trailing commas, leaving string
/// contents alone (`"@/*": ["./*"]` and `"**/*.ts"` are not comments).
fn strip_jsonc(raw: &str) -> String {
    let b = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(b.len());
                out.extend_from_slice(&b[start..i]);
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                out.push(b' ');
            }
            c @ (b'}' | b']') => {
                let last = out.iter().rposition(|x| !x.is_ascii_whitespace());
                if let Some(k) = last.filter(|&k| out[k] == b',') {
                    out[k] = b' ';
                }
                out.push(c);
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Deserialize, Default)]
struct PkgJson {
    name: Option<String>,
    workspaces: Option<Workspaces>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Workspaces {
    List(Vec<String>),
    Map { packages: Option<Vec<String>> },
}

fn load_workspace_pkgs(root: &Path, cfg: &mut ResolveConfig) {
    let pkg_path = root.join("package.json");
    let Ok(raw) = std::fs::read_to_string(&pkg_path) else {
        return;
    };
    let Ok(pkg) = serde_json::from_str::<PkgJson>(&raw) else {
        return;
    };
    if let Some(name) = pkg.name {
        cfg.workspace_pkgs.insert(name, ".".into());
    }
    let globs: Vec<String> = match pkg.workspaces {
        Some(Workspaces::List(v)) => v,
        Some(Workspaces::Map { packages }) => packages.unwrap_or_default(),
        None => vec!["packages/*".into(), "apps/*".into(), "libs/*".into()],
    };
    for g in globs {
        if let Some(pattern) = g.strip_suffix("/*") {
            let dir = root.join(pattern);
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for ent in rd.flatten() {
                if !ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let name = ent.file_name();
                let rel = format!("{pattern}/{}", name.to_string_lossy()).replace('\\', "/");
                register_pkg(root, &rel, cfg);
            }
        } else {
            register_pkg(root, &g, cfg);
        }
    }
}

fn register_pkg(root: &Path, rel_dir: &str, cfg: &mut ResolveConfig) {
    let pkg_path = root.join(rel_dir).join("package.json");
    let Ok(raw) = std::fs::read_to_string(pkg_path) else {
        return;
    };
    let Ok(pkg) = serde_json::from_str::<PkgJson>(&raw) else {
        return;
    };
    if let Some(name) = pkg.name {
        cfg.workspace_pkgs
            .insert(name, rel_dir.trim_end_matches('/').to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ImportBinding, RawCall, Symbol};

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
        let e = &resolve_with_heritage(&syms, &calls, &[], &[], &HashSet::new(), &ResolveConfig::default())[..];
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
        let edges = resolve_with_heritage(&syms, &calls, &binds, &[], &files, &ResolveConfig::default());
        let e = call_edge(&edges);
        assert_eq!(e.reason, "import-binding");
        assert_eq!(e.conf, "strong");
        assert_eq!(e.dst_id.as_deref(), Some("util.ts#helper@1"));
    }

    #[test]
    fn same_named_type_does_not_block_same_file_call() {
        let syms = vec![
            sym("a.ts#User@1", "User", "a.ts", "const", true),
            sym("a.ts#User@2", "User", "a.ts", "type", true),
            sym("a.ts#c@3", "c", "a.ts", "function", false),
        ];
        let calls = vec![call("a.ts#c@3", "User", false, "a.ts")];
        let edges = resolve_with_heritage(&syms, &calls, &[], &[], &HashSet::new(), &ResolveConfig::default());
        let e = call_edge(&edges);
        assert_eq!(e.reason, "same-file");
        assert_eq!(e.dst_id.as_deref(), Some("a.ts#User@1"));
    }

    #[test]
    fn exported_types_do_not_break_sole_export_default() {
        let syms = vec![
            sym("comp.ts#Comp@1", "Comp", "comp.ts", "function", true),
            sym("comp.ts#Props@2", "Props", "comp.ts", "interface", true),
            sym("comp.ts#SIZE@3", "SIZE", "comp.ts", "const", true),
            sym("a.ts#c@1", "c", "a.ts", "function", false),
        ];
        let calls = vec![call("a.ts#c@1", "Comp", false, "a.ts")];
        let binds = vec![ImportBinding {
            file_path: "a.ts".into(),
            local_name: "Comp".into(),
            source_module: "./comp".into(),
            imported_name: "default".into(),
            kind: "default".into(),
        }];
        let files: HashSet<String> = ["comp.ts", "a.ts"].iter().map(|s| s.to_string()).collect();
        let edges = resolve_with_heritage(&syms, &calls, &binds, &[], &files, &ResolveConfig::default());
        let e = call_edge(&edges);
        assert_eq!(e.reason, "import-binding");
        assert_eq!(e.dst_id.as_deref(), Some("comp.ts#Comp@1"));
    }

    #[test]
    fn references_resolve_by_import_or_same_file_only() {
        let symbols = vec![
            sym("ui.ts#Card@1", "Card", "ui.ts", "function", true),
            sym("ui.ts#Row@9", "Row", "ui.ts", "type", true),
            sym("a.ts#LIMIT@2", "LIMIT", "a.ts", "const", false),
            sym("a.ts#View@4", "View", "a.ts", "function", true),
            sym("z.ts#Lonely@1", "Lonely", "z.ts", "function", true),
        ];
        let bind = |local: &str| ImportBinding {
            file_path: "a.ts".into(),
            local_name: local.into(),
            source_module: "./ui".into(),
            imported_name: local.into(),
            kind: "named".into(),
        };
        let r = |name: &str, line: usize| RawRef {
            from_id: "a.ts#View@4".into(),
            name: name.into(),
            line,
            file_path: "a.ts".into(),
        };
        let refs = vec![r("Card", 5), r("Card", 5), r("Row", 6), r("LIMIT", 7), r("Lonely", 8), r("View", 9)];
        let files: HashSet<String> = ["a.ts", "ui.ts", "z.ts"].iter().map(|s| s.to_string()).collect();
        let edges = resolve_refs(&symbols, &refs, &[bind("Card"), bind("Row")], &files, &ResolveConfig::default());
        let got: Vec<(&str, &str)> = edges
            .iter()
            .map(|e| (e.dst_id.as_deref().unwrap(), e.reason.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("ui.ts#Card@1", "import-binding"),
                ("ui.ts#Row@9", "import-binding"),
                ("a.ts#LIMIT@2", "same-file"),
            ],
            "duplicates collapse; an unimported global and a self-reference yield nothing"
        );
        assert!(edges.iter().all(|e| e.kind == "references" && e.resolved && e.conf == "strong"));
    }

    #[test]
    fn receiver_unknown_is_weak() {
        let syms = vec![
            sym("a.ts#save@1", "save", "a.ts", "method", false),
            sym("a.ts#c@1", "c", "a.ts", "function", false),
        ];
        let calls = vec![call("a.ts#c@1", "save", true, "a.ts")];
        let edges = resolve_with_heritage(&syms, &calls, &[], &[], &HashSet::new(), &ResolveConfig::default());
        let e = call_edge(&edges);
        assert_eq!(e.reason, "receiver-unknown");
        assert_eq!(e.conf, "weak");
    }

    #[test]
    fn unresolved_stays_open_never_faked() {
        let syms = vec![sym("a.ts#c@1", "c", "a.ts", "function", false)];
        let calls = vec![call("a.ts#c@1", "mystery", false, "a.ts")];
        let edges = resolve_with_heritage(&syms, &calls, &[], &[], &HashSet::new(), &ResolveConfig::default());
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
        let edges = resolve_with_heritage(&syms, &[], &[], &[], &HashSet::new(), &ResolveConfig::default());
        assert!(edges.iter().any(|e| e.kind == "contains"
            && e.src_id == "a.ts#Box@1"
            && e.dst_id.as_deref() == Some("a.ts#open@2")));
    }

    #[test]
    fn relative_resolution_finds_index() {
        let mut files = HashSet::new();
        files.insert("src/util/index.ts".to_string());
        assert_eq!(
            resolve_relative("src/a.ts", "./util", &files).as_deref(),
            Some("src/util/index.ts")
        );
    }

    #[test]
    fn normalize_collapses_dotdot() {
        assert_eq!(normalize("src/foo/../bar"), "src/bar");
    }

    #[test]
    fn js_specifier_remaps_to_ts() {
        let mut files = HashSet::new();
        files.insert("src/lib/math.ts".to_string());
        assert_eq!(
            resolve_relative("src/a.ts", "./lib/math.js", &files).as_deref(),
            Some("src/lib/math.ts")
        );
    }

    #[test]
    fn jsonc_strip_leaves_glob_strings_alone() {
        let raw = r#"{
  /* Projects */
  "compilerOptions": {
    "paths": { "@/*": ["./*"], "*": ["./types/*"], }, // aliases
    "docs": "https://aka.ms/tsconfig",
  },
  "include": ["**/*.ts", ".next/types/**/*.ts"],
}"#;
        let ts: Tsconfig = serde_json::from_str(&strip_jsonc(raw)).expect("valid JSON after strip");
        let paths = ts.compiler_options.unwrap().paths.unwrap();
        assert_eq!(paths["@/*"], vec!["./*"]);
        assert_eq!(paths["*"], vec!["./types/*"]);
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
            resolve_module("src/a.ts", "@/lib/math.js", &files, &cfg).as_deref(),
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
            resolve_module("src/a.ts", "@medium/core", &files, &cfg).as_deref(),
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
        let edges = resolve_with_heritage(&syms, &calls, &binds, &[], &files, &ResolveConfig::default());
        let e = call_edge(&edges);
        assert_eq!(e.reason, "import-binding");
        assert_eq!(e.conf, "strong");
        assert_eq!(e.dst_id.as_deref(), Some("src/lib/math.ts#add@1"));
    }

    #[test]
    fn import_then_export_hands_the_symbol_on() {
        // lib.ts declares; types.ts does `import { Mode } from "./lib"; export { Mode };`
        let symbols = vec![
            sym("lib.ts#Mode@1", "Mode", "lib.ts", "type", true),
            sym("a.ts#use@3", "use", "a.ts", "function", true),
        ];
        let bind = |file: &str, from: &str| ImportBinding {
            file_path: file.into(),
            local_name: "Mode".into(),
            source_module: from.into(),
            imported_name: "Mode".into(),
            kind: "named".into(),
        };
        let refs = vec![RawRef {
            from_id: "a.ts#use@3".into(),
            name: "Mode".into(),
            line: 4,
            file_path: "a.ts".into(),
        }];
        let files: HashSet<String> = ["a.ts", "types.ts", "lib.ts"].iter().map(|s| s.to_string()).collect();
        let edges = resolve_refs(
            &symbols,
            &refs,
            &[bind("a.ts", "./types"), bind("types.ts", "./lib")],
            &files,
            &ResolveConfig::default(),
        );
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].dst_id.as_deref(), Some("lib.ts#Mode@1"));
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
        let edges = resolve_with_heritage(&syms, &calls, &binds, &[], &files, &cfg);
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
