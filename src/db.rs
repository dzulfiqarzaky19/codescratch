//! SQLite graph store (rusqlite, bundled build → static, FTS5 compiled in).
//! Schema mirrors RUST-REWRITE.md: one `edges` table with a `kind` discriminator,
//! honesty fields (`conf`, `reason`, `provenance`) present from day one.

use anyhow::Result;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: &str = "2";

/// `<root>/.codescratch/graph.db`
fn db_path(root: &Path) -> PathBuf {
    root.join(".codescratch").join("graph.db")
}

pub fn dir(root: &Path) -> PathBuf {
    root.join(".codescratch")
}

pub fn open(root: &Path) -> Result<Connection> {
    std::fs::create_dir_all(dir(root))?;
    let conn = Connection::open(db_path(root))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS files (
            path       TEXT PRIMARY KEY,
            hash       TEXT NOT NULL,
            mtime_ms   INTEGER NOT NULL,
            size       INTEGER NOT NULL,
            language   TEXT NOT NULL,
            indexed_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS nodes (
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
        CREATE INDEX IF NOT EXISTS nodes_name ON nodes(name);
        CREATE INDEX IF NOT EXISTS nodes_file ON nodes(file_path);

        CREATE TABLE IF NOT EXISTS edges (
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
        CREATE INDEX IF NOT EXISTS edges_src ON edges(src_id);
        CREATE INDEX IF NOT EXISTS edges_dst ON edges(dst_id);
        CREATE INDEX IF NOT EXISTS edges_file ON edges(file_path);

        CREATE TABLE IF NOT EXISTS bindings (
            file_path     TEXT NOT NULL,
            local_name    TEXT NOT NULL,
            source_module TEXT NOT NULL,
            imported_name TEXT NOT NULL,
            kind          TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS bindings_file ON bindings(file_path);

        CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        -- v2 (contract): attributes for route/community/process nodes without
        -- a column per kind. key = 'method'|'path'|'community'|'order'|...
        CREATE TABLE IF NOT EXISTS node_attrs (
            node_id TEXT NOT NULL,
            key     TEXT NOT NULL,
            value   TEXT NOT NULL,
            PRIMARY KEY (node_id, key)
        );
        CREATE INDEX IF NOT EXISTS node_attrs_node ON node_attrs(node_id);

        -- v2 (contract): local embeddings for hybrid search (v0.4; unused until then).
        CREATE TABLE IF NOT EXISTS embeddings (
            node_id TEXT PRIMARY KEY,
            dim     INTEGER NOT NULL,
            vec     BLOB NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
            name, qualified_name, file_path, node_id UNINDEXED
        );
        "#,
    )?;
    set_meta(conn, "schema_version", SCHEMA_VERSION)?;
    Ok(())
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (key, value),
    )?;
    Ok(())
}

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    let v = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| {
            r.get::<_, String>(0)
        })
        .ok();
    Ok(v)
}

pub fn count(conn: &Connection, table: &str) -> Result<i64> {
    let n = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
    Ok(n)
}
