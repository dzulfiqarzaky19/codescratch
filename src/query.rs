//! Read-side: fat `explore` payload (the product), plus `status` + `search`.
//!
//! Explore payload v2 (frozen section order):
//!   banner → node+snippet → call-path spine → members/heritage
//!   → depth-grouped blast → routes/processes
//! Weak edges stay labeled. Absence ≠ proof.

use crate::{db, trust};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::{HashSet, VecDeque};
use std::path::Path;

const SNIPPET_BUDGET: usize = 1600; // bytes of verbatim source

struct NodeRow {
    id: String,
    kind: String,
    name: String,
    qualified_name: String,
    file_path: String,
    start_line: i64,
    end_line: i64,
    exported: bool,
    signature: String,
}

fn node_by_id(conn: &Connection, id: &str) -> Option<NodeRow> {
    conn.query_row(
        "SELECT id,kind,name,qualified_name,file_path,start_line,end_line,exported,signature
         FROM nodes WHERE id=?1",
        [id],
        row_to_node,
    )
    .ok()
}

fn row_to_node(r: &rusqlite::Row) -> rusqlite::Result<NodeRow> {
    Ok(NodeRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        name: r.get(2)?,
        qualified_name: r.get(3)?,
        file_path: r.get(4)?,
        start_line: r.get(5)?,
        end_line: r.get(6)?,
        exported: r.get::<_, i64>(7)? != 0,
        signature: r.get(8)?,
    })
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
fn trust_or_missing(root: &Path) -> (trust::Trust, Option<String>) {
    match trust_of(root) {
        Ok(t) => (t, None),
        Err(e) => (trust::missing(), Some(e.to_string())),
    }
}

/// Fan-out status: one merged banner + one line per member repo.
pub fn status_group(roots: &[std::path::PathBuf]) -> Result<String> {
    let mut parts = Vec::new();
    let mut lines = String::new();
    for root in roots {
        let label = repo_label(root);
        let (t, err) = trust_or_missing(root);
        match err {
            None => lines.push_str(&format!("- {label}: {}\n", trust::banner(&t))),
            Some(e) => lines.push_str(&format!("- {label}: unavailable ({e})\n")),
        }
        parts.push(t);
    }
    Ok(trust::render_group(&parts, roots.len(), &lines))
}

pub fn search(root: &Path, q: &str) -> Result<String> {
    let conn = db::open(root)?;
    let t = trust::compute(&conn, root)?;
    let mut out = trust::banner(&t);
    out.push_str("\n\n");
    out.push_str(&search_body(&conn, q, None)?);
    Ok(out)
}

