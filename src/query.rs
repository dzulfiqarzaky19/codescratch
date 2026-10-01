//! Read-side: fat `explore` payload (the product), plus `status` + `search`.
//!
//! Explore payload v2 (frozen section order):
//!   banner → node+snippet → call-path spine → members/heritage
//!   → depth-grouped blast → uses → routes/processes
//! Weak edges stay labeled. Absence ≠ proof.
//! Markdown is an adapter over [`ExploreView`].

use crate::blast;
use crate::model::NodeRow;
use crate::{db, trust};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::Path;

const SNIPPET_BUDGET: usize = 1600;
/// Direct callers (and, separately, uses) a brief answer lists before it counts the rest.
const BRIEF_CALLERS: usize = 40;
/// Same-name symbols a brief answer covers; past this the name is too common to stand in for a grep.
const MAX_SAME_NAME: usize = 6;
/// Files a brief answer may list as text-only mentions; past this the name is a word, not a symbol.
pub(crate) const MAX_TEXT_ONLY: usize = 12;

/// How much of an [`ExploreView`] the markdown adapter prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Detail {
    /// The fat payload: source, spine, members, full blast, callees, routes.
    #[default]
    Full,
    /// What a grep for the name would have told you: where it is defined, its
    /// signature, its direct callers and its uses, for every symbol of that name,
    /// then the files that only mention it as text. No source body. Prints the `## ` header
    /// only when the graph can stand in for that grep (see [`ExploreView::grep_gap`]).
    Brief,
}

fn node_by_id(conn: &Connection, id: &str) -> Option<NodeRow> {
    NodeRow::by_id(conn, id)
}

