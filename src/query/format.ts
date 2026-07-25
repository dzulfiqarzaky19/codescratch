import type { GraphNode, TrustInfo } from "../models.js";
import { statusNotes } from "./trust.js";

/** One-liner for query tools. Full essay only when not fresh or verbose. */
export function formatTrustLine(t: TrustInfo): string {
  const bits = [
    `trust: ${t.trust}`,
    `unresolved: ${t.unresolved_edge_count}`,
    `weak: ${t.weak_edge_count}`,
  ];
  if (t.trust !== "fresh") {
    bits.push(`reindex: ${t.reindex_cmd}`);
  }
  return bits.join("  |  ");
}

export function formatTrustFull(t: TrustInfo): string {
  const notes = statusNotes(t);
  const lines = [
    `trust: ${t.trust}`,
    `indexed_at: ${t.indexed_at ?? "n/a"}`,
    `last_full_index_at: ${t.last_full_index_at ?? "n/a"}`,
    `files: ${t.file_count}  nodes: ${t.node_count}  edges: ${t.edge_count}  unresolved: ${t.unresolved_edge_count}  weak: ${t.weak_edge_count}`,
    `reindex: ${t.reindex_cmd}`,
  ];
  if (notes.length) {
    lines.push("notes:");
    for (const n of notes) lines.push(`  - ${n}`);
  }
  return lines.join("\n");
}

export function formatNodeShort(n: GraphNode): string {
  const exp = n.exported ? " export" : "";
  return `${n.kind}${exp} ${n.qualified_name}  ${n.file_path}:${n.start_line}-${n.end_line}`;
}

export function formatNodeDetail(n: GraphNode, sourceSnippet?: string): string {
  const lines = [
    formatNodeShort(n),
    n.signature ? `sig: ${n.signature}` : null,
    sourceSnippet ? `---\n${sourceSnippet}\n---` : null,
  ].filter(Boolean);
  return lines.join("\n");
}

export function wrapResult(
  trust: TrustInfo,
  body: string,
  extra?: Record<string, unknown>,
  opts?: { verbose?: boolean },
): string {
  const verbose = opts?.verbose === true;
  const header = verbose ? formatTrustFull(trust) : formatTrustLine(trust);
  const payload = {
    trust: trust.trust,
    indexed_at: trust.indexed_at,
    unresolved_edge_count: trust.unresolved_edge_count,
    weak_edge_count: trust.weak_edge_count,
    reindex_cmd: trust.trust === "fresh" ? undefined : trust.reindex_cmd,
    ...extra,
  };
  // drop undefined keys
  const clean = Object.fromEntries(
    Object.entries(payload).filter(([, v]) => v !== undefined),
  );
  return [header, "", body, "", "```json", JSON.stringify(clean, null, 2), "```"].join(
    "\n",
  );
}
