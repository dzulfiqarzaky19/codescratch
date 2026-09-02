//! Read-side: fat `explore` payload (the product), plus `status` + `search`.
//!
//! Explore payload v2 (frozen section order):
//!   banner → node+snippet → call-path spine → members/heritage
//!   → depth-grouped blast → routes/processes
//! Weak edges stay labeled. Absence ≠ proof.

use crate::model::NodeRow;
use crate::{db, explore as explore_mod, trust};
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

fn node_by_id(conn: &Connection, id: &str) -> Option<NodeRow> {
    NodeRow::by_id(conn, id)
}

pub fn status(root: &Path) -> Result<String> {
    let conn = db::open(root)?;
    let t = trust::compute(&conn, root)?;
    Ok(trust::banner(&t))
}

/// Short repo label for group output: the root directory name.
pub fn repo_label(root: &Path) -> String {
    root.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| root.display().to_string())
}

/// One repo's status without the banner formatting, so group callers can fold
/// the axes themselves (worst-wins) instead of concatenating banners.
pub fn trust_of(root: &Path) -> Result<trust::Trust> {
    let conn = db::open(root)?;
    trust::compute(&conn, root)
}

/// Trust for a member, or [`trust::missing`] if the index cannot be read.
/// Group callers must use this so a dead repo cannot hide behind the rest.
pub(crate) fn trust_or_missing(root: &Path) -> (trust::Trust, Option<String>) {
    match trust_of(root) {
        Ok(t) => (t, None),
        Err(e) => (trust::missing(), Some(e.to_string())),
    }
}

pub fn search(root: &Path, q: &str) -> Result<String> {
    let conn = db::open(root)?;
    let t = trust::compute(&conn, root)?;
    let mut out = trust::banner(&t);
    out.push_str("\n\n");
    out.push_str(&search_body(&conn, q, None)?);
    Ok(out)
}

/// Hits for one repo, optionally prefixed with a group label.
pub(crate) fn search_hits(root: &Path, q: &str, label: Option<&str>) -> Result<String> {
    let conn = db::open(root)?;
    search_body(&conn, q, label)
}

/// Shared hit list for both single-repo and group search.
/// Hybrid: RRF-fuse FTS with local embedding similarity. Falls back to
/// FTS-only when the embeddings table is empty (see embeddings.rs).
fn search_body(conn: &Connection, q: &str, label: Option<&str>) -> Result<String> {
    let ids = crate::embeddings::hybrid_search(conn, q, 25)?;
    let prefix = label.map(|l| format!("[{l}] ")).unwrap_or_default();
    if ids.is_empty() {
        return Ok(match label {
            Some(l) => format!("[{l}] no symbol matching `{q}`.\n"),
            None => format!("no symbol matching `{q}`."),
        });
    }
    let mut out = String::new();
    for id in ids {
        if let Some(n) = node_by_id(conn, &id) {
            let star = if n.exported { "★" } else { " " };
            out.push_str(&format!(
                "{prefix}{star} {} {}  {}:{}\n",
                n.kind, n.qualified_name, n.file_path, n.start_line
            ));
        }
    }
    Ok(out)
}

/// One repo's answer to an explore. The **variant** carries found-vs-missing;
/// callers must never re-derive that by searching the rendered text.
pub enum Explored {
    Found(explore_mod::ExploreView),
    Missing { suggestions: Vec<String> },
}

pub fn explore(root: &Path, symbol: &str) -> Result<String> {
    let banner = trust::banner(&trust_of(root)?);
    match explore_one(root, symbol)? {
        Explored::Found(view) => Ok(format!("{banner}\n\n{}", explore_mod::render_view(&view))),
        Explored::Missing { suggestions } => {
            let mut out = format!(
                "{banner}\n\nno symbol named `{symbol}`. try `search {symbol}` for fuzzy matches."
            );
            if !suggestions.is_empty() {
                out.push_str("\n\n**nearby**\n");
                for s in &suggestions {
                    out.push_str(&format!("- {s}\n"));
                }
            }
            Ok(out)
        }
    }
}

/// The explore payload for one repo, minus the banner. Returns [`Explored::Missing`]
/// when no node carries that name — the single place found-vs-missing is decided.
pub fn explore_one(root: &Path, symbol: &str) -> Result<Explored> {
    let conn = db::open(root)?;

    let node = conn
        .query_row(
            "SELECT id,kind,name,qualified_name,file_path,start_line,end_line,exported,signature
             FROM nodes WHERE name=?1 ORDER BY exported DESC, start_line ASC LIMIT 1",
            [symbol],
            NodeRow::from_row,
        )
        .ok();

    let Some(n) = node else {
        let suggestions = search_body(&conn, symbol, None)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with("no symbol matching"))
            .map(|s| s.to_string())
            .take(8)
            .collect();
        return Ok(Explored::Missing { suggestions });
    };

    Ok(Explored::Found(explore_mod::gather(&conn, root, n)))
}
