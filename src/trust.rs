//! The moat: three orthogonal freshness axes, rendered as the first line of
//! every explore/status payload. Ported from codescratch `src/query/trust.ts`
//! + `format.ts`. Neither parent tool has this.

use crate::{db, host};
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

pub struct Trust {
    pub trust: String,    // fresh | stale | rebuilding | missing
    pub coverage: String, // exhaustive | sampled
    pub graph: String,    // ok | degraded
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
        match (indexed, host::git_head(root)) {
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
    let coverage = db::get_meta(conn, "coverage")?
        .unwrap_or_else(|| "exhaustive".to_string());

    // --- graph axis --- resolution quality, not freshness
    let resolved: i64 = conn
        .query_row("SELECT COUNT(*) FROM edges WHERE resolved = 1", [], |r| r.get(0))
        .unwrap_or(0);
    let graph = if edges == 0 {
        "ok"
    } else if (resolved as f64) / (edges as f64) < 0.6 {
        "degraded"
    } else {
        "ok"
    }
    .to_string();

    Ok(Trust { trust, coverage, graph, files, nodes, edges })
}

/// The signature line. Always first.
pub fn banner(t: &Trust) -> String {
    format!(
        "trust: {} · coverage: {} · graph: {}  ({} files, {} symbols, {} edges)",
        t.trust, t.coverage, t.graph, t.files, t.nodes, t.edges
    )
}
