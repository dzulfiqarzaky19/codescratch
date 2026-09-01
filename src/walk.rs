//! One walk of a repo root: source files we index, with mtime+size.
//!
//! `is_dirty` and `scan` used to each own a `WalkBuilder` with the same
//! filters (skip `.codescratch/`, skip unknown languages). A change to either
//! copy would desync the dirty-gate from the rebuild. Callers now consume
//! [`entries`] and decide whether to read/parse.

use crate::model::Lang;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// One source file the indexer cares about. Contents are not read here —
/// the dirty-gate only needs mtime+size; extract reads after a miss.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Repo-relative, forward slashes.
    pub rel: String,
    pub abs: PathBuf,
    pub mtime_ms: i64,
    pub size: i64,
    pub lang: Lang,
}

/// Walk `root` once. Hidden files are included (`hidden(false)`), matching
/// the previous WalkBuilder in `index.rs`. Unknown languages and the graph
/// store itself are dropped.
pub fn entries(root: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    for result in WalkBuilder::new(root).hidden(false).build() {
        let Ok(entry) = result else { continue };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let abs = entry.path();
        let rel = match abs.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        let Some(lang) = tracked(&rel) else {
            continue;
        };
        let meta = std::fs::metadata(abs).ok();
        let mtime_ms = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let size = meta.as_ref().map(|m| m.len() as i64).unwrap_or(0);
        out.push(Entry {
            rel,
            abs: abs.to_path_buf(),
            mtime_ms,
            size,
            lang,
        });
    }
    out
}

/// Repo-relative path the indexer and watcher both care about: not the graph
/// store, not `.git/`, and a known source language. One predicate so a skip
/// rule cannot drift between dirty-gate and fs events.
pub fn tracked(rel: &str) -> Option<Lang> {
    if rel.starts_with(".codescratch/") || rel.starts_with(".git/") {
        return None;
    }
    Lang::from_path(rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cs-walk-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        d
    }

    #[test]
    fn skips_codescratch_and_unknown_langs() {
        let d = tmp("skip");
        fs::write(d.join("src/a.ts"), "export const a = 1;").unwrap();
        fs::write(d.join("README.md"), "nope").unwrap();
        fs::create_dir_all(d.join(".codescratch")).unwrap();
        fs::write(d.join(".codescratch/x.ts"), "should skip").unwrap();
        let ents = entries(&d);
        assert_eq!(ents.len(), 1, "{ents:?}");
        assert_eq!(ents[0].rel, "src/a.ts");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tracked_skips_git_and_store_keeps_source() {
        assert!(tracked("src/a.ts").is_some());
        assert!(tracked(".codescratch/x.ts").is_none());
        assert!(tracked(".git/hooks/pre-commit.ts").is_none());
        assert!(tracked("README.md").is_none());
    }

    #[test]
    fn records_size() {
        let d = tmp("size");
        fs::write(d.join("src/a.ts"), "12345").unwrap();
        let ents = entries(&d);
        assert_eq!(ents[0].size, 5);
        let _ = fs::remove_dir_all(&d);
    }
}
