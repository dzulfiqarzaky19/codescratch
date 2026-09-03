//! Multi-repo groups: a named set of repo roots so an agent can treat several
//! repos (a split monorepo, a set of microservices) as one logical project.
//! Registry lives at `~/.codescratch/groups.json` — GLOBAL, user-scoped, not
//! the per-repo `<repo>/.codescratch/` that db.rs owns. This module only
//! manages the registry and resolves group name → member roots. Fan-out lives
//! in [`crate::scope::Scope`]; this module never indexes or queries.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct GroupEntry {
    #[serde(default)]
    pub roots: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Registry {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub groups: BTreeMap<String, GroupEntry>,
}

fn default_version() -> u32 {
    1
}

impl Default for Registry {
    fn default() -> Self {
        Registry {
            version: 1,
            groups: BTreeMap::new(),
        }
    }
}

/// Where the registry lives. The directory is a parameter rather than
/// something the implementation reaches out and computes, so tests can point
/// at a temp dir without mutating the process-global `HOME`.
#[derive(Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Store { dir }
    }

    /// The real user-global store: `~/.codescratch/`, resolved via HOME
    /// (Linux/mac) falling back to USERPROFILE (Windows).
    pub fn user() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Store::new(home.join(".codescratch"))
    }

    /// `<dir>/groups.json`
    fn path(&self) -> PathBuf {
        self.dir.join("groups.json")
    }

    /// Load the registry. A missing file is not an error: it's an empty registry.
    pub fn load(&self) -> Result<Registry> {
        let path = self.path();
        if !path.exists() {
            return Ok(Registry::default());
        }
        let raw = std::fs::read_to_string(&path)?;
        serde_json::from_str(&raw)
            .map_err(|e| anyhow!("malformed groups.json at {}: {e}", path.display()))
    }

    /// Atomic write: serialize to `groups.json.tmp` then rename over
    /// `groups.json`, so a crash mid-write never leaves a truncated file.
    pub fn save(&self, reg: &Registry) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join("groups.json.tmp");
        let pretty = serde_json::to_string_pretty(reg)?;
        std::fs::write(&tmp, pretty)?;
        std::fs::rename(&tmp, self.path())?;
        Ok(())
    }
}

/// Load from the real user-global store.
pub fn load() -> Result<Registry> {
    Store::user().load()
}

impl Registry {
    /// All groups, sorted by name, with their member roots.
    pub fn list(&self) -> Vec<(String, Vec<PathBuf>)> {
        self.groups
            .iter()
            .map(|(name, entry)| (name.clone(), entry.roots.clone()))
            .collect()
    }

    /// Add `root` to `group`, creating the group if it doesn't exist yet.
    /// The path is canonicalized (so it must exist on disk) and deduped
    /// against the group's existing roots.
    pub fn add(&mut self, group: &str, root: &Path) -> Result<()> {
        let canon = std::fs::canonicalize(root)
            .map_err(|e| anyhow!("no such path: {} ({e})", root.display()))?;
        let entry = self.groups.entry(group.to_string()).or_default();
        if !entry.roots.contains(&canon) {
            entry.roots.push(canon);
        }
        Ok(())
    }

    /// Remove one root from a group. Errors if the group is unknown.
    /// Best-effort canonicalization: if the path no longer exists on disk
    /// (e.g. the repo was deleted), fall back to comparing the raw path so
    /// stale entries can still be removed.
    pub fn remove_root(&mut self, group: &str, root: &Path) -> Result<()> {
        let entry = self
            .groups
            .get_mut(group)
            .ok_or_else(|| anyhow!("no such group: {group}"))?;
        let target = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        entry.roots.retain(|r| r != &target && r != root);
        Ok(())
    }

    /// Remove an entire group. Errors if the group is unknown.
    pub fn remove_group(&mut self, group: &str) -> Result<()> {
        self.groups
            .remove(group)
            .map(|_| ())
            .ok_or_else(|| anyhow!("no such group: {group}"))
    }

    /// Resolve a group name to its member roots. Errors if the group is unknown.
    pub fn roots(&self, group: &str) -> Result<Vec<PathBuf>> {
        self.groups
            .get(group)
            .map(|e| e.roots.clone())
            .ok_or_else(|| anyhow!("no such group: {group}"))
    }
}

/// Member roots for a group name, or auto-detect from `root`.
/// `Some(group)` → that group's members (error if unknown).
/// `None` → unique parent of one group fans out; a member stays that repo;
/// else the single `root`.
pub fn roots(group: Option<&str>, root: &Path) -> Result<Vec<PathBuf>> {
    match group {
        Some(g) => named_roots(g),
        None => Ok(infer(root)),
    }
}

fn named_roots(g: &str) -> Result<Vec<PathBuf>> {
    let roots = load()?.roots(g)?;
    if roots.is_empty() {
        return Err(anyhow!(
            "group '{g}' has no roots (add one: codescratch group add --group {g} --root <path>)"
        ));
    }
    Ok(roots)
}

