import { resolveRoot } from "../config.js";
import { GraphDb } from "../db/client.js";
import type { GraphNode } from "../models.js";
import { resolveTarget } from "./explore.js";
import { formatNodeShort, wrapResult } from "./format.js";
import { computeTrust, requireGraph } from "./trust.js";

export function listCallers(
  target: string,
  rootInput?: string,
  depth = 1,
): string {
  return walkCalls(target, "callers", rootInput, depth);
}

export function listCallees(
  target: string,
  rootInput?: string,
  depth = 1,
): string {
  return walkCalls(target, "callees", rootInput, depth);
}

function walkCalls(
  target: string,
  mode: "callers" | "callees",
  rootInput: string | undefined,
  depth: number,
): string {
  const root = resolveRoot(rootInput);
  requireGraph(root);
  const db = GraphDb.open(root);
  try {
    const trust = computeTrust(db);
    const node = resolveTarget(db, target);
    if (!node) {
      return wrapResult(trust, `No symbol matched ${JSON.stringify(target)}.`, {
        results: [],
      });
    }

    const maxDepth = Math.max(1, Math.min(depth, 6));
    const results: { node: GraphNode; depth: number; line: number; file: string }[] =
      [];
    const seen = new Set<string>([node.id]);
    let frontier: { id: string; depth: number }[] = [{ id: node.id, depth: 0 }];

    while (frontier.length) {
      const next: { id: string; depth: number }[] = [];
      for (const f of frontier) {
        if (f.depth >= maxDepth) continue;
        const edges =
          mode === "callers"
            ? db.edgesTo(f.id, "calls")
            : db.edgesFrom(f.id, "calls");
        for (const e of edges) {
          const otherId = mode === "callers" ? e.src_id : e.dst_id;
          if (!otherId || seen.has(otherId)) continue;
          seen.add(otherId);
          const n = db.getNode(otherId);
          if (!n) continue;
          results.push({
            node: n,
            depth: f.depth + 1,
            line: e.line,
            file: e.file_path,
          });
          next.push({ id: otherId, depth: f.depth + 1 });
        }
      }
      frontier = next;
    }

    const body = [
      `${mode} of ${formatNodeShort(node)}  depth≤${maxDepth}  (${results.length})`,
      ...results.map(
        (r) =>
          `  d${r.depth} ${formatNodeShort(r.node)}  via ${r.file}:${r.line}`,
      ),
      results.length === 0
        ? "  (none resolved — check unresolved edges / dynamic calls)"
        : null,
    ]
      .filter(Boolean)
      .join("\n");

    return wrapResult(trust, body, {
      target: node.qualified_name,
      mode,
      count: results.length,
      results: results.map((r) => ({
        id: r.node.id,
        qualified_name: r.node.qualified_name,
        file_path: r.node.file_path,
        depth: r.depth,
      })),
    });
  } finally {
    db.close();
  }
}
