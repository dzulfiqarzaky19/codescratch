//! The moat: three orthogonal freshness axes, rendered as the first line of
//! every explore/status payload. Ported from codescratch `src/query/trust.ts`
//! + `format.ts`. Neither parent tool has this.

use crate::{db, git};
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

pub struct Trust {
    pub trust: String,    // fresh | stale | rebuilding | missing  (HEAD vs indexed_head)
    pub coverage: String, // exhaustive | sampled
    pub resolve: String,  // ok | partial  (in-repo bind rate — not freshness)
    pub files: i64,
    pub nodes: i64,
    pub edges: i64,
}

pub fn compute(conn: &Connection, root: &Path) -> Result<Trust> {
    let files = db::count(conn, "files")?;
    let nodes = db::count(conn, "nodes")?;
    let edges = db::count(conn, "edges")?;

    // --- trust axis ---
    let state = db::get_meta(conn, "reindex_state")?.unwrap_or_default();
    let trust = if files == 0 {
        "missing"
    } else if state == "rebuilding" {
        "rebuilding"
    } else {
        let indexed = db::get_meta(conn, "indexed_head")?;
        match (indexed, git::head(root)) {
            // git repo, and HEAD moved since index → stale
            (Some(a), Some(b)) if a != b => "stale",
            (Some(_), Some(_)) => "fresh",
            // no indexed head recorded but files exist → can't prove fresh
            (None, Some(_)) => "stale",
            // non-git repo: HEAD tells us nothing; trust the index
            _ => "fresh",
        }
    }
    .to_string();

    // --- coverage axis --- (full-rebuild indexer visits every file → exhaustive)
    let coverage = db::get_meta(conn, "coverage")?.unwrap_or_else(|| "exhaustive".to_string());

    // --- resolve axis --- in-repo bind rate, NOT freshness.
    // Must not share vocabulary with `trust:` (`stale`/`fresh`) or agents
    // treat a quality flag as "index is behind".
    let resolve = resolve_quality(conn)?;

    Ok(Trust {
        trust,
        coverage,
        resolve,
        files,
        nodes,
        edges,
    })
}

/// In-repo call/heritage resolution. Honesty labels that are *not* misses:
/// `external-import`, `receiver-unknown`, and `unresolved` whose name is not
/// a graph node (`expect`/`it`/`Number`/`sql`). Empty in-scope set → `ok`.
fn resolve_quality(conn: &Connection) -> Result<String> {
    let (in_scope, resolved): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(resolved), 0)
         FROM edges e
         WHERE e.kind IN ('calls','extends','implements')
           AND (
             e.reason IN ('import-binding','same-file','unique-global')
             OR (e.reason = 'unresolved'
                 AND EXISTS (SELECT 1 FROM nodes n WHERE n.name = e.raw_name))
           )",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(if in_scope == 0 || (resolved as f64) / (in_scope as f64) >= 0.6 {
        "ok".into()
    } else {
        "partial".into()
    })
}

/// Short repo label for group output: the root directory name.
pub fn repo_label(root: &Path) -> String {
    root.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| root.display().to_string())
}

/// One repo's trust. Opens the graph; callers that already have a connection
/// use [`compute`] instead.
pub fn of(root: &Path) -> Result<Trust> {
    let conn = db::open(root)?;
    compute(&conn, root)
}

/// Trust for a member, or [`missing`] if the index cannot be read.
/// Group callers must use this so a dead repo cannot hide behind the rest.
pub fn or_missing(root: &Path) -> (Trust, Option<String>) {
    match of(root) {
        Ok(t) => (t, None),
        Err(e) => (missing(), Some(e.to_string())),
    }
}

/// The signature line. Always first.
pub fn banner(t: &Trust) -> String {
    format!(
        "trust: {} · coverage: {} · resolve: {}  ({} files, {} symbols, {} edges)",
        t.trust, t.coverage, t.resolve, t.files, t.nodes, t.edges
    )
}

/// Worst-wins rank for the freshness axis. A group is only as trustworthy as
/// its least trustworthy member.
fn trust_rank(s: &str) -> u8 {
    match s {
        "fresh" => 0,
        "stale" => 1,
        "rebuilding" => 2,
        _ => 3, // missing / unknown
    }
}

/// Trust for a member whose index could not be read. Counts stay zero so the
/// merge still sums; the freshness axis is `missing`, which ranks worst.
pub fn missing() -> Trust {
    Trust {
        trust: "missing".into(),
        coverage: "exhaustive".into(),
        resolve: "ok".into(),
        files: 0,
        nodes: 0,
        edges: 0,
    }
}

