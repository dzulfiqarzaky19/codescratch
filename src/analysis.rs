//! Community + process materialization (v0.4). Turns the `calls` graph into
//! two agent-facing conveniences:
//!   - `community` nodes: label-propagation clusters over `calls` (undirected)
//!     — a cheap, deterministic stand-in for Leiden (no external crate).
//!   - `process` nodes: entrypoint→leaf call chains over resolved `calls`
//!     — ready-made "flows" for the agent to skim instead of walking edges.
//!
//! Explore reads these via `member_of` / `step_in` in the routes/processes
//! section — they are not unpaid index work.
//!
//! Both are heuristic aggregations of AST facts, never AST facts themselves:
//! `provenance` is always `heuristic` here, and `conf` reflects whether any
//! underlying edge was `weak` (see `aggregate_conf`). `materialize` is
//! idempotent — it deletes its own prior output before recomputing, so it is
//! safe to call after every `ensure`, incremental or full.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};

const MIN_COMMUNITY_SIZE: usize = 3;
const LABEL_PROP_MAX_ROUNDS: usize = 10;
const PROCESS_MAX_CHAIN: usize = 8;
const PROCESS_MAX_COUNT: usize = 50;
const PROCESS_MIN_LEN: usize = 2;

/// One `calls` edge, reduced to what community/process computation needs.
struct CallEdge {
    src: String,
    dst: String,
    conf: String, // strong | weak — drives aggregate_conf on the derived node
}

/// Materialize community + process nodes/edges from the current `calls` graph.
/// Idempotent: deletes prior `community`/`process` nodes + their `member_of`/
/// `step_in` edges + their node_attrs first, then recomputes. Call AFTER the
/// index has written symbols and resolved `calls` edges. Runs in its own
/// transaction.
pub fn materialize(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;

    // --- idempotent cleanup: drop any prior community/process output ---
    tx.execute_batch(
        "DELETE FROM node_attrs WHERE node_id IN (SELECT id FROM nodes WHERE kind IN ('community','process'));
         DELETE FROM edges WHERE kind IN ('member_of','step_in');
         DELETE FROM nodes WHERE kind IN ('community','process');",
    )?;

    // --- gather symbol universe (deterministic: sorted by id) ---
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

    // --- gather calls edges (prefer resolved; all-conf set used for communities) ---
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

    // Only edges between two known symbol nodes are meaningful for both
    // communities and processes — a route/community/process dst would be
    // noise (and can't appear pre-cleanup anyway since we just deleted them).
    let symbol_set: BTreeSet<&str> = symbol_ids.iter().map(|s| s.as_str()).collect();
    let calls: Vec<&CallEdge> = all_calls
        .iter()
        .filter(|e| symbol_set.contains(e.src.as_str()) && symbol_set.contains(e.dst.as_str()))
        .collect();

    let communities = label_propagation_communities(&symbol_ids, &calls);
    let processes = entrypoint_processes(&symbol_ids, &calls);

    write_communities(&tx, &communities)?;
    write_processes(&tx, &processes)?;

    tx.commit()?;
    Ok(())
}

/// `strong` only if every contributing edge was `strong`; `weak` if any was.
/// Honesty rule: never upgrade a weak fact by aggregating it.
fn aggregate_conf<'a, I: IntoIterator<Item = &'a str>>(confs: I) -> &'static str {
    for c in confs {
        if c != "strong" {
            return "weak";
        }
    }
    "strong"
}

// ============================== communities ==============================

struct RawCommunity {
    members: Vec<String>, // sorted node ids
    label: String,        // qualified_name of the "hub" member
    conf: &'static str,
}