/// Cwd / given path → group members when the match is unique.
///
/// - `root` equals a member → that one repo (agent in kabana-app stays there).
/// - `root` is the unique parent of every member of exactly one group
///   (`/kabana` for group `kabana`) → that group.
/// - Ambiguous (parent of two groups, or member of none) → single `root`.
/// Failures to load the registry degrade to single-root, never error: a missing
/// `groups.json` must not break `codescratch status` in a normal repo.
pub fn infer(root: &Path) -> Vec<PathBuf> {
    let Ok(reg) = load() else {
        return vec![root.to_path_buf()];
    };
    infer_in(&reg, root)
}

fn infer_in(reg: &Registry, root: &Path) -> Vec<PathBuf> {
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());

    // member match: stay in that repo. First unique member wins; a path that
    // is a member of two groups is still one repo, so we do not fan out.
    for (_name, entry) in &reg.groups {
        if entry.roots.iter().any(|r| r == &canon) {
            return vec![canon];
        }
    }

    let mut parent_hits: Vec<&GroupEntry> = Vec::new();
    for (_name, entry) in &reg.groups {
        if entry.roots.is_empty() {
            continue;
        }
        if entry.roots.iter().all(|r| r.parent() == Some(canon.as_path())) {
            parent_hits.push(entry);
        }
    }
    if parent_hits.len() == 1 {
        return parent_hits[0].roots.clone();
    }
    vec![canon]
}

/// Group name from `--group` or the `CODESCRATCH_GROUP` env var, so a host can
/// pin a group once instead of on every call.
pub fn from_env(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(|s| s.to_string())
        .or_else(|| std::env::var("CODESCRATCH_GROUP").ok())
        .filter(|s| !s.trim().is_empty())
}

/// CLI entrypoint: string-dispatch over `action` so main.rs doesn't need to
/// know about this module's types. Returns human-readable output for the
/// caller to println!.
pub fn run(action: &str, group: Option<&str>, root: Option<&Path>) -> Result<String> {
    run_in(&Store::user(), action, group, root)
}

