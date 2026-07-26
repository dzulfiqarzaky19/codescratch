import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import type { GraphDb } from "../db/client.js";
import type { TrustInfo } from "../models.js";
import { EXTRACTOR_LIMITATIONS } from "../models.js";
import { graphExists } from "../config.js";
import {
  META_INDEXED_HEAD,
  META_REINDEX_STARTED,
  META_REINDEX_STATE,
} from "../host/ensure.js";
import { getHead } from "../host/git.js";

/** Baseline hash sample when mtime says nothing changed. */
const STALE_SAMPLE_CAP = 80;
/** Ceiling on files hashed per call — mtime-flagged files come first. */
const STALE_HASH_CAP = 400;
/** mtime_ms is a float stored in an INTEGER-affinity column. */
const MTIME_TOLERANCE_MS = 1;
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
  // Host-owned freshness; agent reindex is emergency only.
  const reindexCmd = `codescratch ensure ${db.root}`;

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
      notes: ["empty graph", `init: codescratch ensure ${db.root}`],
      reindex_cmd: `codescratch ensure ${db.root}`,
    };
  }

  // Host job in flight — never claim fresh; absence ≠ proof mid-rebuild.
  const reindexState = db.getMeta(META_REINDEX_STATE);
  if (reindexState === "rebuilding") {
    const since = db.getMeta(META_REINDEX_STARTED);
    return {
      trust: "rebuilding",
      indexed_at: indexedAt,
      last_full_index_at: lastFull,
      file_count: counts.files,
      node_count: counts.nodes,
      edge_count: counts.edges,
      unresolved_edge_count: counts.unresolved,
      weak_edge_count: counts.weak,
      notes: [
        `host reindex in progress${since ? ` since ${since}` : ""} — absence ≠ proof`,
        "deep queries may see a mid-update graph; retry when trust leaves rebuilding",
      ],
      reindex_cmd: reindexCmd,
    };
  }

  // Branch/HEAD drift: one ensure job will catch up; do not lie as fresh.
  const indexedHead = db.getMeta(META_INDEXED_HEAD);
  const liveHead = getHead(db.root);
  if (
    indexedHead != null &&
    indexedHead !== "" &&
    liveHead != null &&
    liveHead !== indexedHead
  ) {
    trust = "stale";
    notes.push(
      `HEAD moved (${indexedHead.slice(0, 7)}→${liveHead.slice(0, 7)}); host ensure will catch up`,
    );
  }

  const files = db.listFiles();
  let staleFiles = 0;

  // stat every file (no read) so an edit outside the stride sample cannot pass
  // as fresh; hash only the files that could have changed, plus a baseline
  // sample to catch same-mtime rewrites.
  const suspect: typeof files = [];
  const quiet: typeof files = [];
  for (const f of files) {
    const abs = join(db.root, f.path);
    let st: { mtimeMs: number; size: number } | null = null;
    try {
      st = statSync(abs);
    } catch {
      staleFiles++; // gone or unreadable
      continue;
    }
    // size catches a timestamp-preserving rewrite of a different length;
    // size 0 means pre-v4 row (unknown) — hash it rather than trust it
    const sizeChanged = f.size_bytes === 0 || st.size !== f.size_bytes;
    if (
      sizeChanged ||
      Math.abs(st.mtimeMs - f.mtime_ms) > MTIME_TOLERANCE_MS
    ) {
      suspect.push(f);
    } else {
      quiet.push(f);
    }
  }

  const baseline =
    quiet.length <= STALE_SAMPLE_CAP
      ? quiet
      : pickSample(quiet, STALE_SAMPLE_CAP);
  const toHash = [...suspect, ...baseline].slice(0, STALE_HASH_CAP);
  const hashCapped = suspect.length + baseline.length > STALE_HASH_CAP;

  for (const f of toHash) {
    const abs = join(db.root, f.path);
    if (!existsSync(abs)) {
      staleFiles++;
      continue;
    }
    // Content hash — a touch without an edit is not stale
    try {
      const hash = sha256(readFileSync(abs));
      if (hash !== f.hash) staleFiles++;
    } catch {
      staleFiles++;
    }
  }

  if (staleFiles > 0) {
    trust = "stale";
    notes.push(`${staleFiles} content-changed — host: ${reindexCmd}`);
  }

  // `fresh` is a claim of verification, so it requires having hashed every
  // file. Anything less is `partial`: mtime+size can miss a rewrite that
  // preserves both. A note under a `fresh` header does not survive contact
  // with an agent that reads the level and stops.
  const exhaustive = !hashCapped && toHash.length === files.length;
  if (!exhaustive) {
    if (trust === "fresh") trust = "partial";
    notes.push(
      `hash-checked ${toHash.length}/${files.length} (mtime+size-gated${
        hashCapped ? `, capped at ${STALE_HASH_CAP}` : ""
      }; not exhaustive) — unchecked files could have changed silently`,
    );
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
  // Receiver-unknown is a same-file member guess and is expected on OO code;
  // only the cross-file unique-global guess should push trust to partial.
  const globalGuess = Math.max(0, counts.weak - counts.weak_receiver_unknown);
  const weakRatio = resolved === 0 ? 0 : globalGuess / resolved;
  if (counts.weak > 0) {
    notes.push(
      `weak edges: ${counts.weak}/${resolved || 0} resolved ` +
        `(${counts.weak_receiver_unknown} receiver-unknown, ${globalGuess} unique-global ` +
        `= ${(weakRatio * 100).toFixed(0)}% of resolved) — verify before refactoring`,
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
    "freshness is host-owned (codescratch ensure); cs_reindex is emergency only",
  ];
}

export function requireGraph(root: string): void {
  if (!graphExists(root)) {
    throw new Error(`No graph at ${root}. Run: codescratch ensure ${root}`);
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
