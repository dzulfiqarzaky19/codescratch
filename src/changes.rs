//! Symbols touched by a git diff, plus their blast radius. WP-3B.
//! `git diff` → hunk headers → changed line ranges → overlapping symbols
//! (nodes) → reverse-BFS over `calls` edges for the affected set.

use crate::model::NodeRow;
use crate::{blast, db, trust};
use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

/// Which diff to inspect.
#[allow(dead_code)] // Staged/Compare reserved for `--staged` / `--compare <ref>` CLI flags
pub enum ChangeSpec {
    /// Working tree vs index (`git diff`).
    Unstaged,
    /// Index vs HEAD (`git diff --cached`).
    Staged,
    /// Working tree vs an arbitrary ref/range (`git diff <ref>`).
    Compare(String),
}

/// A changed file's new-side line ranges, `[start, end]` inclusive, 1-based.
struct FileHunks {
    file_path: String,
    ranges: Vec<(i64, i64)>,
    /// File appears in the diff but has no new-side ranges we could extract
    /// (e.g. pure deletion, or a hunk header we couldn't parse).
    unmappable: bool,
}

/// Run the requested `git diff` and render markdown: banner → changed → affected.
pub fn detect(root: &Path, spec: ChangeSpec) -> Result<String> {
    let conn = db::open(root)?;
    let banner = trust::banner(&trust::compute(&conn, root)?);
    // A clean tree still gets the full "nothing changed" body in single-repo
    // form, so the output is unchanged from before groups existed.
    let body = detect_one(root, &spec)?.unwrap_or_else(empty_body);
    Ok(format!("{banner}\n\n{body}"))
}

/// The changed+affected body for one repo, minus the banner, or `None` when
/// the diff touches nothing. Group callers use `None` to skip a repo entirely
/// instead of printing an empty section per member.
pub fn detect_one(root: &Path, spec: &ChangeSpec) -> Result<Option<String>> {
    let conn = db::open(root)?;

    let diff_text = run_git_diff(root, spec)?;
    let hunks = parse_hunks(&diff_text);
    if hunks.is_empty() {
        return Ok(None);
    }

    // Pull every node once; overlap is a pure in-memory scan (files are small
    // in practice — a handful per diff — so no need to index by file first).
    let all_nodes = load_nodes(&conn)?;

    let mut changed_ids: HashSet<String> = HashSet::new();
    let mut changed_rows: Vec<&NodeRow> = Vec::new();
    let mut partial = false;

    for fh in &hunks {
        if fh.unmappable {
            partial = true;
            continue;
        }
        let hits = symbols_overlapping(&all_nodes, &fh.file_path, &fh.ranges);
        if hits.is_empty() {
            // Hunk touched a file but matched no known symbol (new file not
            // yet indexed, module-scope edit, whitespace-only region, …).
            // Anti-false-negative: flag rather than silently drop.
            partial = true;
            continue;
        }
        for n in hits {
            if changed_ids.insert(n.id.clone()) {
                changed_rows.push(n);
            }
        }
    }
    changed_rows.sort_by(|a, b| (&a.file_path, a.start_line).cmp(&(&b.file_path, b.start_line)));

    let affected = blast_hops(&conn, &changed_ids)?;

    let mut out = String::new();
    out.push_str("## changed\n");
    if changed_rows.is_empty() {
        out.push_str(NO_OVERLAP);
    } else {
        for n in &changed_rows {
            out.push_str(&format!(
                "- {} `{}`  {}:{}\n",
                n.kind, n.qualified_name, n.file_path, n.start_line
            ));
        }
    }

    out.push_str("\n## affected (blast)\n");
    if affected.is_empty() {
        out.push_str(NO_AFFECTED);
    } else {
        for (depth, n) in &affected {
            out.push_str(&format!(
                "- depth {depth}: {} `{}`  {}:{}\n",
                n.kind, n.qualified_name, n.file_path, n.start_line
            ));
        }
    }

    if partial {
        out.push_str(
            "\npartial: true — some changed files/hunks could not be mapped to symbols \
             (unindexed file, module-scope edit, or unparsed hunk); absence above is not proof of no impact.\n",
        );
    }

    Ok(Some(out))
}

const NO_OVERLAP: &str = "- (no symbols overlap the diff)\n";
const NO_AFFECTED: &str = "- (none — no resolved callers, or absence ≠ proof)\n";

/// The body for a repo with an empty diff. Kept identical to what the old
/// single-repo path printed when no hunk matched a symbol.
fn empty_body() -> String {
    format!("## changed\n{NO_OVERLAP}\n## affected (blast)\n{NO_AFFECTED}")
}

