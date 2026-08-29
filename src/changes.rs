//! Symbols touched by a git diff, plus their blast radius. WP-3B.
//! `git diff` → hunk headers → changed line ranges → overlapping symbols
//! (nodes) → reverse-BFS over `calls` edges for the affected set.

use crate::{db, trust};
use anyhow::Result;
use std::collections::{HashSet, VecDeque};
use std::path::Path;

const MAX_DEPTH: usize = 3;

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

struct NodeRow {
    id: String,
    kind: String,
    qualified_name: String,
    file_path: String,
    start_line: i64,
    end_line: i64,
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
    let t = trust::compute(&conn, root)?;
    let banner = trust::banner(&t);

    let diff_text = run_git_diff(root, &spec)?;
    let hunks = parse_hunks(&diff_text);

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

    let affected = affected_blast(&conn, &changed_ids, MAX_DEPTH)?;

    let mut out = String::new();
    out.push_str(&banner);
    out.push_str("\n\n");

    out.push_str("## changed\n");
    if changed_rows.is_empty() {
        out.push_str("- (no symbols overlap the diff)\n");
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
        out.push_str("- (none — no resolved callers, or absence ≠ proof)\n");
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

    Ok(out)
}

/// Shell out to git for the requested diff, unified with zero context so hunk
/// ranges are tight. Mirrors `host::git_head`'s `Command::new("git")` style.
fn run_git_diff(root: &Path, spec: &ChangeSpec) -> Result<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(root).arg("diff").arg("--unified=0");
    match spec {
        ChangeSpec::Unstaged => {}
        ChangeSpec::Staged => {
            cmd.arg("--cached");
        }
        ChangeSpec::Compare(reference) => {
            cmd.arg(reference);
        }
    }
    let out = cmd.output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("git diff failed: {}", stderr.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn load_nodes(conn: &rusqlite::Connection) -> Result<Vec<NodeRow>> {
    let mut stmt = conn.prepare(
        "SELECT id,kind,qualified_name,file_path,start_line,end_line FROM nodes",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(NodeRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                qualified_name: r.get(2)?,
                file_path: r.get(3)?,
                start_line: r.get(4)?,
                end_line: r.get(5)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Reverse-BFS over `edges(kind='calls')`: from each changed node, follow
/// `dst_id = changed` back to `src_id` (the caller), depth-first-fanning out
/// up to `max_depth` hops, deduping visited nodes.
fn affected_blast(
    conn: &rusqlite::Connection,
    changed: &HashSet<String>,
    max_depth: usize,
) -> Result<Vec<(usize, NodeRow)>> {
    let mut seen: HashSet<String> = changed.clone();
    let mut q: VecDeque<(String, usize)> = changed.iter().map(|id| (id.clone(), 0)).collect();
    let mut out: Vec<(usize, NodeRow)> = Vec::new();

    let mut stmt = conn.prepare("SELECT src_id FROM edges WHERE dst_id = ?1 AND kind = 'calls'")?;

    while let Some((cur, depth)) = q.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let callers: Vec<String> = stmt
            .query_map([&cur], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        for caller_id in callers {
            if !seen.insert(caller_id.clone()) {
                continue;
            }
            let d = depth + 1;
            if let Some(node) = node_by_id(conn, &caller_id)? {
                out.push((d, node));
            }
            q.push_back((caller_id, d));
        }
    }

    out.sort_by(|a, b| {
        (a.0, &a.1.file_path, a.1.start_line).cmp(&(b.0, &b.1.file_path, b.1.start_line))
    });
    Ok(out)
}

fn node_by_id(conn: &rusqlite::Connection, id: &str) -> Result<Option<NodeRow>> {
    let row = conn
        .query_row(
            "SELECT id,kind,qualified_name,file_path,start_line,end_line FROM nodes WHERE id=?1",
            [id],
            |r| {
                Ok(NodeRow {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    qualified_name: r.get(2)?,
                    file_path: r.get(3)?,
                    start_line: r.get(4)?,
                    end_line: r.get(5)?,
                })
            },
        )
        .ok();
    Ok(row)
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
            qualified_name: id.to_string(),
            file_path: file.to_string(),
            start_line: start,
            end_line: end,
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
