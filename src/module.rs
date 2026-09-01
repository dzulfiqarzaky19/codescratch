//! Specifier → file. Relative, tsconfig `paths`/`baseUrl`(+`extends`), workspace
//! packages. `export *` barrels are followed at export lookup in resolve.rs
//! (depth ≤6); this module only answers "which file does this specifier hit?".

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

/// Resolve any specifier: relative, alias, baseUrl, workspace package.
pub fn resolve_module(
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
pub fn resolve_relative(from_file: &str, spec: &str, files: &HashSet<String>) -> Option<String> {
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

pub fn normalize(path: &str) -> String {
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

fn strip_jsonc(raw: &str) -> String {
    let mut s = raw.to_string();
    while let Some(i) = s.find("/*") {
        match s[i + 2..].find("*/") {
            Some(j) => s.replace_range(i..i + 2 + j + 2, " "),
            None => break,
        }
    }
    s.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
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