fn run_git_diff(root: &Path, spec: &ChangeSpec) -> Result<String> {
    let extra: Vec<&str> = match spec {
        ChangeSpec::Unstaged => vec![],
        ChangeSpec::Staged => vec!["--cached"],
        ChangeSpec::Compare(reference) => vec![reference.as_str()],
    };
    crate::git::diff(root, &extra)
}

fn load_nodes(conn: &rusqlite::Connection) -> Result<Vec<NodeRow>> {
    Ok(NodeRow::all(conn)?)
}

/// Format adapter over [`blast::from_ids`]: hops with no node are dropped
/// (module-scope call sites have nothing to name in the changes report).
fn blast_hops(
    conn: &rusqlite::Connection,
    changed: &HashSet<String>,
) -> Result<Vec<(usize, NodeRow)>> {
    let seeds: Vec<&str> = changed.iter().map(|s| s.as_str()).collect();
    let mut hops = blast::from_ids(conn, &seeds, blast::MAX_DEPTH)?;
    hops.sort_by(|a, b| {
        let af = a.node.as_ref().map(|n| n.file_path.as_str()).unwrap_or("");
        let bf = b.node.as_ref().map(|n| n.file_path.as_str()).unwrap_or("");
        let al = a.node.as_ref().map(|n| n.start_line).unwrap_or(0);
        let bl = b.node.as_ref().map(|n| n.start_line).unwrap_or(0);
        (a.depth, af, al).cmp(&(b.depth, bf, bl))
    });
    Ok(hops
        .into_iter()
        .filter_map(|h| h.node.map(|n| (h.depth, n)))
        .collect())
}

/// Pure: parse a unified diff (as produced by `git diff --unified=0`) into
/// per-file changed NEW-side line ranges, `[start, end]` inclusive.
///
/// Recognizes `+++ b/<path>` to start a new file section and
/// `@@ -a[,b] +c[,d] @@` hunk headers for ranges. A hunk with `d == 0`
/// (pure deletion, nothing added) contributes no new-side range but does not
/// itself mark the file unmappable — deleted-only hunks legitimately have no
/// overlap to report. A file section with `+++ /dev/null` (deleted file) is
/// skipped as unmappable, since there is no new-side content to map to nodes.
fn parse_hunks(diff: &str) -> Vec<FileHunks> {
    let mut files: Vec<FileHunks> = Vec::new();
    let mut cur: Option<usize> = None; // index into `files`

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let path = rest.trim();
            if path == "/dev/null" {
                // Deleted file: no new-side lines to map. Record as
                // unmappable so callers can flag `partial: true`.
                files.push(FileHunks {
                    file_path: String::new(),
                    ranges: Vec::new(),
                    unmappable: true,
                });
                cur = None;
                continue;
            }
            let clean = path.strip_prefix("b/").unwrap_or(path).to_string();
            files.push(FileHunks {
                file_path: clean,
                ranges: Vec::new(),
                unmappable: false,
            });
            cur = Some(files.len() - 1);
            continue;
        }
        if line.starts_with("@@") {
            let Some(i) = cur else { continue };
            match parse_hunk_header(line) {
                Some((start, len)) if len > 0 => {
                    files[i].ranges.push((start, start + len - 1));
                }
                Some(_) => {
                    // len == 0: pure deletion on the new side, nothing added.
                }
                None => {
                    files[i].unmappable = true;
                }
            }
        }
    }

    files
}

/// Parse one `@@ -a[,b] +c[,d] @@ ...` hunk header, returning the new-side
/// `(start_line, line_count)`. Bare `+c` (no comma) means a 1-line range.
fn parse_hunk_header(line: &str) -> Option<(i64, i64)> {
    // Header looks like: @@ -12,3 +14,5 @@ optional trailing context
    let rest = line.strip_prefix("@@ ")?;
    let plus_start = rest.find('+')?;
    let after_plus = &rest[plus_start + 1..];
    let end = after_plus.find(' ').unwrap_or(after_plus.len());
    let new_range = &after_plus[..end];

    let mut parts = new_range.splitn(2, ',');
    let start: i64 = parts.next()?.parse().ok()?;
    let len: i64 = match parts.next() {
        Some(l) => l.parse().ok()?,
        None => 1,
    };
    Some((start, len))
}

