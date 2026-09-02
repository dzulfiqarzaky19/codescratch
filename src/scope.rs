//! `Scope` — the set of repos a command acts on, and the single place that
//! knows the one-vs-many rule.
//!
//! Callers say `scope.explore(sym)` instead of `if roots.len() == 1`. The
//! one-vs-many *dispatch* lives here; per-repo work stays in `query` / `host`
//! / `changes`. Group fan-out (trust_or_missing + dead-member merge +
//! `trust::render_group`) is one loop — a group honesty leak is one place to
//! miss. One root renders exactly what it rendered before groups existed.

use crate::query::Explored;
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
        if !self.is_group() {
            return query::status(&self.roots[0]);
        }
        self.map_group(|_root, label, t, err| {
            Ok(Some(match err {
                None => format!("- {label}: {}\n", trust::banner(t)),
                Some(e) => format!("- {label}: unavailable ({e})\n"),
            }))
        })
    }

    pub fn search(&self, q: &str) -> Result<String> {
        if !self.is_group() {
            return query::search(&self.roots[0], q);
        }
        self.map_group(|root, label, _t, err| {
            if let Some(e) = err {
                return Ok(Some(format!("- {label}: unavailable ({e})\n")));
            }
            match query::search_hits(root, q, Some(label)) {
                Ok(s) => Ok(Some(s)),
                Err(e) => Ok(Some(format!("- {label}: error ({e})\n"))),
            }
        })
    }

    pub fn explore(&self, symbol: &str) -> Result<String> {
        if !self.is_group() {
            return query::explore(&self.roots[0], symbol);
        }
        let mut hits = Vec::new();
        let mut misses = Vec::new();
        let (parts, _) = self.fold_group(|root, label, _t, _err| {
            match query::explore_one(root, symbol) {
                Ok(Explored::Found(view)) => hits.push(format!(
                    "# repo `{label}`\n\n{}",
                    crate::explore::render_view(&view)
                )),
                Ok(Explored::Missing { .. }) => misses.push(label.to_string()),
                Err(e) => misses.push(format!("{label} (error: {e})")),
            }
            Ok(None)
        })?;
        let mut body = String::new();
        if hits.is_empty() {
            body.push_str(&format!(
                "no symbol named `{symbol}` in any group repo. try `search {symbol}`.\n"
            ));
        } else {
            body.push_str(&hits.join("\n---\n\n"));
        }
        if !misses.is_empty() {
            body.push_str(&format!("\nnot found in: {}\n", misses.join(", ")));
        }
        Ok(trust::render_group(&parts, self.roots.len(), &body))
    }

    /// Changed symbols + blast for every repo in scope. Group form labels each
    /// repo and skips those with an empty diff, so a 3-repo group where only
    /// one repo has edits reads like a single-repo answer plus a header.
    pub fn changes(&self, spec: changes::ChangeSpec) -> Result<String> {
        if !self.is_group() {
            return changes::detect(&self.roots[0], spec);
        }
        let (parts, mut body) = self.fold_group(|root, label, _t, _err| {
            match changes::detect_one(root, &spec) {
                Ok(Some(section)) => Ok(Some(format!("# repo `{label}`\n\n{section}\n"))),
                Ok(None) => Ok(None), // clean tree — nothing to say about this repo
                Err(e) => Ok(Some(format!("# repo `{label}`\n\n- unavailable ({e})\n\n"))),
            }
        })?;
        if body.is_empty() {
            body.push_str("no symbols overlap the diff in any group repo.\n");
        }
        Ok(trust::render_group(&parts, self.roots.len(), &body))
    }

    /// One loop over members: always `trust_or_missing` (so a dead repo cannot
    /// hide behind the rest), then `body_of` builds that member's fragment.
    /// `None` omits the member from the body; the banner still counts it.
    fn fold_group(
        &self,
        mut body_of: impl FnMut(&Path, &str, &trust::Trust, Option<&str>) -> Result<Option<String>>,
    ) -> Result<(Vec<trust::Trust>, String)> {
        let mut parts = Vec::new();
        let mut body = String::new();
        for root in &self.roots {
            let label = query::repo_label(root);
            let (t, err) = query::trust_or_missing(root);
            if let Some(chunk) = body_of(root, &label, &t, err.as_deref())? {
                body.push_str(&chunk);
            }
            parts.push(t);
        }
        Ok((parts, body))
    }

    fn map_group(
        &self,
        body_of: impl FnMut(&Path, &str, &trust::Trust, Option<&str>) -> Result<Option<String>>,
    ) -> Result<String> {
        let (parts, body) = self.fold_group(body_of)?;
        Ok(trust::render_group(&parts, self.roots.len(), &body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn single_root_is_not_a_group() {
        let s = Scope {
            roots: vec![PathBuf::from("/repo")],
        };
        assert!(!s.is_group());
        assert_eq!(s.roots().len(), 1);
    }

    #[test]
    fn many_roots_is_a_group() {
        let s = Scope {
            roots: vec![PathBuf::from("/a"), PathBuf::from("/b")],
        };
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

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cs-scope-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn fold_group_merges_every_member_into_the_banner() {
        let a = tmp("a");
        let b = tmp("b");
        let s = Scope {
            roots: vec![a.clone(), b.clone()],
        };
        let out = s
            .map_group(|_root, label, _t, _err| Ok(Some(format!("saw {label}\n"))))
            .unwrap();
        assert!(out.contains("[group: 2 repos]"), "{out}");
        assert!(out.contains("trust: missing"), "{out}");
        assert!(out.contains("saw"), "{out}");
        let _ = fs::remove_dir_all(&a);
        let _ = fs::remove_dir_all(&b);
    }
}
