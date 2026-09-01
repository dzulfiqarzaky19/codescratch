//! Walk → hash → extract → store. `ensure` dirty-gates then full-rebuilds;
//! per-file dirty ∪ importers is not this module (RUST-REWRITE.md).

use crate::model::{Edge, FileFacts, RouteFact, Symbol};
use crate::{extract, resolve, walk};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

struct Scanned {
    path: String, // repo-relative, forward slashes
    hash: String,
    mtime_ms: i64,
    size: i64,
    language: String,
    src: String,
    facts: FileFacts,
}

pub fn index_all(conn: &mut Connection, root: &Path) -> Result<()> {
    let scanned = scan(root);

    let mut symbols: Vec<Symbol> = Vec::new();
    let mut calls = Vec::new();
    let mut bindings = Vec::new();
    let mut files_set: HashSet<String> = HashSet::new();
    for s in &scanned {
        files_set.insert(s.path.clone());
    }
    for s in &scanned {
        symbols.extend(s.facts.symbols.iter().cloned());
        calls.extend(s.facts.calls.iter().cloned());
        bindings.extend(s.facts.imports.iter().cloned());
    }

    let cfg = resolve::load_config(root, &files_set);
    let mut heritage: Vec<Edge> = Vec::new();
    let mut extra: Vec<Edge> = Vec::new();
    let mut routes: Vec<RouteFact> = Vec::new();
    for s in &scanned {
        heritage.extend(s.facts.heritage.iter().cloned());
        extra.extend(extract::heuristics::extra_edges(&s.path, &s.src, &s.facts));
        routes.extend(extract::plugins::collect(&s.path, &s.src));
    }
    let mut edges =
        resolve::resolve_with_heritage(&symbols, &calls, &bindings, &heritage, &files_set, &cfg);
    for r in &mut routes {
        if let Some(name) = r
            .handler_id
            .split('#')
            .nth(1)
            .and_then(|s| s.split('@').next())
        {
            if let Some(sym) = symbols
                .iter()
                .find(|s| s.name == name && s.file_path == r.file_path)
            {
                r.handler_id = sym.id.clone();
            }
        }
    }
    edges.extend(extra);

    let now = now_ms();
    let tx = conn.transaction()?;
    tx.execute_batch(
        "DELETE FROM files; DELETE FROM nodes; DELETE FROM edges; DELETE FROM bindings; DELETE FROM nodes_fts; DELETE FROM node_attrs;",
    )?;

    {
        let mut fstmt = tx.prepare(
            "INSERT INTO files(path, hash, mtime_ms, size, language, indexed_at) VALUES(?1,?2,?3,?4,?5,?6)",
        )?;
        for s in &scanned {
            fstmt.execute((&s.path, &s.hash, s.mtime_ms, s.size, &s.language, now))?;
        }

        let mut nstmt = tx.prepare(
            "INSERT OR REPLACE INTO nodes(id,kind,name,qualified_name,file_path,start_line,end_line,exported,signature)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )?;
        let mut ftstmt = tx.prepare(
            "INSERT INTO nodes_fts(name, qualified_name, file_path, node_id) VALUES(?1,?2,?3,?4)",
        )?;
        for sym in &symbols {
            nstmt.execute((
                &sym.id,
                &sym.kind,
                &sym.name,
                &sym.qualified_name,
                &sym.file_path,
                sym.start_line as i64,
                sym.end_line as i64,
                sym.exported as i64,
                &sym.signature,
            ))?;
            ftstmt.execute((&sym.name, &sym.qualified_name, &sym.file_path, &sym.id))?;
        }

        let mut astmt =
            tx.prepare("INSERT OR REPLACE INTO node_attrs(node_id, key, value) VALUES(?1,?2,?3)")?;
        let mut estmt = tx.prepare(
            "INSERT INTO edges(src_id,dst_id,kind,raw_name,resolved,conf,reason,provenance,file_path,line)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        )?;

        for r in &routes {
            let id = format!("{}#route:{}:{}", r.file_path, r.method, r.path);
            nstmt.execute((
                &id,
                "route",
                &r.path,
                format!("{} {}", r.method, r.path),
                &r.file_path,
                r.line as i64,
                r.line as i64,
                1i64,
                format!("{} {}", r.method, r.path),
            ))?;
            ftstmt.execute((
                &r.path,
                format!("{} {}", r.method, r.path),
                &r.file_path,
                &id,
            ))?;
            astmt.execute((&id, "method", &r.method))?;
            astmt.execute((&id, "path", &r.path))?;
            estmt.execute((
                &r.handler_id,
                &id,
                "handles_route",
                format!("{} {}", r.method, r.path),
                1i64,
                "strong",
                "route-plugin",
                "ast",
                &r.file_path,
                r.line as i64,
            ))?;
        }

        let mut bstmt = tx.prepare(
            "INSERT INTO bindings(file_path, local_name, source_module, imported_name, kind) VALUES(?1,?2,?3,?4,?5)",
        )?;
        for b in &bindings {
            bstmt.execute((
                &b.file_path,
                &b.local_name,
                &b.source_module,
                &b.imported_name,
                &b.kind,
            ))?;
        }

        for e in &edges {
            estmt.execute((
                &e.src_id,
                &e.dst_id,
                &e.kind,
                &e.raw_name,
                e.resolved as i64,
                &e.conf,
                &e.reason,
                &e.provenance,
                &e.file_path,
                e.line as i64,
            ))?;
        }
    }

    crate::db::set_meta(&tx, "coverage", "exhaustive")?;
    tx.commit()?;
    Ok(())
}

/// Dirty-gate then full rebuild. Not a scoped incremental: a partial rewrite
/// would desync edges. No-op when `mtime`+`size` match.
pub fn ensure_current(conn: &mut Connection, root: &Path) -> Result<()> {
    if !is_dirty(conn, root)? {
        return Ok(());
    }
    index_all(conn, root)
}

/// True if any tracked source file was added, removed, or changed since the last
/// index, judged by `mtime`+`size` (a stat — no file read, no parse). A same-size
/// edit that also preserves mtime is not caught here, but host `ensure` rebuilds
/// on git HEAD drift too, which covers commits.
fn is_dirty(conn: &Connection, root: &Path) -> Result<bool> {
    let mut db_map: HashMap<String, (i64, i64)> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT path, mtime_ms, size FROM files")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (p, m, s) = row?;
            db_map.insert(p, (m, s));
        }
    }

    let ents = walk::entries(root);
    for e in &ents {
        match db_map.get(&e.rel) {
            Some(&(m, s)) if m == e.mtime_ms && s == e.size => {}
            _ => return Ok(true), // new or modified
        }
    }
    // a file in the DB but no longer on disk → removed → dirty
    Ok(ents.len() != db_map.len())
}

fn scan(root: &Path) -> Vec<Scanned> {
    let mut out = Vec::new();
    for e in walk::entries(root) {
        let Ok(src) = std::fs::read_to_string(&e.abs) else {
            continue;
        };
        let hash = blake3::hash(src.as_bytes()).to_hex().to_string();
        let facts = extract::extract(&e.rel, &src);
        out.push(Scanned {
            path: e.rel,
            hash,
            mtime_ms: e.mtime_ms,
            size: e.size,
            language: e.lang.as_str().to_string(),
            src,
            facts,
        });
    }
    out
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