/// Pure: which nodes in `file` overlap any of `ranges`?
/// A node overlaps a range if `[start_line, end_line]` intersects it.
fn symbols_overlapping<'a>(
    nodes: &'a [NodeRow],
    file: &str,
    ranges: &[(i64, i64)],
) -> Vec<&'a NodeRow> {
    nodes
        .iter()
        .filter(|n| n.file_path == file)
        .filter(|n| {
            ranges
                .iter()
                .any(|&(rs, re)| n.start_line <= re && rs <= n.end_line)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, file: &str, start: i64, end: i64) -> NodeRow {
        NodeRow {
            id: id.to_string(),
            kind: "function".to_string(),
            name: id.to_string(),
            qualified_name: id.to_string(),
            file_path: file.to_string(),
            start_line: start,
            end_line: end,
            exported: true,
            signature: String::new(),
        }
    }

    #[test]
    fn parses_single_hunk_header_with_counts() {
        let (start, len) = parse_hunk_header("@@ -12,3 +14,5 @@ fn foo() {").unwrap();
        assert_eq!(start, 14);
        assert_eq!(len, 5);
    }

    #[test]
    fn parses_bare_single_line_hunk_header() {
        // No comma means a single-line range on that side.
        let (start, len) = parse_hunk_header("@@ -8 +9 @@").unwrap();
        assert_eq!(start, 9);
        assert_eq!(len, 1);
    }

    #[test]
    fn parses_pure_addition_hunk_header() {
        // Pure addition: old side is `-a,0`.
        let (start, len) = parse_hunk_header("@@ -5,0 +6,2 @@").unwrap();
        assert_eq!(start, 6);
        assert_eq!(len, 2);
    }

    #[test]
    fn parses_pure_deletion_hunk_header_as_zero_len() {
        // Pure deletion: new side is `+c,0` — nothing added.
        let (start, len) = parse_hunk_header("@@ -5,2 +6,0 @@").unwrap();
        assert_eq!(start, 6);
        assert_eq!(len, 0);
    }

    #[test]
    fn rejects_malformed_header() {
        assert!(parse_hunk_header("@@ nonsense @@").is_none());
    }

    #[test]
    fn parse_hunks_extracts_ranges_per_file() {
        let diff = "\
diff --git a/src/foo.rs b/src/foo.rs
index 111..222 100644
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -10,0 +11,3 @@ fn foo() {
+line1
+line2
+line3
@@ -20,2 +23,1 @@ fn bar() {
-old
-old2
+new
diff --git a/src/baz.rs b/src/baz.rs
index 333..444 100644
--- a/src/baz.rs
+++ b/src/baz.rs
@@ -1,1 +1,1 @@
-old
+new
";
        let hunks = parse_hunks(diff);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].file_path, "src/foo.rs");
        assert_eq!(hunks[0].ranges, vec![(11, 13), (23, 23)]);
        assert!(!hunks[0].unmappable);
        assert_eq!(hunks[1].file_path, "src/baz.rs");
        assert_eq!(hunks[1].ranges, vec![(1, 1)]);
    }

    #[test]
    fn parse_hunks_pure_deletion_hunk_yields_no_range() {
        let diff = "\
diff --git a/src/foo.rs b/src/foo.rs
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -10,3 +9,0 @@ fn foo() {
-line1
-line2
-line3
";
        let hunks = parse_hunks(diff);
        assert_eq!(hunks.len(), 1);
        assert!(hunks[0].ranges.is_empty());
        assert!(!hunks[0].unmappable);
    }

    #[test]
    fn parse_hunks_deleted_file_marked_unmappable() {
        let diff = "\
diff --git a/src/gone.rs b/src/gone.rs
deleted file mode 100644
--- a/src/gone.rs
+++ /dev/null
@@ -1,5 +0,0 @@
-stuff
";
        let hunks = parse_hunks(diff);
        assert_eq!(hunks.len(), 1);
        assert!(hunks[0].unmappable);
    }

    #[test]
    fn overlap_detects_intersecting_ranges() {
        let nodes = vec![
            node("a", "src/foo.rs", 1, 5),
            node("b", "src/foo.rs", 10, 20),
            node("c", "src/foo.rs", 25, 30),
            node("d", "src/other.rs", 10, 20),
        ];
        // Range 18-22 overlaps node b (10-20) but not c (25-30) or a.
        let hits = symbols_overlapping(&nodes, "src/foo.rs", &[(18, 22)]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "b");
    }

    #[test]
    fn overlap_matches_exact_boundary_touch() {
        let nodes = vec![node("a", "src/foo.rs", 10, 20)];
        // Range starts exactly at node's end_line: still overlaps.
        let hits = symbols_overlapping(&nodes, "src/foo.rs", &[(20, 25)]);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn overlap_ignores_other_files() {
        let nodes = vec![node("a", "src/foo.rs", 1, 100)];
        let hits = symbols_overlapping(&nodes, "src/bar.rs", &[(1, 100)]);
        assert!(hits.is_empty());
    }

    #[test]
    fn overlap_none_when_ranges_dont_touch() {
        let nodes = vec![node("a", "src/foo.rs", 1, 5)];
        let hits = symbols_overlapping(&nodes, "src/foo.rs", &[(10, 20)]);
        assert!(hits.is_empty());
    }
}
