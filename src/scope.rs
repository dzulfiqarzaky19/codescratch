//! `Scope` — the set of repos a command acts on, and the single place that
//! knows the one-vs-many rule.
//!
//! Callers say `scope.explore(sym)` instead of `if roots.len() == 1`. The
//! one-vs-many *dispatch* lives here; per-repo work stays in `query` / `host`
//! / `changes`, and the group banner merge lives in `trust::render_group`.
//! One root renders exactly what it rendered before groups existed.

use crate::{changes, group, host, query, trust};
use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct Scope {
    roots: Vec<PathBuf>,
}

impl Scope {
    /// Resolve `--group` (or `CODESCRATCH_GROUP`) to its member roots, else
    /// fall back to the single `root`. Errors if the group is unknown or empty,
    /// so a typo fails here instead of silently degrading to one repo.
    pub fn resolve(group_name: Option<&str>, root: &Path) -> Result<Self> {
        let name = group::from_env(group_name);
        let roots = group::scope(name.as_deref(), root)?;
        Ok(Scope { roots })
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// True when this scope fans out. The rule lives here and nowhere else.
    pub fn is_group(&self) -> bool {
        self.roots.len() > 1
    }

    /// Bring every repo in scope up to date. `force` is the emergency full
    /// rebuild. One failing repo does not abort the others.
    pub fn ensure(&self, force: bool) -> Result<()> {
        host::ensure_many(&self.roots, force)
    }

    pub fn status(&self) -> Result<String> {
        if self.is_group() {
            query::status_group(&self.roots)
        } else {
            query::status(&self.roots[0])
        }
    }

    pub fn search(&self, q: &str) -> Result<String> {
        if self.is_group() {
            query::search_group(&self.roots, q)
        } else {
            query::search(&self.roots[0], q)
        }
    }

    pub fn explore(&self, symbol: &str) -> Result<String> {
        if self.is_group() {
            query::explore_group(&self.roots, symbol)
        } else {
            query::explore(&self.roots[0], symbol)
        }
    }

    /// Changed symbols + blast for every repo in scope. Group form labels each
    /// repo and skips those with an empty diff, so a 3-repo group where only
    /// one repo has edits reads like a single-repo answer plus a header.
    pub fn changes(&self, spec: changes::ChangeSpec) -> Result<String> {
        if !self.is_group() {
            return changes::detect(&self.roots[0], spec);
        }
        let mut parts = Vec::new();
        let mut body = String::new();
        for root in &self.roots {
            let label = query::repo_label(root);
            match query::trust_of(root) {
                Ok(t) => parts.push(t),
                Err(_) => parts.push(trust::missing()),
            }
            match changes::detect_one(root, &spec) {
                Ok(Some(section)) => body.push_str(&format!("# repo `{label}`\n\n{section}\n")),
                Ok(None) => continue, // clean tree — nothing to say about this repo
                // A non-git or unreadable member must not sink the whole
                // report; name it and keep going.
                Err(e) => body.push_str(&format!("# repo `{label}`\n\n- unavailable ({e})\n\n")),
            }
        }
        if body.is_empty() {
            body.push_str("no symbols overlap the diff in any group repo.\n");
        }
        Ok(trust::render_group(&parts, self.roots.len(), &body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_root_is_not_a_group() {
        let s = Scope { roots: vec![PathBuf::from("/repo")] };
        assert!(!s.is_group());
        assert_eq!(s.roots().len(), 1);
    }

    #[test]
    fn many_roots_is_a_group() {
        let s = Scope { roots: vec![PathBuf::from("/a"), PathBuf::from("/b")] };
        assert!(s.is_group());
    }

    #[test]
    fn resolve_without_group_uses_the_single_root() {
        std::env::remove_var("CODESCRATCH_GROUP");
        let s = Scope::resolve(None, Path::new("/repo")).unwrap();
        assert!(!s.is_group());
        assert_eq!(s.roots()[0], PathBuf::from("/repo"));
    }

    #[test]
    fn resolve_errors_on_unknown_group() {
        assert!(Scope::resolve(Some("definitely-not-a-real-group"), Path::new("/repo")).is_err());
    }
}
