import { resolveRoot, SEARCH_DEFAULT_LIMIT } from "../config.js";
import { GraphDb } from "../db/client.js";
import { formatNodeShort, wrapResult } from "./format.js";
import { computeTrust, requireGraph } from "./trust.js";

export function searchSymbols(
  query: string,
  rootInput?: string,
  limit = SEARCH_DEFAULT_LIMIT,
): string {
  const root = resolveRoot(rootInput);
  requireGraph(root);
  const db = GraphDb.open(root);
  try {
    const trust = computeTrust(db);
    const fts = db.searchFts(query, limit);
    const byName = db.findNodesByName(query, limit);
    const seen = new Set<string>();
    const merged = [...fts, ...byName].filter((n) => {
      if (seen.has(n.id)) return false;
      seen.add(n.id);
      return true;
    });

    if (merged.length === 0) {
      return wrapResult(trust, `No symbols matched ${JSON.stringify(query)}.`, {
        results: [],
      });
    }

    const body = [
      `search: ${query}  (${merged.length} hit(s))`,
      ...merged.map((n, i) => `${i + 1}. ${formatNodeShort(n)}`),
    ].join("\n");

    return wrapResult(trust, body, {
      results: merged.map((n) => ({
        id: n.id,
        kind: n.kind,
        name: n.name,
        qualified_name: n.qualified_name,
        file_path: n.file_path,
        start_line: n.start_line,
        end_line: n.end_line,
        exported: n.exported,
      })),
    });
  } finally {
    db.close();
  }
}
