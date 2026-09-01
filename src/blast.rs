//! Reverse-BFS over `calls` edges: the blast radius.
//!
//! Explore and detect_changes are format adapters over this module. One walk,
//! one place the hop set is decided; callers only format.

use crate::model::NodeRow;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::{HashSet, VecDeque};

/// Shared hop cap. Explore and detect_changes both walk this far.
pub const MAX_DEPTH: usize = 4;

/// One caller hop. `node` is None when the caller id is not a graph node
/// (module-scope call site). `file_path`/`line`/`conf`/`reason` come from the
/// `calls` edge (the call site), not the caller node.
#[derive(Debug, Clone)]
pub struct Hop {
    pub depth: usize,
    pub src_id: String,
    pub node: Option<NodeRow>,
    pub file_path: String,
    pub line: i64,
    pub conf: String,
    pub reason: String,
}

/// Callers of `seeds`, walking `edges(kind='calls')` backward up to `max_depth`
/// hops. Seeds themselves are excluded. BFS order; first visit wins on diamonds.
pub fn from_ids(conn: &Connection, seeds: &[&str], max_depth: usize) -> Result<Vec<Hop>> {
    let mut seen: HashSet<String> = seeds.iter().map(|s| (*s).to_string()).collect();
    let mut q: VecDeque<(String, usize)> = seeds.iter().map(|s| ((*s).to_string(), 0)).collect();
    let mut out: Vec<Hop> = Vec::new();

    let mut stmt = conn.prepare(
        "SELECT src_id, file_path, line, conf, reason FROM edges
         WHERE dst_id=?1 AND kind='calls'",
    )?;

    while let Some((cur, depth)) = q.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let rows: Vec<(String, String, i64, String, String)> = stmt
            .query_map([&cur], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        for (src, file, line, conf, reason) in rows {
            if !seen.insert(src.clone()) {
                continue;
            }
            let d = depth + 1;
            let node = NodeRow::by_id(conn, &src);
            out.push(Hop {
                depth: d,
                src_id: src.clone(),
                node,
                file_path: file,
                line,
                conf,
                reason,
            });
            q.push_back((src, d));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE nodes (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                name TEXT NOT NULL,
                qualified_name TEXT NOT NULL,
                file_path TEXT NOT NULL,
                start_line INTEGER NOT NULL,
                end_line INTEGER NOT NULL,
                exported INTEGER NOT NULL DEFAULT 0,
                signature TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE edges (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                src_id TEXT NOT NULL,
                dst_id TEXT,
                kind TEXT NOT NULL,
                raw_name TEXT NOT NULL DEFAULT '',
                resolved INTEGER NOT NULL DEFAULT 0,
                conf TEXT NOT NULL DEFAULT 'weak',
                reason TEXT NOT NULL DEFAULT '',
                provenance TEXT NOT NULL DEFAULT 'ast',
                file_path TEXT NOT NULL,
                line INTEGER NOT NULL DEFAULT 0
            );
            "#,
        )
        .unwrap();
        conn
    }

    fn insert_fn(conn: &Connection, id: &str, name: &str, file: &str, line: i64) {
        conn.execute(
            "INSERT INTO nodes(id,kind,name,qualified_name,file_path,start_line,end_line,exported,signature)
             VALUES(?1,'function',?2,?2,?3,?4,?4,1,'')",
            (id, name, file, line),
        )
        .unwrap();
    }

    fn insert_call(
        conn: &Connection,
        src: &str,
        dst: &str,
        file: &str,
        line: i64,
        conf: &str,
        reason: &str,
    ) {
        conn.execute(
            "INSERT INTO edges(src_id,dst_id,kind,raw_name,resolved,conf,reason,provenance,file_path,line)
             VALUES(?1,?2,'calls','',1,?3,?4,'ast',?5,?6)",
            (src, dst, conf, reason, file, line),
        )
        .unwrap();
    }

    /// C calls B calls A.
    fn chain(conn: &Connection) {
        insert_fn(conn, "a", "a", "a.ts", 1);
        insert_fn(conn, "b", "b", "b.ts", 2);
        insert_fn(conn, "c", "c", "c.ts", 3);
        insert_call(conn, "b", "a", "b.ts", 10, "strong", "same-file");
        insert_call(conn, "c", "b", "c.ts", 20, "weak", "unique-global");
    }

    #[test]
    fn seeds_are_excluded_and_depths_count_hops() {
        let conn = setup();
        chain(&conn);
        let hops = from_ids(&conn, &["a"], 4).unwrap();
        assert_eq!(hops.len(), 2, "{hops:?}");
        assert_eq!(hops[0].src_id, "b");
        assert_eq!(hops[0].depth, 1);
        assert_eq!(hops[0].line, 10);
        assert_eq!(hops[0].reason, "same-file");
        assert_eq!(hops[1].src_id, "c");
        assert_eq!(hops[1].depth, 2);
        assert_eq!(hops[1].conf, "weak");
    }

    #[test]
    fn max_depth_stops_the_walk() {
        let conn = setup();
        chain(&conn);
        let hops = from_ids(&conn, &["a"], 1).unwrap();
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].src_id, "b");
    }

    #[test]
    fn diamond_first_visit_wins() {
        let conn = setup();
        insert_fn(&conn, "a", "a", "a.ts", 1);
        insert_fn(&conn, "b", "b", "b.ts", 2);
        insert_fn(&conn, "c", "c", "c.ts", 3);
        insert_fn(&conn, "d", "d", "d.ts", 4);
        // d calls both b and c; b and c both call a.
        insert_call(&conn, "b", "a", "b.ts", 1, "strong", "same-file");
        insert_call(&conn, "c", "a", "c.ts", 1, "strong", "same-file");
        insert_call(&conn, "d", "b", "d.ts", 1, "strong", "same-file");
        insert_call(&conn, "d", "c", "d.ts", 2, "strong", "same-file");
        let hops = from_ids(&conn, &["a"], 4).unwrap();
        let d_hits: Vec<_> = hops.iter().filter(|h| h.src_id == "d").collect();
        assert_eq!(d_hits.len(), 1, "diamond must not emit d twice: {hops:?}");
        assert_eq!(d_hits[0].depth, 2);
    }

    #[test]
    fn missing_node_still_emits_a_hop() {
        let conn = setup();
        insert_fn(&conn, "a", "a", "a.ts", 1);
        insert_call(&conn, "ghost", "a", "mod.ts", 4, "weak", "unresolved");
        let hops = from_ids(&conn, &["a"], 4).unwrap();
        assert_eq!(hops.len(), 1);
        assert!(hops[0].node.is_none());
        assert_eq!(hops[0].src_id, "ghost");
        assert_eq!(hops[0].file_path, "mod.ts");
        assert_eq!(hops[0].line, 4);
    }

    #[test]
    fn empty_seeds_or_no_callers_is_empty() {
        let conn = setup();
        insert_fn(&conn, "a", "a", "a.ts", 1);
        assert!(from_ids(&conn, &["a"], 4).unwrap().is_empty());
        assert!(from_ids(&conn, &[], 4).unwrap().is_empty());
    }
}
