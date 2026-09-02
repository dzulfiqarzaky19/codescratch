//! Community + process materialization. Turns the `calls` graph into two
//! agent-facing conveniences. Explore reads these via `member_of` / `step_in`.
//!
//! Both are heuristic aggregations of AST facts: `provenance` is always
//! `heuristic`, and `conf` never upgrades a weak edge (`aggregate_conf`).
//! `materialize` is idempotent.

mod communities;
mod processes;

use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeSet;

/// One `calls` edge, reduced to what community/process computation needs.
pub(crate) struct CallEdge {
    pub src: String,
    pub dst: String,
    pub conf: String, // strong | weak — drives aggregate_conf on the derived node
}

/// Materialize community + process nodes/edges from the current `calls` graph.
/// Idempotent: deletes prior `community`/`process` nodes + their `member_of`/
/// `step_in` edges + their node_attrs first, then recomputes. Call AFTER the
/// index has written symbols and resolved `calls` edges.
pub fn materialize(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;

    tx.execute_batch(
        "DELETE FROM node_attrs WHERE node_id IN (SELECT id FROM nodes WHERE kind IN ('community','process'));
         DELETE FROM edges WHERE kind IN ('member_of','step_in');
         DELETE FROM nodes WHERE kind IN ('community','process');",
    )?;

    let mut symbol_ids: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT id FROM nodes WHERE kind IN ('function','class','method') ORDER BY id",
        )?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        ids
    };
    symbol_ids.sort();

    let all_calls: Vec<CallEdge> = {
        let mut stmt = tx.prepare(
            "SELECT src_id, dst_id, conf FROM edges
             WHERE kind='calls' AND resolved=1 AND dst_id IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(CallEdge {
                src: r.get(0)?,
                dst: r.get(1)?,
                conf: r.get(2)?,
            })
        })?;
        rows.filter_map(|r| r.ok()).collect()
    };

    let symbol_set: BTreeSet<&str> = symbol_ids.iter().map(|s| s.as_str()).collect();
    let calls: Vec<&CallEdge> = all_calls
        .iter()
        .filter(|e| symbol_set.contains(e.src.as_str()) && symbol_set.contains(e.dst.as_str()))
        .collect();

    let communities = communities::compute(&symbol_ids, &calls);
    let processes = processes::compute(&symbol_ids, &calls);

    communities::write(&tx, &communities)?;
    processes::write(&tx, &processes)?;

    tx.commit()?;
    Ok(())
}

/// `strong` only if every contributing edge was `strong`; `weak` if any was.
/// Honesty rule: never upgrade a weak fact by aggregating it.
pub(crate) fn aggregate_conf<'a, I: IntoIterator<Item = &'a str>>(confs: I) -> &'static str {
    for c in confs {
        if c != "strong" {
            return "weak";
        }
    }
    "strong"
}

