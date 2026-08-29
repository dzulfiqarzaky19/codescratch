//! Walk → hash → extract → store. v0.1 does a full rebuild each `ensure`
//! (correct and simple). Incremental dirty+importers is v0.2 (RUST-REWRITE.md).

use crate::model::{Edge, FileFacts, RouteFact, Symbol};
use crate::{extract, resolve};
use anyhow::Result;
use ignore::WalkBuilder;
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
    let mut edges = resolve::resolve_with(&symbols, &calls, &bindings, &files_set, &cfg);

    let mut extra: Vec<Edge> = Vec::new();
    let mut routes: Vec<RouteFact> = Vec::new();
    for s in &scanned {
        extra.extend(extract::extra_ast_edges(&s.path, &s.src, &s.facts));
        extra.extend(extract::heuristics::extra_edges(&s.path, &s.src, &s.facts));
        routes.extend(extract::plugins::collect(&s.path, &s.src));
    }
    for r in &mut routes {
        if let Some(name) = r.handler_id.split('#').nth(1).and_then(|s| s.split('@').next()) {
            if let Some(sym) = symbols.iter().find(|s| s.name == name && s.file_path == r.file_path) {
                r.handler_id = sym.id.clone();
            }
        }
    }
    resolve_heritage(&mut extra, &symbols);
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
                &sym.id, &sym.kind, &sym.name, &sym.qualified_name, &sym.file_path,
                sym.start_line as i64, sym.end_line as i64, sym.exported as i64, &sym.signature,
            ))?;
            ftstmt.execute((&sym.name, &sym.qualified_name, &sym.file_path, &sym.id))?;
        }

        let mut astmt = tx.prepare(
            "INSERT OR REPLACE INTO node_attrs(node_id, key, value) VALUES(?1,?2,?3)",
        )?;
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
            ftstmt.execute((&r.path, format!("{} {}", r.method, r.path), &r.file_path, &id))?;
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
            bstmt.execute((&b.file_path, &b.local_name, &b.source_module, &b.imported_name, &b.kind))?;
        }

        for e in &edges {
            estmt.execute((
                &e.src_id, &e.dst_id, &e.kind, &e.raw_name, e.resolved as i64,
                &e.conf, &e.reason, &e.provenance, &e.file_path, e.line as i64,
            ))?;
        }
    }

    crate::db::set_meta(&tx, "coverage", "exhaustive")?;
    tx.commit()?;
    Ok(())
}

fn resolve_heritage(edges: &mut [Edge], symbols: &[Symbol]) {
    let mut by_name: HashMap<&str, Vec<&Symbol>> = HashMap::new();
    for s in symbols {
        by_name.entry(s.name.as_str()).or_default().push(s);
    }
    for e in edges.iter_mut() {
        if e.kind != "extends" && e.kind != "implements" {
            continue;
        }
        let simple = e.raw_name.rsplit('.').next().unwrap_or(&e.raw_name);
        if let Some(cands) = by_name.get(simple) {
            if cands.len() == 1 {
                e.dst_id = Some(cands[0].id.clone());
                e.resolved = true;
                e.conf = "weak".into();
                e.reason = "unique-global".into();
            } else if let Some(same) = cands.iter().find(|s| s.file_path == e.file_path) {
                e.dst_id = Some(same.id.clone());
                e.resolved = true;
                e.conf = "strong".into();
                e.reason = "same-file".into();
            }
        }
    }
}

/// Incremental update (WP-2B). Sound design: a cheap `mtime`+`size` dirty-gate
/// short-circuits to a **no-op** when nothing changed — the common `ensure` case
/// (SessionStart/PostToolUse with untouched files) — and does a full rebuild when
/// anything changed. Never leaves a partial/desynced graph; per-file scoped
/// rebuild (dirty ∪ importers + orphan sweep) is a later refinement.
pub fn index_incremental(conn: &mut Connection, root: &Path, _changed: &[String]) -> Result<()> {
    if !is_dirty(conn, root)? {
        return Ok(()); // graph already current — skip the re-parse entirely
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
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
        })?;
        for row in rows {
            let (p, m, s) = row?;
            db_map.insert(p, (m, s));
        }
    }

    let mut seen = 0usize;
    for entry in WalkBuilder::new(root).hidden(false).build() {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.starts_with(".codescratch/") || crate::model::Lang::from_path(&rel).is_none() {
            continue;
        }
        seen += 1;
        let meta = std::fs::metadata(entry.path()).ok();
        let mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let size = meta.as_ref().map(|m| m.len() as i64).unwrap_or(0);
        match db_map.get(&rel) {
            Some(&(m, s)) if m == mtime && s == size => {}
            _ => return Ok(true), // new or modified
        }
    }
    // a file in the DB but no longer on disk → removed → dirty
    Ok(seen != db_map.len())
}

fn scan(root: &Path) -> Vec<Scanned> {
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
        if rel.starts_with(".codescratch/") {
            continue;
        }
        let Some(lang) = crate::model::Lang::from_path(&rel) else { continue };
        let Ok(src) = std::fs::read_to_string(abs) else { continue };
        let meta = std::fs::metadata(abs).ok();
        let mtime_ms = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let size = meta.as_ref().map(|m| m.len() as i64).unwrap_or(0);
        let hash = blake3::hash(src.as_bytes()).to_hex().to_string();
        let facts = extract::extract(&rel, &src);
        out.push(Scanned {
            path: rel,
            hash,
            mtime_ms,
            size,
            language: lang.as_str().to_string(),
            src,
            facts,
        });
    }
    out
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}
