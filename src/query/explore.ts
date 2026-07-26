import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { resolveRoot } from "../config.js";
import { GraphDb } from "../db/client.js";
import type { GraphNode } from "../models.js";
import { formatNodeDetail, formatNodeShort, wrapResult } from "./format.js";
import { computeTrust, requireGraph } from "./trust.js";

export function exploreSymbol(target: string, rootInput?: string): string {
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
        `No symbol/path matched ${JSON.stringify(target)}. Try cs_search.`,
        { results: [] },
      );
    }
    const ambiguous = ambiguityNote(match);

    const outEdges = db.edgesFrom(node.id);
    const inEdges = db.edgesTo(node.id);
    const children = outEdges
      .filter((e) => e.kind === "has_method" || e.kind === "has_member")
      .map((e) => (e.dst_id ? db.getNode(e.dst_id) : null))
      .filter((n): n is GraphNode => !!n);

    const calls = outEdges.filter((e) => e.kind === "calls");
    const callers = inEdges.filter((e) => e.kind === "calls");
    const imports = outEdges.filter((e) => e.kind === "imports");
    const heritage = outEdges.filter(
      (e) => e.kind === "extends" || e.kind === "implements",
    );
    const bindings = db.bindingsInFile(node.file_path);

    const snippet = readSnippet(root, node);
    const lines: string[] = [
      ...(ambiguous ? [ambiguous, ""] : []),
      "explore:",
      formatNodeDetail(node, snippet),
      "",
      `members (${children.length}):`,
      ...children.slice(0, 30).map((c) => `  - ${formatNodeShort(c)}`),
      "",
      `calls (${calls.length}):`,
      ...calls.slice(0, 30).map((e) => formatEdgeLine(db, e, "out")),
      "",
      `callers (${callers.length}):`,
      ...callers.slice(0, 30).map((e) => formatEdgeLine(db, e, "in")),
      "",
      `imports (${imports.length}):`,
      ...imports.slice(0, 20).map((e) => formatEdgeLine(db, e, "out")),
      "",
      `bindings (${bindings.length}):`,
      ...bindings
        .slice(0, 30)
        .map(
          (b) =>
            `  - ${b.local_name} ← ${b.imported_name} from ${b.module_specifier}` +
            (b.module_path ? ` → ${b.module_path}` : " (unresolved)") +
            (b.is_namespace ? " [ns]" : "") +
            (b.is_type_only ? " [type]" : ""),
        ),
      "",
      `heritage (${heritage.length}):`,
      ...heritage.map((e) => formatEdgeLine(db, e, "out")),
    ];

    return wrapResult(trust, lines.join("\n"), {
      node: {
        id: node.id,
        kind: node.kind,
        qualified_name: node.qualified_name,
        file_path: node.file_path,
        start_line: node.start_line,
        end_line: node.end_line,
      },
      calls: calls.length,
      callers: callers.length,
      imports: imports.length,
      candidates: candidatePayload(match),
    });
  } finally {
    db.close();
  }
}

export interface TargetMatch {
  node: GraphNode | null;
  /** every match when >1 — the pick is a heuristic and must be disclosed */
  candidates: GraphNode[];
}

export function resolveTarget(db: GraphDb, target: string): TargetMatch {
  const one = (node: GraphNode | null): TargetMatch => ({
    node,
    candidates: [],
  });

  // exact id
  const byId = db.getNode(target);
  if (byId) return one(byId);

  // path
  const asPath = target.replace(/\\/g, "/").replace(/^\.\//, "");
  const fileNodes = db.nodesInFile(asPath);
  if (fileNodes.length) {
    return one(fileNodes.find((n) => n.kind === "file") ?? fileNodes[0] ?? null);
  }

  // qualified name
  const q = db.findNodesByQualifiedName(target);
  if (q.length === 1) return one(q[0] ?? null);
  if (q.length > 1) {
    return {
      node: q.find((n) => n.exported) ?? q[0] ?? null,
      candidates: q,
    };
  }

  // bare name
  const byName = db.findNodesByName(target, 20);
  if (byName.length === 1) return one(byName[0] ?? null);
  if (byName.length > 1) {
    const exported = byName.filter((n) => n.exported);
    if (exported.length === 1) {
      return { node: exported[0] ?? null, candidates: byName };
    }
    // prefer function/class over file
    const ranked = [...byName].sort(
      (a, b) => kindRank(a.kind) - kindRank(b.kind),
    );
    return { node: ranked[0] ?? null, candidates: byName };
  }

  // name@path
  if (target.includes("@")) {
    const [name, path] = target.split("@");
    if (name && path) {
      const hit = db
        .nodesInFile(path.replace(/\\/g, "/"))
        .find((n) => n.name === name || n.qualified_name === name);
      if (hit) return one(hit);
    }
  }

  return one(null);
}

/** Disclosure line when the pick was a guess among several matches. */
export function ambiguityNote(m: TargetMatch): string | null {
  if (!m.node || m.candidates.length < 2) return null;
  const others = m.candidates
    .filter((c) => c.id !== m.node!.id)
    .slice(0, 5)
    .map((c) => `${c.kind} ${c.qualified_name} (${c.file_path}:${c.start_line})`);
  return (
    `ambiguous: ${m.candidates.length} matched, showing ${m.node.qualified_name} ` +
    `(${m.node.file_path}:${m.node.start_line}) — others: ${others.join(", ")}`
  );
}

/** JSON payload form — same list, machine-readable. */
export function candidatePayload(m: TargetMatch) {
  if (m.candidates.length < 2) return undefined;
  return m.candidates.map((c) => ({
    id: c.id,
    kind: c.kind,
    qualified_name: c.qualified_name,
    file_path: c.file_path,
    start_line: c.start_line,
  }));
}

function kindRank(kind: string): number {
  const order = [
    "function",
    "class",
    "method",
    "interface",
    "type",
    "variable",
    "module",
    "file",
  ];
  const i = order.indexOf(kind);
  return i === -1 ? 99 : i;
}

function readSnippet(root: string, node: GraphNode): string | undefined {
  const abs = join(root, node.file_path);
  if (!existsSync(abs)) return undefined;
  const lines = readFileSync(abs, "utf8").split(/\r?\n/);
  const start = Math.max(0, node.start_line - 1);
  const end = Math.min(lines.length, Math.min(node.end_line, start + 40));
  return lines
    .slice(start, end)
    .map((l, i) => `${String(start + i + 1).padStart(4, " ")}| ${l}`)
    .join("\n");
}

function formatEdgeLine(
  db: GraphDb,
  e: {
    src_id: string | null;
    dst_id: string | null;
    kind: string;
    raw_name: string;
    resolved: boolean;
    confidence: string | null;
    reason: string | null;
    line: number;
    file_path: string;
  },
  dir: "in" | "out",
): string {
  const otherId = dir === "out" ? e.dst_id : e.src_id;
  const other = otherId ? db.getNode(otherId) : null;
  const conf =
    e.resolved && e.confidence ? ` conf=${e.confidence}` : e.resolved ? "" : " (unresolved)";
  const why = e.resolved && e.reason ? ` reason=${e.reason}` : "";
  const label = other
    ? formatNodeShort(other)
    : `${e.raw_name}${e.resolved ? "" : " (unresolved)"}`;
  return `  - [${e.kind}] ${label}  @${e.file_path}:${e.line}${conf}${why}`;
}