/// `run` against an explicit store. Tests drive this with a temp dir instead of
/// rewriting the process-global `HOME`.
pub fn run_in(
    store: &Store,
    action: &str,
    group: Option<&str>,
    root: Option<&Path>,
) -> Result<String> {
    match action {
        "list" => {
            let reg = store.load()?;
            let groups = reg.list();
            if groups.is_empty() {
                return Ok("no groups defined".to_string());
            }
            let mut out = String::new();
            for (name, roots) in groups {
                out.push_str(&format!(
                    "{name} ({} root{})\n",
                    roots.len(),
                    if roots.len() == 1 { "" } else { "s" }
                ));
                for r in roots {
                    out.push_str(&format!("  {}\n", r.display()));
                }
            }
            Ok(out.trim_end().to_string())
        }
        "add" => {
            let group = group.ok_or_else(|| anyhow!("add requires --group"))?;
            let root = root.ok_or_else(|| anyhow!("add requires --root"))?;
            let mut reg = store.load()?;
            reg.add(group, root)?;
            store.save(&reg)?;
            let n = reg.roots(group)?.len();
            Ok(format!(
                "added {} to '{group}' ({n} root{} total)",
                root.display(),
                if n == 1 { "" } else { "s" }
            ))
        }
        "remove" => {
            let group = group.ok_or_else(|| anyhow!("remove requires --group"))?;
            let root = root.ok_or_else(|| anyhow!("remove requires --root"))?;
            let mut reg = store.load()?;
            reg.remove_root(group, root)?;
            store.save(&reg)?;
            Ok(format!("removed {} from '{group}'", root.display()))
        }
        "rm-group" => {
            let group = group.ok_or_else(|| anyhow!("rm-group requires --group"))?;
            let mut reg = store.load()?;
            reg.remove_group(group)?;
            store.save(&reg)?;
            Ok(format!("removed group '{group}'"))
        }
        "roots" => {
            let group = group.ok_or_else(|| anyhow!("roots requires --group"))?;
            let reg = store.load()?;
            let roots = reg.roots(group)?;
            if roots.is_empty() {
                return Ok(format!("'{group}' has no roots"));
            }
            Ok(roots
                .iter()
                .map(|r| r.display().to_string())
                .collect::<Vec<_>>()
                .join("\n"))
        }
        other => Err(anyhow!(
            "unknown group action: {other} (expected list|add|remove|rm-group|roots)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A store in its own temp dir. No env mutation, so tests are independent
    /// and safe under the default parallel test runner.
    fn test_store(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("cs-group-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Store::new(dir)
    }

    /// A real temp directory to use as a group root (canonicalize needs it
    /// to exist). Caller-unique subdir so parallel tests don't collide.
    fn temp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cs-group-test-root-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn add_creates_group_and_persists_then_load_sees_it() {
        let store = test_store("persist");
        let root = temp_root("persist");

        let mut reg = store.load().unwrap();
        reg.add("backend", &root).unwrap();
        store.save(&reg).unwrap();

        let reloaded = store.load().unwrap();
        let roots = reloaded.roots("backend").unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0], fs::canonicalize(&root).unwrap());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_registry_file_loads_as_empty() {
        let store = test_store("missing");
        assert!(store.load().unwrap().groups.is_empty());
    }

    #[test]
    fn add_dedupes_roots() {
        let root = temp_root("dedupe");

        let mut reg = Registry::default();
        reg.add("svc", &root).unwrap();
        reg.add("svc", &root).unwrap();
        assert_eq!(reg.roots("svc").unwrap().len(), 1);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn roots_errors_on_unknown_group() {
        let reg = Registry::default();
        assert!(reg.roots("nope").is_err());
    }

    #[test]
    fn add_errors_on_nonexistent_path() {
        let mut reg = Registry::default();
        let bogus = std::env::temp_dir().join("cs-group-test-does-not-exist-xyz");
        let _ = fs::remove_dir_all(&bogus);
        assert!(reg.add("g", &bogus).is_err());
    }

    #[test]
    fn remove_root_and_remove_group_work() {
        let a = temp_root("rm-a");
        let b = temp_root("rm-b");

        let mut reg = Registry::default();
        reg.add("multi", &a).unwrap();
        reg.add("multi", &b).unwrap();
        assert_eq!(reg.roots("multi").unwrap().len(), 2);

        reg.remove_root("multi", &a).unwrap();
        assert_eq!(reg.roots("multi").unwrap().len(), 1);

        reg.remove_group("multi").unwrap();
        assert!(reg.roots("multi").is_err());
        // second removal of an already-gone group is an error, not a panic
        assert!(reg.remove_group("multi").is_err());

        let _ = fs::remove_dir_all(&a);
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn save_then_load_round_trips_json() {
        let store = test_store("roundtrip");
        let a = temp_root("roundtrip-a");
        let b = temp_root("roundtrip-b");

        let mut reg = Registry::default();
        reg.add("one", &a).unwrap();
        reg.add("two", &b).unwrap();
        store.save(&reg).unwrap();

        let reloaded = store.load().unwrap();
        assert_eq!(reloaded.version, 1);
        assert_eq!(reloaded.groups.len(), 2);
        let listed = reloaded.list();
        // list() is sorted by group name
        assert_eq!(listed[0].0, "one");
        assert_eq!(listed[1].0, "two");

        let _ = fs::remove_dir_all(&a);
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn run_string_dispatch_covers_all_actions() {
        let store = test_store("dispatch");
        let root = temp_root("dispatch");

        let msg = run_in(&store, "list", None, None).unwrap();
        assert!(msg.contains("no groups"));

        let msg = run_in(&store, "add", Some("cli-group"), Some(root.as_path())).unwrap();
        assert!(msg.contains("cli-group"));

        let msg = run_in(&store, "roots", Some("cli-group"), None).unwrap();
        assert!(!msg.is_empty());

        let msg = run_in(&store, "remove", Some("cli-group"), Some(root.as_path())).unwrap();
        assert!(msg.contains("cli-group"));

        let msg = run_in(&store, "rm-group", Some("cli-group"), None);
        // group still exists (now empty) until explicitly removed
        assert!(msg.is_ok());

        assert!(run_in(&store, "bogus-action", None, None).is_err());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn infer_member_stays_in_that_repo() {
        let parent = temp_root("infer-parent");
        let app = parent.join("app");
        let api = parent.join("api");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(&api).unwrap();

        let mut reg = Registry::default();
        reg.add("k", &app).unwrap();
        reg.add("k", &api).unwrap();

        let one = infer_in(&reg, &app);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0], fs::canonicalize(&app).unwrap());

        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn infer_unique_parent_fans_out() {
        let parent = temp_root("infer-fan");
        let app = parent.join("app");
        let api = parent.join("api");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(&api).unwrap();

        let mut reg = Registry::default();
        reg.add("k", &app).unwrap();
        reg.add("k", &api).unwrap();

        let all = infer_in(&reg, &parent);
        assert_eq!(all.len(), 2);

        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn infer_ambiguous_parent_stays_single() {
        let parent = temp_root("infer-ambig");
        let app = parent.join("app");
        let api = parent.join("api");
        let extra = parent.join("extra");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(&api).unwrap();
        fs::create_dir_all(&extra).unwrap();

        let mut reg = Registry::default();
        reg.add("k", &app).unwrap();
        reg.add("k", &api).unwrap();
        reg.add("other", &extra).unwrap();

        let hit = infer_in(&reg, &parent);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0], fs::canonicalize(&parent).unwrap());

        let _ = fs::remove_dir_all(&parent);
    }
}