/// Deterministic label propagation over the undirected `calls` graph.
/// Each node starts labeled with its own id. Each round, every node (visited
/// in sorted-id order) adopts the most common label among its neighbors,
/// tie-broken by the smallest label string. Repeats to a fixed point or
/// `LABEL_PROP_MAX_ROUNDS`, whichever comes first. Isolated nodes (no calls
/// edges at all) keep their own label and are dropped later by the
/// min-size filter.
fn label_propagation_communities(symbol_ids: &[String], calls: &[&CallEdge]) -> Vec<RawCommunity> {
    if symbol_ids.is_empty() {
        return Vec::new();
    }

    // Undirected adjacency, deduped, sorted — built once.
    let mut adj: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for id in symbol_ids {
        adj.entry(id.as_str()).or_default();
    }
    for e in calls {
        if e.src == e.dst {
            continue; // self-calls don't inform clustering
        }
        adj.entry(e.src.as_str())
            .or_default()
            .insert(e.dst.as_str());
        adj.entry(e.dst.as_str())
            .or_default()
            .insert(e.src.as_str());
    }

    let mut label: BTreeMap<&str, String> =
        symbol_ids.iter().map(|s| (s.as_str(), s.clone())).collect();

    for _round in 0..LABEL_PROP_MAX_ROUNDS {
        let mut changed = false;
        // Deterministic visit order: sorted node ids (symbol_ids is already sorted).
        for id in symbol_ids {
            let neighbors = &adj[id.as_str()];
            if neighbors.is_empty() {
                continue;
            }
            // Count neighbor labels, tie-break by smallest label string.
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            for n in neighbors {
                let l = label[n].as_str();
                *counts.entry(l).or_insert(0) += 1;
            }
            let max_count = *counts.values().max().unwrap();
            let best = counts
                .iter()
                .filter(|(_, &c)| c == max_count)
                .map(|(l, _)| *l)
                .min()
                .unwrap()
                .to_string();
            if label[id.as_str()] != best {
                label.insert(id.as_str(), best);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Group by final label.
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for id in symbol_ids {
        groups
            .entry(label[id.as_str()].clone())
            .or_default()
            .push(id.clone());
    }

    // Edge conf lookup per unordered pair, for aggregate_conf.
    let mut pair_conf: BTreeMap<(&str, &str), &str> = BTreeMap::new();
    for e in calls {
        let key = if e.src.as_str() <= e.dst.as_str() {
            (e.src.as_str(), e.dst.as_str())
        } else {
            (e.dst.as_str(), e.src.as_str())
        };
        let entry = pair_conf.entry(key).or_insert(e.conf.as_str());
        if e.conf != "strong" {
            *entry = "weak";
        }
    }

    // Call-count per node (within the whole graph) to pick the label/hub
    // deterministically: most calls (in + out), tie-break by smallest id.
    let mut call_count: BTreeMap<&str, usize> = BTreeMap::new();
    for e in calls {
        *call_count.entry(e.src.as_str()).or_insert(0) += 1;
        *call_count.entry(e.dst.as_str()).or_insert(0) += 1;
    }

    let mut out = Vec::new();
    for (_label_id, mut members) in groups {
        if members.len() < MIN_COMMUNITY_SIZE {
            continue; // drop singletons/pairs — noise
        }
        members.sort();

        // Hub = member with most calls; tie-break smallest id. Explicit fold
        // (not Iterator::max_by) so the tie-break is unambiguous by inspection.
        let mut hub = members[0].clone();
        let mut hub_count = call_count.get(hub.as_str()).copied().unwrap_or(0);
        for m in &members[1..] {
            let c = call_count.get(m.as_str()).copied().unwrap_or(0);
            if c > hub_count {
                hub = m.clone();
                hub_count = c;
            }
        }

        // Community conf = aggregate over every edge with both endpoints inside.
        let member_set: BTreeSet<&str> = members.iter().map(|s| s.as_str()).collect();
        let confs: Vec<&str> = pair_conf
            .iter()
            .filter(|((a, b), _)| member_set.contains(a) && member_set.contains(b))
            .map(|(_, c)| *c)
            .collect();

        // Density guard: keep only clusters, not paths. `pair_conf` keys are the
        // unordered edges, so `confs.len()` is the count of distinct internal
        // edges. A tree/path over N members has N-1 edges; a genuine cluster has
        // a cycle → at least N. A pure call chain (which is already surfaced as a
        // `process`) therefore falls through here instead of doubling as a
        // community — communities mean tight neighborhoods, not flows.
        if confs.len() < members.len() {
            continue;
        }
        let conf = if confs.is_empty() {
            "strong"
        } else {
            aggregate_conf(confs)
        };

        out.push(RawCommunity {
            members,
            label: hub, // qualified_name resolved at write time
            conf,
        });
    }

    // Deterministic emission order: by first (smallest) member id.
    out.sort_by(|a, b| a.members[0].cmp(&b.members[0]));
    out
}

fn write_communities(tx: &Connection, communities: &[RawCommunity]) -> Result<()> {
    let mut nstmt = tx.prepare(
        "INSERT INTO nodes(id,kind,name,qualified_name,file_path,start_line,end_line,exported,signature)
         VALUES(?1,'community',?2,?2,?3,0,0,0,'')",
    )?;
    let mut estmt = tx.prepare(
        "INSERT INTO edges(src_id,dst_id,kind,raw_name,resolved,conf,reason,provenance,file_path,line)
         VALUES(?1,?2,'member_of','',1,?3,'community','heuristic',?4,0)",
    )?;
    let mut astmt =
        tx.prepare("INSERT INTO node_attrs(node_id,key,value) VALUES(?1,'community_size',?2)")?;

    for (i, c) in communities.iter().enumerate() {
        let id = format!("#community:{i}");
        let (label, label_file) = hub_label(tx, &c.label);
        nstmt.execute((&id, &label, &label_file))?;
        astmt.execute((&id, c.members.len().to_string()))?;
        for m in &c.members {
            let file = symbol_file(tx, m).unwrap_or_default();
            estmt.execute((m, &id, c.conf, &file))?;
        }
    }
    Ok(())
}

/// Resolve a hub member id to (qualified_name, file_path) for the community label.
fn hub_label(conn: &Connection, hub_id: &str) -> (String, String) {
    conn.query_row(
        "SELECT qualified_name, file_path FROM nodes WHERE id=?1",
        [hub_id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )
    .unwrap_or_else(|_| (hub_id.to_string(), String::new()))
}

fn symbol_file(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row("SELECT file_path FROM nodes WHERE id=?1", [id], |r| {
        r.get::<_, String>(0)
    })
    .ok()
}

// =============================== processes =================================

struct RawProcess {
    entrypoint_qn: String,
    steps: Vec<String>, // ordered symbol ids, len >= PROCESS_MIN_LEN
    conf: &'static str,
}

/// Precomputed entrypoint→leaf call chains over resolved `calls` edges.
/// Entrypoints are symbol nodes with in-degree 0 on `calls` (no resolved
/// caller), chosen deterministically by sorted id, capped at
/// `PROCESS_MAX_COUNT`. From each entrypoint, walks the lexicographically
/// smallest outgoing `calls` edge at each step (deterministic single path,
/// not an exhaustive DFS/BFS enumeration), stopping at a leaf, a repeated
/// node (cycle guard), or `PROCESS_MAX_CHAIN` steps. Chains shorter than
/// `PROCESS_MIN_LEN` are skipped as trivial.
fn entrypoint_processes(symbol_ids: &[String], calls: &[&CallEdge]) -> Vec<RawProcess> {
    if symbol_ids.is_empty() || calls.is_empty() {
        return Vec::new();
    }

    // Directed adjacency (resolved calls only), sorted targets per src.
    let mut out_edges: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new(); // src -> [(dst, conf)]
    let mut has_incoming: BTreeSet<&str> = BTreeSet::new();
    for e in calls {
        out_edges
            .entry(e.src.as_str())
            .or_default()
            .push((e.dst.as_str(), e.conf.as_str()));
        has_incoming.insert(e.dst.as_str());
    }
    for v in out_edges.values_mut() {
        v.sort();
    }

    // Entrypoints: symbols with outgoing calls but no resolved incoming call,
    // in sorted id order (deterministic), capped.
    let entrypoints: Vec<&str> = symbol_ids
        .iter()
        .map(|s| s.as_str())
        .filter(|id| out_edges.contains_key(id) && !has_incoming.contains(id))
        .take(PROCESS_MAX_COUNT)
        .collect();

    let mut out = Vec::new();
    for ep in entrypoints {
        let mut steps: Vec<String> = vec![ep.to_string()];
        let mut visited: BTreeSet<&str> = BTreeSet::new();
        visited.insert(ep);
        let mut confs: Vec<&str> = Vec::new();
        let mut cur = ep;

        while steps.len() < PROCESS_MAX_CHAIN {
            let Some(targets) = out_edges.get(cur) else {
                break;
            };
            // Smallest-id outgoing edge not yet visited (cycle guard), deterministic.
            let Some(&(next, conf)) = targets.iter().find(|(t, _)| !visited.contains(t)) else {
                break;
            };
            steps.push(next.to_string());
            confs.push(conf);
            visited.insert(next);
            cur = next;
        }

        if steps.len() < PROCESS_MIN_LEN {
            continue; // trivial chain — skip
        }

        let conf = if confs.is_empty() {
            "strong"
        } else {
            aggregate_conf(confs)
        };
        out.push(RawProcess {
            entrypoint_qn: ep.to_string(), // resolved to qualified_name at write time
            steps,
            conf,
        });
        if out.len() >= PROCESS_MAX_COUNT {
            break;
        }
    }

    out
}

fn write_processes(tx: &Connection, processes: &[RawProcess]) -> Result<()> {
    let mut nstmt = tx.prepare(
        "INSERT INTO nodes(id,kind,name,qualified_name,file_path,start_line,end_line,exported,signature)
         VALUES(?1,'process',?2,?2,?3,0,0,0,'')",
    )?;
    let mut estmt = tx.prepare(
        "INSERT INTO edges(src_id,dst_id,kind,raw_name,resolved,conf,reason,provenance,file_path,line)
         VALUES(?1,?2,'step_in','',1,?3,'process','heuristic',?4,?5)",
    )?;

    for (i, p) in processes.iter().enumerate() {
        let id = format!("#process:{i}");
        let (ep_qn, ep_file) = hub_label(tx, &p.entrypoint_qn);
        let name = format!("flow:{ep_qn}");
        nstmt.execute((&id, &name, &ep_file))?;
        for (idx, step) in p.steps.iter().enumerate() {
            let file = symbol_file(tx, step).unwrap_or_default();
            estmt.execute((step, &id, p.conf, &file, idx as i64))?;
        }
    }
    Ok(())
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