pub fn status(root: &Path) -> Result<String> {
    let conn = db::open(root)?;
    let t = trust::compute(&conn, root)?;
    Ok(trust::banner(&t))
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

/// Typed explore payload. Markdown is an adapter over this, not the interface.
#[derive(Debug, Clone)]
pub struct ExploreView {
    pub node: NodeRow,
    pub snippet: Option<String>,
    pub spine: Vec<String>,
    pub members: Vec<NodeRow>,
    pub heritage: Vec<String>,
    pub blast: Vec<(usize, Vec<String>)>,
    pub callees: Vec<String>,
    pub routes: Vec<String>,
    /// Inbound `references` edges: JSX elements, type annotations, value reads.
    pub uses: Vec<String>,
    /// Nodes carrying this name, the explored one included.
    pub same_name: usize,
    /// The other nodes of that name, up to [`MAX_SAME_NAME`] in all.
    pub others: Vec<ExploreView>,
    /// `file:line` of the first word match in each indexed file that no node,
    /// caller or use of this name lives in: strings, comments, fields, bare imports.
    pub text_only: Vec<String>,
}

impl ExploreView {
    /// Why this view cannot replace a text search for the name, or `None` when it
    /// can: few enough nodes carry the name, and few enough files mention it
    /// outside the graph that the answer can list them all.
    pub fn grep_gap(&self) -> Option<String> {
        if self.same_name > MAX_SAME_NAME {
            return Some(format!("{} symbols share this name", self.same_name));
        }
        if self.text_only.len() > MAX_TEXT_ONLY {
            return Some(format!(
                "{} files mention it outside the graph",
                self.text_only.len()
            ));
        }
        None
    }

    fn direct_callers(&self) -> &[String] {
        self.blast
            .iter()
            .find(|(depth, _)| *depth == 1)
            .map(|(_, rows)| rows.as_slice())
            .unwrap_or(&[])
    }

}

/// One repo's answer to an explore. The **variant** carries found-vs-missing;
/// callers must never re-derive that by searching the rendered text.
pub enum Explored {
    Found(ExploreView),
    Missing { suggestions: Vec<String> },
}

pub fn explore(root: &Path, symbol: &str, detail: Detail) -> Result<String> {
    let banner = trust::banner(&trust::of(root)?);
    match explore_one(root, symbol, detail)? {
        Explored::Found(view) => Ok(format!("{banner}\n\n{}", render_view(&view, detail))),
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
pub fn explore_one(root: &Path, symbol: &str, detail: Detail) -> Result<Explored> {
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

    let mut view = gather(&conn, root, n);
    // Only brief prints the same-name nodes and the text scan; full skips the disk read.
    if detail == Detail::Brief {
        if view.same_name > 1 && view.same_name <= MAX_SAME_NAME {
            view.others = same_name_nodes(&conn, &view.node)
                .into_iter()
                .map(|o| gather(&conn, root, o))
                .collect();
        }
        view.text_only = text_only_mentions(&conn, root, &view.node.name);
    }
    Ok(Explored::Found(view))
}

fn same_name_nodes(conn: &Connection, n: &NodeRow) -> Vec<NodeRow> {
    let sql = "SELECT id,kind,name,qualified_name,file_path,start_line,end_line,exported,signature
               FROM nodes WHERE name=?1 AND id<>?2 ORDER BY exported DESC, file_path, start_line";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return vec![];
    };
    stmt.query_map([&n.name, &n.id], NodeRow::from_row)
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// Spine, members, heritage, blast, callees, routes, snippet for one node.
fn gather(conn: &Connection, root: &Path, n: NodeRow) -> ExploreView {
    let snippet = read_lines(root, &n.file_path, n.start_line, n.end_line)
        .map(|code| budget(&code, SNIPPET_BUDGET));
    ExploreView {
        snippet,
        spine: call_path_spine(conn, &n.id),
        members: child_symbols(conn, &n.id),
        heritage: heritage_out(conn, &n.id),
        blast: blast_by_depth(conn, &n.id, blast::MAX_DEPTH),
        callees: edges_out(conn, &n.id, "calls"),
        routes: routes_touching(conn, &n.id),
        uses: uses_in(conn, &n.id),
        others: vec![],
        text_only: vec![],
        same_name: conn
            .query_row("SELECT COUNT(*) FROM nodes WHERE name=?1", [&n.name], |r| {
                r.get::<_, i64>(0)
            })
            .map(|c| c as usize)
            .unwrap_or(1),
        node: n,
    }
}

/// Markdown adapter over [`ExploreView`]. Frozen section order is here, not in
/// the gather path, so tests can assert on the view without grepping prose.
pub(crate) fn render_view(v: &ExploreView, detail: Detail) -> String {
    let n = &v.node;
    let mut out = String::new();
    if detail == Detail::Brief {
        if let Some(gap) = v.grep_gap() {
            return render_refusal(v, &gap);
        }
    }
    render_header(v, &mut out);
    if detail == Detail::Brief {
        render_brief_users(v, &mut out);
        for o in &v.others {
            out.push('\n');
            render_header(o, &mut out);
            render_brief_users(o, &mut out);
        }
        if !v.text_only.is_empty() {
            out.push_str("\n**text-only mentions** (string, comment, field or bare import)\n");
            for m in &v.text_only {
                out.push_str(&format!("- {m}\n"));
            }
        } else {
            out.push_str("\nno other file mentions this name.\n");
        }
        out.push_str(&format!(
            "\n(brief — `codescratch explore {}` for transitive callers, source and callees)\n",
            n.name
        ));
        return out;
    }
    if let Some(code) = &v.snippet {
        out.push_str("\n```\n");
        out.push_str(code);
        out.push_str("\n```\n");
    }

    out.push_str("\n**call-path spine**\n");
    if v.spine.is_empty() {
        out.push_str("- (leaf — no named callees)\n");
    } else {
        for s in &v.spine {
            out.push_str(&format!("- {s}\n"));
        }
    }

    if !v.members.is_empty() {
        out.push_str("\n**members**\n");
        for m in &v.members {
            out.push_str(&format!("- {} `{}`  :{}\n", m.kind, m.name, m.start_line));
        }
    }
    if !v.heritage.is_empty() {
        out.push_str("\n**heritage**\n");
        for h in &v.heritage {
            out.push_str(&format!("- {h}\n"));
        }
    }

    out.push_str("\n**callers ← (blast radius)**\n");
    if v.blast.is_empty() {
        out.push_str(
            "- (no resolved callers — absence ≠ proof; weak/dynamic calls may be missed)\n",
        );
    } else {
        for (depth, rows) in &v.blast {
            out.push_str(&format!("depth {depth}:\n"));
            for r in rows {
                out.push_str(&format!("- {r}\n"));
            }
        }
    }

    if !v.uses.is_empty() {
        out.push_str("\n**uses ←**\n");
        for u in &v.uses {
            out.push_str(&format!("- {u}\n"));
        }
    }

    out.push_str("\n**calls →**\n");
    if v.callees.is_empty() {
        out.push_str("- (none captured)\n");
    }
    for c in &v.callees {
        out.push_str(&format!("- {c}\n"));
    }

    out.push_str("\n**routes / processes**\n");
    if v.routes.is_empty() {
        out.push_str("- (none)\n");
    } else {
        for r in &v.routes {
            out.push_str(&format!("- {r}\n"));
        }
    }
    out
}

/// Brief's "grep instead": where the symbol is and why the graph cannot stand in.
/// No `## ` header: hosts read that header as "the graph answered the grep".
pub(crate) fn render_refusal(v: &ExploreView, gap: &str) -> String {
    let n = &v.node;
    format!(
        "{} `{}`  ({}:{}-{}){}\nnot a grep substitute: {gap}. grep `{}` for its uses.\n",
        n.kind,
        n.qualified_name,
        n.file_path,
        n.start_line,
        n.end_line,
        if n.exported { "  [exported]" } else { "" },
        n.name
    )
}

fn render_header(v: &ExploreView, out: &mut String) {
    let n = &v.node;
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
}

/// Brief body: direct callers and uses, each capped, deeper hops as a count.
fn render_brief_users(v: &ExploreView, out: &mut String) {
    let deeper: usize = v
        .blast
        .iter()
        .filter(|(depth, _)| *depth > 1)
        .map(|(_, rows)| rows.len())
        .sum();
    for (title, rows) in [("callers", v.direct_callers()), ("uses", v.uses.as_slice())] {
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!("\n**{title} ←**\n"));
        for r in rows.iter().take(BRIEF_CALLERS) {
            out.push_str(&format!("- {r}\n"));
        }
        if rows.len() > BRIEF_CALLERS {
            out.push_str(&format!("- … {} more\n", rows.len() - BRIEF_CALLERS));
        }
    }
    if deeper > 0 {
        out.push_str(&format!("({deeper} transitive callers)\n"));
    }
}

fn child_symbols(conn: &Connection, id: &str) -> Vec<NodeRow> {
    let sql = "SELECT n.id,n.kind,n.name,n.qualified_name,n.file_path,n.start_line,n.end_line,n.exported,n.signature
               FROM edges e JOIN nodes n ON n.id = e.dst_id
               WHERE e.src_id=?1 AND e.kind='contains' ORDER BY n.start_line";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return vec![];
    };
    stmt.query_map([id], NodeRow::from_row)
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

fn edges_out(conn: &Connection, id: &str, kind: &str) -> Vec<String> {
    let sql = "SELECT raw_name, dst_id, resolved, conf, reason FROM edges
               WHERE src_id=?1 AND kind=?2 ORDER BY line";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return vec![];
    };
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
    let Ok(mut stmt) = conn.prepare(sql) else {
        return vec![];
    };
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
        let Some((dst, raw, resolved)) = row else {
            break;
        };
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
    let Ok(hops) = blast::from_ids(conn, &[id], max) else {
        return vec![];
    };
    let mut buckets: Vec<(usize, Vec<String>)> = Vec::new();
    for h in hops {
        let who = h
            .node
            .as_ref()
            .map(|nd| nd.qualified_name.clone())
            .unwrap_or_else(|| "<module>".to_string());
        let mark = if h.conf == "weak" { "  ⟨weak⟩" } else { "" };
        let line_s = format!("{who}  {}:{}  [{}]{mark}", h.file_path, h.line, h.reason);
        if let Some((_, rows)) = buckets.iter_mut().find(|(dd, _)| *dd == h.depth) {
            rows.push(line_s);
        } else {
            buckets.push((h.depth, vec![line_s]));
        }
    }
    buckets.sort_by_key(|(d, _)| *d);
    buckets
}

fn uses_in(conn: &Connection, id: &str) -> Vec<String> {
    let sql = "SELECT n.qualified_name, e.file_path, e.line, e.reason
               FROM edges e LEFT JOIN nodes n ON n.id = e.src_id
               WHERE e.dst_id=?1 AND e.kind='references'
               ORDER BY e.file_path, e.line";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return vec![];
    };
    stmt.query_map([id], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
        ))
    })
    .map(|it| {
        it.filter_map(|r| r.ok())
            .map(|(who, file, line, reason)| {
                let who = who.unwrap_or_else(|| "<module>".to_string());
                format!("{who}  {file}:{line}  [{reason}]")
            })
            .collect()
    })
    .unwrap_or_default()
}

