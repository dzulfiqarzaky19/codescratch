import {
  IMPACT_MAX_DEPTH,
  IMPACT_MAX_NODES,
  resolveRoot,
} from "../config.js";
import { GraphDb } from "../db/client.js";
import type { GraphNode, ImpactDirection } from "../models.js";
import { ambiguityNote, candidatePayload, resolveTarget } from "./explore.js";
import { formatNodeShort, wrapResult } from "./format.js";
import { computeTrust, requireGraph } from "./trust.js";

export function impactAnalysis(
  target: string,
  rootInput?: string,
  direction: ImpactDirection = "up",
): string {
  const root = resolveRoot(rootInput);
  requireGraph(root);
  const db = GraphDb.open(root);
  try {
    const trust = computeTrust(db);
    const match = resolveTarget(db, target);
    const node = match.node;
    if (!node) {
      return wrapResult(
        trust,
        `No symbol/path matched ${JSON.stringify(target)}.`,
        { results: [] },
      );
    }
    const ambiguous = ambiguityNote(match);

    const seeds: GraphNode[] =
      node.kind === "file" ? db.nodesInFile(node.file_path) : [node];

    const sections: string[] = [
      ...(ambiguous ? [ambiguous] : []),
      `impact of ${formatNodeShort(node)}  direction=${direction}`,
    ];
    const json: Record<string, unknown> = {
      target: node.qualified_name,
      direction,
      candidates: candidatePayload(match),
    };

    if (direction === "up" || direction === "both") {
      const up = bfs(db, seeds, "up");
      sections.push(formatSection("upstream (dependents)", up));
      json.upstream = summarize(up);
    }
    if (direction === "down" || direction === "both") {
      const down = bfs(db, seeds, "down");
      sections.push(formatSection("downstream (dependencies)", down));
      json.downstream = summarize(down);
    }

    return wrapResult(trust, sections.join("\n\n"), json);
  } finally {
    db.close();
  }
}

interface Hit {
  node: GraphNode;
  depth: number;
  via: string;
}

function bfs(
  db: GraphDb,
  seeds: GraphNode[],
  dir: "up" | "down",
): { list: Hit[]; truncated: boolean } {
  const affected = new Map<string, Hit>();
  const seen = new Set(seeds.map((s) => s.id));
  let frontier = seeds.map((s) => ({ id: s.id, depth: 0 }));
  let truncated = false;

  while (frontier.length) {
    const next: { id: string; depth: number }[] = [];
    for (const f of frontier) {
      if (f.depth >= IMPACT_MAX_DEPTH) continue;
      const edges =
        dir === "up"
          ? [
              ...db.edgesTo(f.id, "calls"),
              ...db.edgesTo(f.id, "imports"),
              ...db.edgesTo(f.id, "extends"),
              ...db.edgesTo(f.id, "implements"),
            ]
          : [
              ...db.edgesFrom(f.id, "calls"),
              ...db.edgesFrom(f.id, "imports"),
              ...db.edgesFrom(f.id, "extends"),
              ...db.edgesFrom(f.id, "implements"),
            ];
      for (const e of edges) {
        const otherId = dir === "up" ? e.src_id : e.dst_id;
        if (!otherId || seen.has(otherId)) continue;
        if (affected.size >= IMPACT_MAX_NODES) {
          truncated = true;
          break;
        }
        seen.add(otherId);
        const n = db.getNode(otherId);
        if (!n) continue;
        const conf = e.confidence ? ` conf=${e.confidence}` : "";
        const why = e.reason ? ` reason=${e.reason}` : "";
        affected.set(n.id, {
          node: n,
          depth: f.depth + 1,
          via: `${e.kind}@${e.file_path}:${e.line}${conf}${why}`,
        });
        next.push({ id: otherId, depth: f.depth + 1 });
      }
      if (truncated) break;
    }
    if (truncated) break;
    frontier = next;
  }

  return {
    list: [...affected.values()].sort((a, b) => a.depth - b.depth),
    truncated,
  };
}

function formatSection(
  title: string,
  data: { list: Hit[]; truncated: boolean },
): string {
  const byFile = new Map<string, number>();
  for (const a of data.list) {
    byFile.set(a.node.file_path, (byFile.get(a.node.file_path) ?? 0) + 1);
  }
  return [
    `${title}: ${data.list.length} nodes${data.truncated ? " (truncated)" : ""}  depth≤${IMPACT_MAX_DEPTH}`,
    `files: ${byFile.size}`,
    "by depth:",
    ...data.list
      .slice(0, 80)
      .map((a) => `  d${a.depth} ${formatNodeShort(a.node)}  via ${a.via}`),
    "files:",
    ...[...byFile.entries()]
      .sort((a, b) => b[1] - a[1])
      .slice(0, 40)
      .map(([f, c]) => `  ${f} (${c})`),
  ].join("\n");
}

function summarize(data: { list: Hit[]; truncated: boolean }) {
  const byFile = new Map<string, number>();
  for (const a of data.list) {
    byFile.set(a.node.file_path, (byFile.get(a.node.file_path) ?? 0) + 1);
  }
  return {
    count: data.list.length,
    files: byFile.size,
    truncated: data.truncated,
  };
}