/// Fold per-repo trust into one group-level trust. Counts sum; each axis takes
/// the worst member value, so a group banner never over-promises.
/// Callers must include [`missing`] for unreadables — omitting them would let
/// a dead member hide behind the remaining `fresh` repos.
pub fn merge(parts: &[Trust]) -> Trust {
    if parts.is_empty() {
        return missing();
    }
    let worst = parts
        .iter()
        .max_by_key(|t| trust_rank(&t.trust))
        .map(|t| t.trust.clone())
        .unwrap_or_else(|| "missing".into());
    let coverage = if parts.iter().any(|t| t.coverage != "exhaustive") {
        "sampled".to_string()
    } else {
        "exhaustive".to_string()
    };
    let resolve = if parts.iter().any(|t| t.resolve != "ok") {
        "partial".to_string()
    } else {
        "ok".to_string()
    };
    Trust {
        trust: worst,
        coverage,
        resolve,
        files: parts.iter().map(|t| t.files).sum(),
        nodes: parts.iter().map(|t| t.nodes).sum(),
        edges: parts.iter().map(|t| t.edges).sum(),
    }
}

/// Group banner: the normal signature line plus the repo count, so the agent
/// can see it is reading a fan-out and not one repo.
fn banner_group(t: &Trust, repos: usize) -> String {
    format!("{}  [group: {} repos]", banner(t), repos)
}

/// One group payload: merged banner, then the per-repo body. Shared by status,
/// search, explore, and changes so the merge rule is not re-typed at each call.
pub fn render_group(parts: &[Trust], repos: usize, body: &str) -> String {
    let head = banner_group(&merge(parts), repos);
    if body.is_empty() {
        head
    } else {
        format!("{head}\n\n{}", body.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE edges (
                resolved INTEGER NOT NULL DEFAULT 0,
                kind TEXT NOT NULL,
                reason TEXT NOT NULL DEFAULT '',
                raw_name TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE nodes (name TEXT NOT NULL);",
        )
        .unwrap();
        conn
    }

    fn edge(conn: &rusqlite::Connection, kind: &str, reason: &str, resolved: i64, raw: &str) {
        conn.execute(
            "INSERT INTO edges(kind, reason, resolved, raw_name) VALUES(?1, ?2, ?3, ?4)",
            (kind, reason, resolved, raw),
        )
        .unwrap();
    }

    #[test]
    fn resolve_ok_when_in_repo_calls_resolve() {
        let conn = mem();
        edge(&conn, "calls", "import-binding", 1, "helper");
        edge(&conn, "calls", "same-file", 1, "run");
        // noise that used to drag a healthy repo under 0.6
        for _ in 0..10 {
            edge(&conn, "calls", "receiver-unknown", 0, "map");
            edge(&conn, "calls", "external-import", 0, "expect");
            edge(&conn, "calls", "unresolved", 0, "expect"); // no node named expect
        }
        assert_eq!(resolve_quality(&conn).unwrap(), "ok");
    }

    #[test]
    fn resolve_partial_when_in_repo_calls_do_not_resolve() {
        let conn = mem();
        conn.execute("INSERT INTO nodes(name) VALUES('helper')", [])
            .unwrap();
        edge(&conn, "calls", "unresolved", 0, "helper");
        edge(&conn, "calls", "unresolved", 0, "helper");
        edge(&conn, "calls", "import-binding", 1, "run");
        assert_eq!(resolve_quality(&conn).unwrap(), "partial");
    }

    fn t(trust: &str, coverage: &str, resolve: &str) -> Trust {
        Trust {
            trust: trust.into(),
            coverage: coverage.into(),
            resolve: resolve.into(),
            files: 1,
            nodes: 2,
            edges: 3,
        }
    }

    #[test]
    fn merge_takes_worst_axis_and_sums_counts() {
        let m = merge(&[
            t("fresh", "exhaustive", "ok"),
            t("stale", "sampled", "partial"),
        ]);
        assert_eq!(m.trust, "stale");
        assert_eq!(m.coverage, "sampled");
        assert_eq!(m.resolve, "partial");
        assert_eq!((m.files, m.nodes, m.edges), (2, 4, 6));
    }

    #[test]
    fn merge_ranks_missing_worst_and_empty_is_missing() {
        let m = merge(&[
            t("rebuilding", "exhaustive", "ok"),
            t("missing", "exhaustive", "ok"),
        ]);
        assert_eq!(m.trust, "missing");
        assert_eq!(merge(&[]).trust, "missing");
    }

    #[test]
    fn group_banner_shows_repo_count() {
        let b = banner_group(&t("fresh", "exhaustive", "ok"), 3);
        assert!(b.contains("[group: 3 repos]"), "{b}");
    }

    #[test]
    fn omitting_an_unreadable_member_would_hide_it_so_missing_must_be_merged() {
        // Two fresh repos look fresh. Adding the unread member as `missing`
        // is what makes the group banner honest.
        let fresh = [
            t("fresh", "exhaustive", "ok"),
            t("fresh", "exhaustive", "ok"),
        ];
        assert_eq!(merge(&fresh).trust, "fresh");
        let with_dead = [t("fresh", "exhaustive", "ok"), missing()];
        assert_eq!(merge(&with_dead).trust, "missing");
    }

    #[test]
    fn render_group_puts_banner_then_body() {
        let out = render_group(&[t("fresh", "exhaustive", "ok")], 2, "- a\n- b\n");
        assert!(out.starts_with("trust: fresh"), "{out}");
        assert!(out.contains("[group: 2 repos]"), "{out}");
        assert!(out.contains("- a\n- b"), "{out}");
    }
}
