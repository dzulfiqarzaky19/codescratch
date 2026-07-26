import type { GraphNode, TrustInfo } from "../models.js";
import { statusNotes } from "./trust.js";

/**
 * One-liner for query tools. Three independent axes stay three fields so a
 * noisy graph cannot masquerade as staleness, or vice versa.
 *
 * A reader who stops at the first token must not read `fresh` as "all good",
 * so a qualified `fresh` names its own caveats instead of relying on the
 * fields that follow.
 */
export function formatTrustLine(t: TrustInfo): string {
  const caveats = trustCaveats(t);
  const bits = [
    t.trust === "fresh" && caveats.length > 0
      ? `trust: fresh but ${caveats.join(" + ")} — absence ≠ proof`
      : `trust: ${t.trust}`,
    `coverage: ${t.coverage}${
      t.coverage === "sampled" ? ` (${t.files_hashed}/${t.file_count})` : ""
    }`,
    `graph: ${t.graph} (unresolved ${t.unresolved_edge_count}, weak ${t.weak_edge_count})`,
  ];
  if (t.trust === "rebuilding") {
    bits.push("host ensure in progress — absence ≠ proof");
  } else if (t.trust !== "fresh") {
    bits.push(`ensure: ${t.reindex_cmd}`);
  }
  return bits.join("  |  ");
}

/** Secondary axes that qualify a `fresh` verdict, in severity order. */
function trustCaveats(t: TrustInfo): string[] {
  const out: string[] = [];
  if (t.coverage !== "exhaustive") out.push("unverified");
  if (t.graph !== "ok") out.push("degraded");
  return out;
}

/**
 * Reasons not to treat absence as proof, for JSON consumers that read one
 * field. Covers every level, not just a qualified `fresh` — a parser should
 * never have to reconstruct this from three separate keys.
 */
function trustWarnings(t: TrustInfo): string[] {
  const out: string[] = [];
  if (t.trust === "stale") out.push("graph is behind the files on disk");
  if (t.trust === "rebuilding") out.push("host reindex in progress");
  if (t.trust === "missing") out.push("no graph indexed yet");
  if (t.coverage !== "exhaustive") {
    out.push(
      `only ${t.files_hashed}/${t.file_count} files content-verified`,
    );
  }
  if (t.graph !== "ok") {
    out.push("resolution degraded — unresolved or unique-global-weak edges");
  }
  return out;
}

export function formatTrustFull(t: TrustInfo): string {
  const notes = statusNotes(t);
  const caveats = trustCaveats(t);
  const lines = [
    `trust: ${t.trust}  (freshness vs disk)${
      t.trust === "fresh" && caveats.length > 0
        ? `  — but ${caveats.join(" + ")}; absence ≠ proof`
        : ""
    }`,
    `coverage: ${t.coverage}  hashed ${t.files_hashed}/${t.file_count}`,
    `graph: ${t.graph}`,
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
  const warnings = trustWarnings(trust);
  const payload = {
    trust: trust.trust,
    coverage: trust.coverage,
    files_hashed: trust.files_hashed,
    file_count: trust.file_count,
    graph: trust.graph,
    // present whenever absence ≠ proof, so a one-field parser cannot miss it
    warnings: warnings.length > 0 ? warnings : undefined,
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
