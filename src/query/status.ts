import { resolveRoot } from "../config.js";
import { GraphDb } from "../db/client.js";
import { wrapResult } from "./format.js";
import { computeTrust, requireGraph } from "./trust.js";

export function statusReport(rootInput?: string): string {
  const root = resolveRoot(rootInput);
  requireGraph(root);
  const db = GraphDb.open(root);
  try {
    const trust = computeTrust(db);
    const counts = db.counts();
    const body = [
      `root: ${root}`,
      `schema: ${db.getMeta("schema_version") ?? "?"}`,
      `last_index_at: ${db.getMeta("last_index_at") ?? "n/a"}`,
      `last_full_index_at: ${db.getMeta("last_full_index_at") ?? "n/a"}`,
      `trust: ${trust.trust}  coverage: ${trust.coverage}  graph: ${trust.graph}`,
      `bindings: ${counts.bindings}`,
      `reindex: ${trust.reindex_cmd}`,
    ].join("\n");
    return wrapResult(
      trust,
      body,
      { root, bindings: counts.bindings },
      { verbose: true },
    );
  } finally {
    db.close();
  }
}