/// Resolve a hub member id to (qualified_name, file_path).
pub(crate) fn hub_label(conn: &Connection, hub_id: &str) -> (String, String) {
    conn.query_row(
        "SELECT qualified_name, file_path FROM nodes WHERE id=?1",
        [hub_id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )
    .unwrap_or_else(|_| (hub_id.to_string(), String::new()))
}

pub(crate) fn symbol_file(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row("SELECT file_path FROM nodes WHERE id=?1", [id], |r| {
        r.get::<_, String>(0)
    })
    .ok()
}

// ================================== tests ===================================

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE nodes (
                id             TEXT PRIMARY KEY,
                kind           TEXT NOT NULL,
                name           TEXT NOT NULL,
                qualified_name TEXT NOT NULL,
                file_path      TEXT NOT NULL,
                start_line     INTEGER NOT NULL,
                end_line       INTEGER NOT NULL,
                exported       INTEGER NOT NULL DEFAULT 0,
                signature      TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE edges (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                src_id     TEXT NOT NULL,
                dst_id     TEXT,
                kind       TEXT NOT NULL,
                raw_name   TEXT NOT NULL DEFAULT '',
                resolved   INTEGER NOT NULL DEFAULT 0,
                conf       TEXT NOT NULL DEFAULT 'weak',
                reason     TEXT NOT NULL DEFAULT '',
                provenance TEXT NOT NULL DEFAULT 'ast',
                file_path  TEXT NOT NULL,
                line       INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE node_attrs (
                node_id TEXT NOT NULL,
                key     TEXT NOT NULL,
                value   TEXT NOT NULL,
                PRIMARY KEY (node_id, key)
            );
            "#,
        )
        .unwrap();
        conn
    }

    fn insert_symbol(conn: &Connection, id: &str, name: &str, file: &str) {
        conn.execute(
            "INSERT INTO nodes(id,kind,name,qualified_name,file_path,start_line,end_line,exported,signature)
             VALUES(?1,'function',?2,?2,?3,1,10,0,'')",
            (id, name, file),
        )
        .unwrap();
    }

    fn insert_call(conn: &Connection, src: &str, dst: &str, conf: &str) {
        conn.execute(
            "INSERT INTO edges(src_id,dst_id,kind,raw_name,resolved,conf,reason,provenance,file_path,line)
             VALUES(?1,?2,'calls','',1,?3,'same-file','ast','f.ts',1)",
            (src, dst, conf),
        )
        .unwrap();
    }

    /// Builds:
    ///   community trio: a <-> b <-> c <-> a (triangle, tightly connected)
    ///   chain: e0 -> e1 -> e2 -> e3 (entrypoint e0 has no incoming calls)
    /// `d` is an isolated symbol (no calls at all) — should not form a community
    /// and should not become a process entrypoint (no outgoing calls).
    fn build_graph(conn: &Connection) {
        insert_symbol(conn, "f.ts#a@1", "a", "f.ts");
        insert_symbol(conn, "f.ts#b@2", "b", "f.ts");
        insert_symbol(conn, "f.ts#c@3", "c", "f.ts");
        insert_symbol(conn, "f.ts#d@4", "d", "f.ts");
        insert_call(conn, "f.ts#a@1", "f.ts#b@2", "strong");
        insert_call(conn, "f.ts#b@2", "f.ts#c@3", "strong");
        insert_call(conn, "f.ts#c@3", "f.ts#a@1", "strong");

        insert_symbol(conn, "f.ts#e0@10", "e0", "f.ts");
        insert_symbol(conn, "f.ts#e1@11", "e1", "f.ts");
        insert_symbol(conn, "f.ts#e2@12", "e2", "f.ts");
        insert_symbol(conn, "f.ts#e3@13", "e3", "f.ts");
        insert_call(conn, "f.ts#e0@10", "f.ts#e1@11", "strong");
        insert_call(conn, "f.ts#e1@11", "f.ts#e2@12", "strong");
        insert_call(conn, "f.ts#e2@12", "f.ts#e3@13", "weak");
    }

    #[test]
    fn materializes_community_with_correct_members() {
        let mut conn = setup();
        build_graph(&conn);
        materialize(&mut conn).unwrap();

        let comm_id: String = conn
            .query_row("SELECT id FROM nodes WHERE kind='community'", [], |r| {
                r.get(0)
            })
            .unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT src_id FROM edges WHERE kind='member_of' AND dst_id=?1 ORDER BY src_id",
            )
            .unwrap();
        let members: Vec<String> = stmt
            .query_map([&comm_id], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert_eq!(
            members,
            vec![
                "f.ts#a@1".to_string(),
                "f.ts#b@2".to_string(),
                "f.ts#c@3".to_string()
            ]
        );

        let size: String = conn
            .query_row(
                "SELECT value FROM node_attrs WHERE node_id=?1 AND key='community_size'",
                [&comm_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(size, "3");

        // `d` is isolated — no community should include it, and only one
        // community should exist overall (the e-chain has no cycle to cluster).
        let comm_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM nodes WHERE kind='community'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(comm_count, 1);
    }

    #[test]
    fn materializes_process_with_ordered_steps() {
        let mut conn = setup();
        build_graph(&conn);
        materialize(&mut conn).unwrap();

        let proc_id: String = conn
            .query_row(
                "SELECT id FROM nodes WHERE kind='process' AND name='flow:e0'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT src_id, line FROM edges WHERE kind='step_in' AND dst_id=?1 ORDER BY line",
            )
            .unwrap();
        let steps: Vec<(String, i64)> = stmt
            .query_map([&proc_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert_eq!(
            steps,
            vec![
                ("f.ts#e0@10".to_string(), 0),
                ("f.ts#e1@11".to_string(), 1),
                ("f.ts#e2@12".to_string(), 2),
                ("f.ts#e3@13".to_string(), 3),
            ]
        );

        // One edge in the chain was weak → the process conf must be weak
        // (never upgraded by aggregation).
        let conf: String = conn
            .query_row(
                "SELECT conf FROM edges WHERE kind='step_in' AND dst_id=?1 LIMIT 1",
                [&proc_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(conf, "weak");
    }

    #[test]
    fn materialize_is_idempotent() {
        let mut conn = setup();
        build_graph(&conn);
        materialize(&mut conn).unwrap();
        materialize(&mut conn).unwrap();
        materialize(&mut conn).unwrap();

        let comm_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM nodes WHERE kind='community'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let proc_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM nodes WHERE kind='process'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let member_edges: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE kind='member_of'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let step_edges: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges WHERE kind='step_in'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let attrs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM node_attrs WHERE key='community_size'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        assert_eq!(comm_count, 1);
        assert_eq!(proc_count, 1);
        assert_eq!(member_edges, 3);
        assert_eq!(step_edges, 4);
        assert_eq!(attrs, 1);
    }

    #[test]
    fn isolated_symbol_is_not_an_entrypoint_or_community_member() {
        let mut conn = setup();
        build_graph(&conn);
        materialize(&mut conn).unwrap();

        let touches_d: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE (src_id='f.ts#d@4' OR dst_id='f.ts#d@4')
                 AND kind IN ('member_of','step_in')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(touches_d, 0);
    }
}
