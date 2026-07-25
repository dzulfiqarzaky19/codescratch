import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { GraphDb } from "../db/client.js";
import type { TrustInfo } from "../models.js";
import { EXTRACTOR_LIMITATIONS } from "../models.js";
import { graphExists } from "../config.js";

/** Sample size for hash-based stale checks (content, not mtime). */
const STALE_SAMPLE_CAP = 80;
/** Weak edges / resolved edges above this → partial (not a single weak stain). */
const WEAK_RATIO_PARTIAL = 0.08;
/** Unresolved / all edges above this → partial. */
const UNRESOLVED_RATIO_PARTIAL = 0.35;

export function computeTrust(db: GraphDb): TrustInfo {
  const counts = db.counts();
  const indexedAt = db.getMeta("last_index_at");
  const lastFull = db.getMeta("last_full_index_at");
  const notes: string[] = [];
  let trust: TrustInfo["trust"] = "fresh";
  const reindexCmd = `codescratch reindex ${db.root}`;

  if (counts.files === 0) {
    return {
      trust: "missing",
      indexed_at: indexedAt,
      last_full_index_at: lastFull,
      file_count: 0,
      node_count: 0,
      edge_count: 0,
      unresolved_edge_count: 0,
      weak_edge_count: 0,
      notes: ["empty graph", `init: codescratch init ${db.root}`],
      reindex_cmd: `codescratch init ${db.root}`,
    };
  }

  const files = db.listFiles();
  let staleFiles = 0;
  const sample =
    files.length <= STALE_SAMPLE_CAP
      ? files
      : pickSample(files, STALE_SAMPLE_CAP);

  for (const f of sample) {
    const abs = join(db.root, f.path);
    if (!existsSync(abs)) {
      staleFiles++;
      continue;
    }
    // Content hash — ignore touch-without-change
    try {
      const hash = sha256(readFileSync(abs));
      if (hash !== f.hash) staleFiles++;
    } catch {
      staleFiles++;
    }
  }

  if (staleFiles > 0) {
    trust = "stale";
    const approx =
      files.length > STALE_SAMPLE_CAP
        ? `≥${staleFiles} content-changed in sample ${sample.length}/${files.length}`
        : `${staleFiles} content-changed`;
    notes.push(`${approx} — run: ${reindexCmd}`);
  }

  const unresolvedRatio =
    counts.edges === 0 ? 0 : counts.unresolved / counts.edges;
  if (unresolvedRatio > UNRESOLVED_RATIO_PARTIAL) {
    if (trust !== "stale") trust = "partial";
    notes.push(
      `unresolved edge ratio ${(unresolvedRatio * 100).toFixed(0)}% — packages/dynamic may be missing`,
    );
  } else if (counts.unresolved > 0) {
    notes.push(
      `${counts.unresolved} unresolved edges — absence ≠ no reference`,
    );
  }

  const resolved = Math.max(0, counts.edges - counts.unresolved);
  const weakRatio = resolved === 0 ? 0 : counts.weak / resolved;
  if (counts.weak > 0) {
    notes.push(
      `weak edges: ${counts.weak}/${resolved || 0} resolved (${(weakRatio * 100).toFixed(0)}%) — unique-global fallback; verify`,
    );
    if (trust === "fresh" && weakRatio >= WEAK_RATIO_PARTIAL) {
      trust = "partial";
    }
  }

  return {
    trust,
    indexed_at: indexedAt,
    last_full_index_at: lastFull,
    file_count: counts.files,
    node_count: counts.nodes,
    edge_count: counts.edges,
    unresolved_edge_count: counts.unresolved,
    weak_edge_count: counts.weak,
    notes,
    reindex_cmd: reindexCmd,
  };
}

/** Full notes for cs_status only. */
export function statusNotes(trust: TrustInfo): string[] {
  return [
    ...trust.notes,
    "static graph only; verify critical paths under dynamic dispatch/DI",
    `extractor misses: ${EXTRACTOR_LIMITATIONS.join("; ")}`,
  ];
}

export function requireGraph(root: string): void {
  if (!graphExists(root)) {
    throw new Error(`No graph at ${root}. Run: codescratch init ${root}`);
  }
}

function pickSample<T>(items: T[], n: number): T[] {
  if (items.length <= n) return items;
  const out: T[] = [];
  const step = items.length / n;
  for (let i = 0; i < n; i++) {
    out.push(items[Math.floor(i * step)]!);
  }
  return out;
}

function sha256(buf: Buffer | string): string {
  return createHash("sha256").update(buf).digest("hex");
}
