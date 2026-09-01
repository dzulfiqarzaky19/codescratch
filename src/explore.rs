//! Explore gather. Markdown is an adapter over [`ExploreView`].

use crate::blast;
use crate::model::NodeRow;
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::Path;

const SNIPPET_BUDGET: usize = 1600;

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
}

/// Spine, members, heritage, blast, callees, routes, snippet for one node.
pub fn gather(conn: &Connection, root: &Path, n: NodeRow) -> ExploreView {
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
        node: n,
    }
}

fn node_by_id(conn: &Connection, id: &str) -> Option<NodeRow> {
    NodeRow::by_id(conn, id)
}

/// Markdown adapter over [`ExploreView`]. Frozen section order is here, not in
/// the gather path, so tests can assert on the view without grepping prose.
pub(crate) fn render_view(v: &ExploreView) -> String {
    let n = &v.node;
    let mut out = String::new();
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
        }
    }

    #[test]
    fn render_view_keeps_frozen_section_order() {
        let s = render_view(&view_fixture());
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
    fn budget_truncates_on_char_boundary() {
        let s = budget("abcdefghij", 4);
        assert!(s.starts_with("abcd"));
        assert!(s.contains("truncated"));
    }
}
