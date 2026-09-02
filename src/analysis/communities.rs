//! Label-propagation clusters over undirected `calls`.

use super::{aggregate_conf, hub_label, symbol_file, CallEdge};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};

const MIN_COMMUNITY_SIZE: usize = 3;
const LABEL_PROP_MAX_ROUNDS: usize = 10;

pub(crate) struct RawCommunity {
    members: Vec<String>,
    label: String,
    conf: &'static str,
}

pub(crate) fn compute(symbol_ids: &[String], calls: &[&CallEdge]) -> Vec<RawCommunity> {
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

pub(crate) fn write(tx: &Connection, communities: &[RawCommunity]) -> Result<()> {
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
