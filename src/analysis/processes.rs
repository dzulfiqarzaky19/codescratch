//! Entrypoint→leaf call chains over resolved `calls`.

use super::{aggregate_conf, hub_label, symbol_file, CallEdge};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};

const PROCESS_MAX_CHAIN: usize = 8;
const PROCESS_MAX_COUNT: usize = 50;
const PROCESS_MIN_LEN: usize = 2;

pub(crate) struct RawProcess {
    entrypoint_qn: String,
    steps: Vec<String>,
    conf: &'static str,
}

pub(crate) fn compute(symbol_ids: &[String], calls: &[&CallEdge]) -> Vec<RawProcess> {
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

pub(crate) fn write(tx: &Connection, processes: &[RawProcess]) -> Result<()> {
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
