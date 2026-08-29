//! Multi-repo groups: a named set of repo roots so an agent can treat several
//! repos (a split monorepo, a set of microservices) as one logical project.
//! Registry lives at `~/.codescratch/groups.json` — GLOBAL, user-scoped, not
//! the per-repo `<repo>/.codescratch/` that db.rs owns. This module only
//! manages the registry and resolves group name → member roots; it does not
//! index or query anything (cross-repo fan-out is a later WP).

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
        Registry { version: 1, groups: BTreeMap::new() }
    }
}

/// `~/` — resolved via HOME (Linux/mac), falling back to USERPROFILE (Windows).
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `~/.codescratch/`
fn dir() -> PathBuf {
    home().join(".codescratch")
}

/// `~/.codescratch/groups.json`
fn registry_path() -> PathBuf {
    dir().join("groups.json")
}

/// Load the registry. A missing file is not an error: it's an empty registry.
pub fn load() -> Result<Registry> {
    let path = registry_path();
    if !path.exists() {
        return Ok(Registry::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    let reg: Registry = serde_json::from_str(&raw)
        .map_err(|e| anyhow!("malformed groups.json at {}: {e}", path.display()))?;
    Ok(reg)
}

impl Registry {
    /// Atomic write: serialize to `groups.json.tmp` then rename over
    /// `groups.json`, so a crash mid-write never leaves a truncated file.
    pub fn save(&self) -> Result<()> {
        let dir = dir();
        std::fs::create_dir_all(&dir)?;
        let path = registry_path();
        let tmp = dir.join("groups.json.tmp");
        let pretty = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, pretty)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

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

/// CLI entrypoint: string-dispatch over `action` so main.rs doesn't need to
/// know about this module's types. Returns human-readable output for the
/// caller to println!.
pub fn run(action: &str, group: Option<&str>, root: Option<&Path>) -> Result<String> {
    match action {
        "list" => {
            let reg = load()?;
            let groups = reg.list();
            if groups.is_empty() {
                return Ok("no groups defined".to_string());
            }
            let mut out = String::new();
            for (name, roots) in groups {
                out.push_str(&format!("{name} ({} root{})\n", roots.len(), if roots.len() == 1 { "" } else { "s" }));
                for r in roots {
                    out.push_str(&format!("  {}\n", r.display()));
                }
            }
            Ok(out.trim_end().to_string())
        }
        "add" => {
            let group = group.ok_or_else(|| anyhow!("add requires --group"))?;
            let root = root.ok_or_else(|| anyhow!("add requires --root"))?;
            let mut reg = load()?;
            reg.add(group, root)?;
            reg.save()?;
            let n = reg.roots(group)?.len();
            Ok(format!("added {} to '{group}' ({n} root{} total)", root.display(), if n == 1 { "" } else { "s" }))
        }
        "remove" => {
            let group = group.ok_or_else(|| anyhow!("remove requires --group"))?;
            let root = root.ok_or_else(|| anyhow!("remove requires --root"))?;
            let mut reg = load()?;
            reg.remove_root(group, root)?;
            reg.save()?;
            Ok(format!("removed {} from '{group}'", root.display()))
        }
        "rm-group" => {
            let group = group.ok_or_else(|| anyhow!("rm-group requires --group"))?;
            let mut reg = load()?;
            reg.remove_group(group)?;
            reg.save()?;
            Ok(format!("removed group '{group}'"))
        }
        "roots" => {
            let group = group.ok_or_else(|| anyhow!("roots requires --group"))?;
            let reg = load()?;
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
        other => Err(anyhow!("unknown group action: {other} (expected list|add|remove|rm-group|roots)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Redirect HOME to an isolated tempdir so tests never touch the real
    /// `~/.codescratch`. env is process-global, so all tests share one HOME
    /// (set once, first test to run) and use their own group names / temp
    /// root dirs to avoid interfering with each other.
    fn test_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cs-group-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);
        // Also cover the Windows fallback path in case HOME is unset there.
        std::env::set_var("USERPROFILE", &dir);
        dir
    }

    /// A real temp directory to use as a group root (canonicalize needs it
    /// to exist). Caller-unique subdir so parallel tests don't collide.
    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cs-group-test-root-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn add_creates_group_and_persists_then_load_sees_it() {
        test_home();
        let root = temp_root("persist");

        let mut reg = load().unwrap();
        reg.add("backend", &root).unwrap();
        reg.save().unwrap();

        let reloaded = load().unwrap();
        let roots = reloaded.roots("backend").unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0], fs::canonicalize(&root).unwrap());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn add_dedupes_roots() {
        test_home();
        let root = temp_root("dedupe");

        let mut reg = Registry::default();
        reg.add("svc", &root).unwrap();
        reg.add("svc", &root).unwrap();
        assert_eq!(reg.roots("svc").unwrap().len(), 1);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn roots_errors_on_unknown_group() {
        test_home();
        let reg = Registry::default();
        assert!(reg.roots("nope").is_err());
    }

    #[test]
    fn add_errors_on_nonexistent_path() {
        test_home();
        let mut reg = Registry::default();
        let bogus = std::env::temp_dir().join("cs-group-test-does-not-exist-xyz");
        let _ = fs::remove_dir_all(&bogus);
        assert!(reg.add("g", &bogus).is_err());
    }

    #[test]
    fn remove_root_and_remove_group_work() {
        test_home();
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
        test_home();
        let a = temp_root("roundtrip-a");
        let b = temp_root("roundtrip-b");

        let mut reg = Registry::default();
        reg.add("one", &a).unwrap();
        reg.add("two", &b).unwrap();
        reg.save().unwrap();

        let reloaded = load().unwrap();
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
        test_home();
        let root = temp_root("dispatch");

        let msg = run("list", None, None).unwrap();
        assert!(msg.contains("no groups") || !msg.is_empty());

        let msg = run("add", Some("cli-group"), Some(root.as_path())).unwrap();
        assert!(msg.contains("cli-group"));

        let msg = run("roots", Some("cli-group"), None).unwrap();
        assert!(!msg.is_empty());

        let msg = run("remove", Some("cli-group"), Some(root.as_path())).unwrap();
        assert!(msg.contains("cli-group"));

        let msg = run("rm-group", Some("cli-group"), None);
        // group still exists (now empty) until explicitly removed
        assert!(msg.is_ok());

        assert!(run("bogus-action", None, None).is_err());

        let _ = fs::remove_dir_all(&root);
    }
}