/// Fan-out search: every member repo is queried, hits are prefixed with the
/// repo label. Ranking stays per-repo (scores are not comparable across
/// separate FTS/embedding indexes).
pub fn search_group(roots: &[std::path::PathBuf], q: &str) -> Result<String> {
    let mut parts = Vec::new();
    let mut body = String::new();
    for root in roots {
        let label = repo_label(root);
        let (t, err) = trust_or_missing(root);
        parts.push(t);
        if let Some(e) = err {
            body.push_str(&format!("- {label}: unavailable ({e})\n"));
            continue;
        }
        match db::open(root).and_then(|conn| search_body(&conn, q, Some(&label))) {
            Ok(s) => body.push_str(&s),
            Err(e) => body.push_str(&format!("- {label}: error ({e})\n")),
        }
    }
    Ok(trust::render_group(&parts, roots.len(), &body))
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
/// callers must never re-derive that by searching the rendered text, because
/// `Found` embeds verbatim source that can contain any sentence we might emit.
pub enum Explored {
    /// Payload body **without** the trust banner, so a group caller can put its
    /// own merged banner on top without splitting a string back apart.
    Found(String),
    Missing,
}

/// Fan-out explore: the full payload from every repo that defines the symbol.
/// Cross-repo call edges do not exist (each index is scoped to its own root),
/// so this is a union of per-repo answers, explicitly labeled.
pub fn explore_group(roots: &[std::path::PathBuf], symbol: &str) -> Result<String> {
    let mut parts = Vec::new();
    let mut hits = Vec::new();
    let mut misses = Vec::new();
    for root in roots {
        let label = repo_label(root);
        let (t, _) = trust_or_missing(root);
        parts.push(t);
        match explore_one(root, symbol) {
            Ok(Explored::Found(body)) => hits.push(format!("# repo `{label}`\n\n{body}")),
            Ok(Explored::Missing) => misses.push(label),
            Err(e) => misses.push(format!("{label} (error: {e})")),
        }
    }
    let mut body = String::new();
    if hits.is_empty() {
        body.push_str(&format!(
            "no symbol named `{symbol}` in any group repo. try `search {symbol}`.\n"
        ));
    } else {
        body.push_str(&hits.join("\n---\n\n"));
    }
    if !misses.is_empty() {
        body.push_str(&format!("\nnot found in: {}\n", misses.join(", ")));
    }
    Ok(trust::render_group(&parts, roots.len(), &body))
}

pub fn explore(root: &Path, symbol: &str) -> Result<String> {
    let banner = trust::banner(&trust_of(root)?);
    match explore_one(root, symbol)? {
        Explored::Found(body) => Ok(format!("{banner}\n\n{body}")),
        Explored::Missing => Ok(format!(
            "{banner}\n\nno symbol named `{symbol}`. try `search {symbol}` for fuzzy matches."
        )),
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
            row_to_node,
        )
        .ok();

    let Some(n) = node else {
        return Ok(Explored::Missing);
    };

    let mut out = String::new();
    // 1. node + snippet
    out.push_str(&format!(
        "## {} `{}`  ({}:{}-{}){}\n",
        n.kind,
        n.qualified_name,
        n.file_path,
        n.start_line,
        n.end_line,
        if n.exported { "  [exported]" } else { "" }
    ));
    if !n.signature.is_empty() {
        out.push_str(&format!("`{}`\n", n.signature));
    }
    if let Some(code) = read_lines(root, &n.file_path, n.start_line, n.end_line) {
        out.push_str("\n```\n");
        out.push_str(&budget(&code, SNIPPET_BUDGET));
        out.push_str("\n```\n");
    }

    // 2. call-path spine
    out.push_str("\n**call-path spine**\n");
    let spine = call_path_spine(&conn, &n.id);
    if spine.is_empty() {
        out.push_str("- (leaf — no named callees)\n");
    } else {
        for s in &spine {
            out.push_str(&format!("- {s}\n"));
        }
    }

    // 3. members / heritage
    let members = child_symbols(&conn, &n.id);
    if !members.is_empty() {
        out.push_str("\n**members**\n");
        for m in &members {
            out.push_str(&format!("- {} `{}`  :{}\n", m.kind, m.name, m.start_line));
        }
    }
    let heritage = heritage_out(&conn, &n.id);
    if !heritage.is_empty() {
        out.push_str("\n**heritage**\n");
        for h in &heritage {
            out.push_str(&format!("- {h}\n"));
        }
    }

    // 4. depth-grouped blast
    out.push_str("\n**callers ← (blast radius)**\n");
    let blast = blast_by_depth(&conn, &n.id, 4);
    if blast.is_empty() {
        out.push_str("- (no resolved callers — absence ≠ proof; weak/dynamic calls may be missed)\n");
    } else {
        for (depth, rows) in blast {
            out.push_str(&format!("depth {depth}:\n"));
            for r in rows {
                out.push_str(&format!("- {r}\n"));
            }
        }
    }

    // also keep a flat calls → section so golden's callee check still hits
    out.push_str("\n**calls →**\n");
    let callees = edges_out(&conn, &n.id, "calls");
    if callees.is_empty() {
        out.push_str("- (none captured)\n");
    }
    for c in &callees {
        out.push_str(&format!("- {c}\n"));
    }

    // 5. routes / processes
    let routes = routes_touching(&conn, &n.id);
    out.push_str("\n**routes / processes**\n");
    if routes.is_empty() {
        out.push_str("- (none)\n");
    } else {
        for r in &routes {
            out.push_str(&format!("- {r}\n"));
        }
    }

    Ok(Explored::Found(out))
}

fn child_symbols(conn: &Connection, id: &str) -> Vec<NodeRow> {
    let sql = "SELECT n.id,n.kind,n.name,n.qualified_name,n.file_path,n.start_line,n.end_line,n.exported,n.signature
               FROM edges e JOIN nodes n ON n.id = e.dst_id
               WHERE e.src_id=?1 AND e.kind='contains' ORDER BY n.start_line";
    let Ok(mut stmt) = conn.prepare(sql) else { return vec![] };
    stmt.query_map([id], row_to_node)
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

fn edges_out(conn: &Connection, id: &str, kind: &str) -> Vec<String> {
    let sql = "SELECT raw_name, dst_id, resolved, conf, reason FROM edges
               WHERE src_id=?1 AND kind=?2 ORDER BY line";
    let Ok(mut stmt) = conn.prepare(sql) else { return vec![] };
    let rows = stmt.query_map(rusqlite::params![id, kind], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, i64>(2)? != 0,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    });
    let Ok(rows) = rows else { return vec![] };
    rows.filter_map(|r| r.ok())
        .map(|(raw, dst, resolved, conf, reason)| {
            let target = dst
                .as_deref()
                .and_then(|d| node_by_id(conn, d))
                .map(|nd| format!("{} ({}:{})", nd.qualified_name, nd.file_path, nd.start_line))
                .unwrap_or_else(|| format!("`{raw}`"));
            let mark = if !resolved {
                "  ⟨unresolved⟩"
            } else if conf == "weak" {
                "  ⟨weak⟩"
            } else {
                ""
            };
            format!("{target}  [{reason}]{mark}")
        })
        .collect()
}

fn heritage_out(conn: &Connection, id: &str) -> Vec<String> {
    let sql = "SELECT kind, raw_name, dst_id, conf, reason FROM edges
               WHERE src_id=?1 AND kind IN ('extends','implements') ORDER BY kind";
    let Ok(mut stmt) = conn.prepare(sql) else { return vec![] };
    stmt.query_map([id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })
    .map(|it| {
        it.filter_map(|r| r.ok())
            .map(|(kind, raw, dst, conf, reason)| {
                let target = dst
                    .as_deref()
                    .and_then(|d| node_by_id(conn, d))
                    .map(|n| n.qualified_name)
                    .unwrap_or(raw);
                let mark = if conf == "weak" { "  ⟨weak⟩" } else { "" };
                format!("{kind} {target}  [{reason}]{mark}")
            })
            .collect()
    })
    .unwrap_or_default()
}

fn call_path_spine(conn: &Connection, id: &str) -> Vec<String> {
    // Walk named callees up to 4 hops. ≤1 unnamed (`<anon>` / unresolved) bridge.
    let mut path: Vec<String> = Vec::new();
    let mut cur = id.to_string();
    let mut seen = HashSet::new();
    seen.insert(cur.clone());
    let mut unnamed = 0usize;
    for _ in 0..4 {
        let sql = "SELECT dst_id, raw_name, resolved FROM edges
                   WHERE src_id=?1 AND kind='calls' ORDER BY line LIMIT 1";
        let row: Option<(Option<String>, String, i64)> = conn
            .query_row(sql, [&cur], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .ok();
        let Some((dst, raw, resolved)) = row else { break };
        let label = dst
            .as_deref()
            .and_then(|d| node_by_id(conn, d))
            .map(|n| n.qualified_name)
            .unwrap_or_else(|| raw.clone());
        let unnamed_hop = dst.is_none() || resolved == 0 || label.starts_with('<');
        if unnamed_hop {
            unnamed += 1;
            if unnamed > 1 {
                break;
            }
        }
        path.push(format!("{label}  `{raw}`"));
        match dst {
            Some(d) if seen.insert(d.clone()) => cur = d,
            _ => break,
        }
    }
    if path.is_empty() {
        vec![]
    } else {
        let start = node_by_id(conn, id)
            .map(|n| n.qualified_name)
            .unwrap_or_else(|| id.to_string());
        vec![format!("{} → {}", start, path.join(" → "))]
    }
}

fn blast_by_depth(conn: &Connection, id: &str, max: usize) -> Vec<(usize, Vec<String>)> {
    let mut seen = HashSet::new();
    seen.insert(id.to_string());
    let mut q: VecDeque<(String, usize)> = VecDeque::new();
    q.push_back((id.to_string(), 0));
    let mut buckets: Vec<(usize, Vec<String>)> = Vec::new();
    while let Some((cur, depth)) = q.pop_front() {
        if depth >= max {
            continue;
        }
        let sql = "SELECT src_id, file_path, line, conf, reason FROM edges
                   WHERE dst_id=?1 AND kind='calls'";
        let Ok(mut stmt) = conn.prepare(sql) else { continue };
        let rows = stmt.query_map([&cur], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        });
        let Ok(rows) = rows else { continue };
        for (src, file, line, conf, reason) in rows.filter_map(|r| r.ok()) {
            if !seen.insert(src.clone()) {
                continue;
            }
            let who = node_by_id(conn, &src)
                .map(|nd| nd.qualified_name)
                .unwrap_or_else(|| "<module>".to_string());
            let mark = if conf == "weak" { "  ⟨weak⟩" } else { "" };
            let d = depth + 1;
            let line_s = format!("{who}  {file}:{line}  [{reason}]{mark}");
            if let Some((_, rows)) = buckets.iter_mut().find(|(dd, _)| *dd == d) {
                rows.push(line_s);
            } else {
                buckets.push((d, vec![line_s]));
            }
            q.push_back((src, d));
        }
    }
    buckets.sort_by_key(|(d, _)| *d);
    buckets
}

fn routes_touching(conn: &Connection, id: &str) -> Vec<String> {
    let sql = "SELECT n.qualified_name, n.file_path, n.start_line, e.kind
               FROM edges e JOIN nodes n ON n.id = e.dst_id
               WHERE e.src_id=?1 AND e.kind IN ('handles_route','step_in','member_of')
               UNION
               SELECT n.qualified_name, n.file_path, n.start_line, e.kind
               FROM edges e JOIN nodes n ON n.id = e.src_id
               WHERE e.dst_id=?1 AND e.kind IN ('handles_route','step_in','member_of')";
    let Ok(mut stmt) = conn.prepare(sql) else { return vec![] };
    stmt.query_map([id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
        ))
    })
    .map(|it| {
        it.filter_map(|r| r.ok())
            .map(|(qn, file, line, kind)| format!("{kind}  {qn}  {file}:{line}"))
            .collect()
    })
    .unwrap_or_default()
}

fn read_lines(root: &Path, rel: &str, start: i64, end: i64) -> Option<String> {
    let src = std::fs::read_to_string(root.join(rel)).ok()?;
    let s = (start.max(1) - 1) as usize;
    let e = end.max(start) as usize;
    let out: Vec<&str> = src.lines().skip(s).take(e.saturating_sub(s)).collect();
    Some(out.join("\n"))
}

fn budget(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) && cut > 0 {
        cut -= 1;
    }
    format!("{}\n… [truncated to {} bytes]", &s[..cut], max)
}