/// [`text_only_mentions`] for a repo taken on its own: every word match, when the
/// repo declares no symbol of that name.
pub(crate) fn text_mentions(root: &Path, name: &str) -> Vec<String> {
    db::open(root)
        .map(|conn| text_only_mentions(&conn, root, name))
        .unwrap_or_default()
}

/// Indexed files where `name` appears as a whole word but no node of that name is
/// declared, called or used. Read from disk, so it is as fresh as a grep.
fn text_only_mentions(conn: &Connection, root: &Path, name: &str) -> Vec<String> {
    let column = |sql: &str, arg: Option<&str>| -> Vec<String> {
        let Ok(mut stmt) = conn.prepare(sql) else {
            return vec![];
        };
        stmt.query_map(rusqlite::params_from_iter(arg), |r| r.get::<_, String>(0))
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    };
    let linked: HashSet<String> = column(
        "SELECT file_path FROM nodes WHERE name=?1
         UNION
         SELECT e.file_path FROM edges e JOIN nodes n ON n.id = e.dst_id
         WHERE n.name=?1 AND e.kind IN ('calls','references') AND e.resolved=1",
        Some(name),
    )
    .into_iter()
    .collect();
    let mut out = Vec::new();
    for path in column("SELECT path FROM files ORDER BY path", None) {
        if linked.contains(&path) {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(root.join(&path)) else {
            continue;
        };
        let hits: Vec<usize> = src
            .lines()
            .enumerate()
            .filter(|(_, l)| has_word(l, name))
            .map(|(i, _)| i + 1)
            .collect();
        match hits.as_slice() {
            [] => {}
            [one] => out.push(format!("{path}:{one}")),
            [first, rest @ ..] => out.push(format!("{path}:{first}  (+{} lines)", rest.len())),
        }
    }
    out
}

/// `word` occurs in `line` with no identifier character on either side.
fn has_word(line: &str, word: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    line.match_indices(word).any(|(i, _)| {
        !line[..i].chars().next_back().is_some_and(ident)
            && !line[i + word.len()..].chars().next().is_some_and(ident)
    })
}

fn routes_touching(conn: &Connection, id: &str) -> Vec<String> {
    let sql = "SELECT n.qualified_name, n.file_path, n.start_line, e.kind
               FROM edges e JOIN nodes n ON n.id = e.dst_id
               WHERE e.src_id=?1 AND e.kind IN ('handles_route','step_in','member_of')
               UNION
               SELECT n.qualified_name, n.file_path, n.start_line, e.kind
               FROM edges e JOIN nodes n ON n.id = e.src_id
               WHERE e.dst_id=?1 AND e.kind IN ('handles_route','step_in','member_of')";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return vec![];
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn view_fixture() -> ExploreView {
        ExploreView {
            node: NodeRow {
                id: "f#foo@1".into(),
                kind: "function".into(),
                name: "foo".into(),
                qualified_name: "foo".into(),
                file_path: "src/a.ts".into(),
                start_line: 1,
                end_line: 3,
                exported: true,
                signature: "function foo()".into(),
            },
            snippet: Some("export function foo() { return 1; }".into()),
            spine: vec![],
            members: vec![],
            heritage: vec![],
            blast: vec![(1, vec!["bar  src/b.ts:2  [same-file]".into()])],
            callees: vec![],
            routes: vec!["step_in  flow:bar  src/b.ts:0".into()],
            uses: vec![],
            others: vec![],
            text_only: vec![],
            same_name: 1,
        }
    }

    #[test]
    fn render_view_keeps_frozen_section_order() {
        let s = render_view(&view_fixture(), Detail::Full);
        let spine = s.find("**call-path spine**").unwrap();
        let blast = s.find("**callers ← (blast radius)**").unwrap();
        let calls = s.find("**calls →**").unwrap();
        let routes = s.find("**routes / processes**").unwrap();
        assert!(spine < blast && blast < calls && calls < routes, "{s}");
        assert!(s.contains("[exported]"));
        assert!(s.contains("depth 1:"));
        assert!(s.contains("step_in"));
    }

    #[test]
    fn brief_keeps_location_and_direct_callers_only() {
        let mut v = view_fixture();
        v.blast.push((2, vec!["baz  src/c.ts:9  [import]".into()]));
        let s = render_view(&v, Detail::Brief);
        assert!(s.starts_with("## function `foo`  (src/a.ts:1-3)  [exported]"), "{s}");
        assert!(s.contains("`function foo()`"));
        assert!(s.contains("- bar  src/b.ts:2  [same-file]"));
        assert!(s.contains("(1 transitive callers)"), "{s}");
        for gone in ["return 1", "baz", "**call-path spine**", "**calls →**", "**routes / processes**"] {
            assert!(!s.contains(gone), "brief leaked `{gone}`:\n{s}");
        }
    }

    #[test]
    fn brief_answers_any_kind_the_graph_holds_a_use_of() {
        let mut ty = view_fixture();
        ty.node.kind = "type".into();
        ty.blast.clear();
        ty.uses = vec!["Row  src/c.ts:4  [import-binding]".into()];
        let s = render_view(&ty, Detail::Brief);
        assert!(s.starts_with("## type `foo`"), "{s}");
        assert!(s.contains("**uses ←**\n- Row  src/c.ts:4  [import-binding]"), "{s}");
        assert!(!s.contains("**callers ←**"), "{s}");
    }

    #[test]
    fn brief_accounts_for_every_file_a_grep_would_list() {
        let mut v = view_fixture();
        assert!(render_view(&v, Detail::Brief).contains("no other file mentions this name."));
        v.text_only = vec!["src/doc.ts:7  (+2 lines)".into()];
        let s = render_view(&v, Detail::Brief);
        assert!(s.contains("**text-only mentions**") && s.contains("- src/doc.ts:7  (+2 lines)"), "{s}");
        assert!(!render_view(&v, Detail::Full).contains("text-only"), "full payload is unchanged");
    }

    #[test]
    fn has_word_respects_identifier_boundaries() {
        assert!(has_word("const a = foo(1)", "foo") && has_word("'foo'", "foo") && has_word("x.foo", "foo"));
        assert!(!has_word("foobar()", "foo") && !has_word("my_foo", "foo") && !has_word("$foo", "foo"));
        assert!(has_word("foobar foo", "foo"), "a later whole-word match still counts");
    }

    #[test]
    fn brief_lists_every_symbol_of_a_shared_name() {
        let mut v = view_fixture();
        let mut twin = view_fixture();
        twin.node.file_path = "src/z.ts".into();
        twin.blast.clear();
        v.same_name = 2;
        v.others = vec![twin];
        let s = render_view(&v, Detail::Brief);
        assert!(s.contains("(src/a.ts:1-3)") && s.contains("## function `foo`  (src/z.ts:1-3)"), "{s}");
        assert_eq!(s.matches("(brief — ").count(), 1, "{s}");
    }

    #[test]
    fn brief_drops_the_header_when_the_graph_cannot_replace_a_grep() {
        let mut common = view_fixture();
        common.same_name = MAX_SAME_NAME + 1;
        let mut wordy = view_fixture();
        wordy.text_only = (0..=MAX_TEXT_ONLY).map(|i| format!("src/f{i}.ts:1")).collect();
        for (v, why) in [(common, "7 symbols share"), (wordy, "13 files mention it outside the graph")] {
            let s = render_view(&v, Detail::Brief);
            assert!(!s.contains("## "), "{s}");
            assert!(s.contains("src/a.ts:1-3") && s.contains(why), "{s}");
            assert!(render_view(&v, Detail::Full).starts_with("## "), "full keeps the header");
        }
    }

    #[test]
    fn budget_truncates_on_char_boundary() {
        let s = budget("abcdefghij", 4);
        assert!(s.starts_with("abcd"));
        assert!(s.contains("truncated"));
    }
}
