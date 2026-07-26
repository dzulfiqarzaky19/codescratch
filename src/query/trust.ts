import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import type { GraphDb } from "../db/client.js";
import type { GraphQuality, TrustInfo } from "../models.js";
import { EXTRACTOR_LIMITATIONS } from "../models.js";
import { graphExists } from "../config.js";
import {
  META_INDEXED_HEAD,
  META_REINDEX_STARTED,
  META_REINDEX_STATE,
} from "../host/ensure.js";
import { getHead } from "../host/git.js";

/**
 * Ceiling on files hashed per call. computeTrust runs on every query tool, so
 * the stat gate does the real work (~15ms for 2000 files) and hashing only
 * confirms it. Suspects are always hashed; quiet files are extra assurance.
 */
const STALE_HASH_CAP = 4000;
/**
 * Byte ceiling for hashing quiet files. Measured: 11MB ≈ 570ms, which is too
 * slow to pay per query for the narrow case of a rewrite that preserved both
 * mtime and size. Past this, coverage reports `sampled` instead.
 */
const HASH_BYTE_BUDGET = 4 * 1024 * 1024;
/** mtime_ms is a float stored in an INTEGER-affinity column. */
const MTIME_TOLERANCE_MS = 1;
/** Unique-global weak edges / resolved above this → graph degraded. */
const WEAK_RATIO_DEGRADED = 0.08;
/** Unresolved / all edges above this → graph degraded. */
const UNRESOLVED_RATIO_DEGRADED = 0.35;

/**
 * @param opts.hashBudget max files to hash — test/override only; production
 * callers should omit it and take `STALE_HASH_CAP`.
 */
export function computeTrust(
  db: GraphDb,
  opts?: { hashBudget?: number },
): TrustInfo {
  const hashBudget = opts?.hashBudget ?? STALE_HASH_CAP;
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
      coverage: "exhaustive",
      files_hashed: 0,
      graph: "ok",
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
      // nothing was verified — the graph is mid-write
      coverage: "sampled",
      files_hashed: 0,
      graph: graphQuality(counts).quality,
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

  // stat every file (cheap, no read): mtime or size drift makes a file suspect.
  // Size catches a timestamp-preserving rewrite of a different length; a size
  // of 0 means a pre-v4 row (unknown), so hash it rather than trust it.
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

  // Hash suspects first — they are the only files that can prove staleness
  // cheaply. Then spend what is left of the budget confirming quiet files,
  // which only catches a rewrite preserving BOTH mtime and size.
  const quietBudget = Math.max(0, hashBudget - suspect.length);
  const quietToHash =
    quiet.length <= quietBudget ? quiet : pickSample(quiet, quietBudget);
  let hashCapped = quietToHash.length < quiet.length;
  let hashedCount = 0;
  let bytesRead = 0;

  const hashOne = (f: (typeof files)[number]): void => {
    const abs = join(db.root, f.path);
    hashedCount++;
    if (!existsSync(abs)) {
      staleFiles++;
      return;
    }
    try {
      const buf = readFileSync(abs);
      bytesRead += buf.byteLength;
      if (sha256(buf) !== f.hash) staleFiles++;
    } catch {
      staleFiles++;
    }
  };

  for (const f of suspect) hashOne(f);

  // Already stale, or over the byte budget: stop reading. Every query tool
  // calls this, so verification past the point of a decision is wasted I/O.
  for (const f of quietToHash) {
    if (staleFiles > 0 || bytesRead >= HASH_BYTE_BUDGET) {
      hashCapped = true;
      break;
    }
    hashOne(f);
  }

  if (staleFiles > 0) {
    trust = "stale";
    notes.push(`${staleFiles} content-changed — host: ${reindexCmd}`);
  }

  // Coverage is its own axis: it qualifies how well `trust` was verified and
  // never downgrades `trust` itself. `fresh + sampled` is an honest statement —
  // "matches disk as far as we looked" — where a bare `fresh` would overclaim.
  const exhaustive = !hashCapped && hashedCount === files.length;
  if (!exhaustive) {
    notes.push(
      trust === "stale"
        ? `hash-checked ${hashedCount}/${files.length} — stopped early; drift already proven`
        : `hash-checked ${hashedCount}/${files.length} (mtime+size-gated${
            hashCapped ? "; hash budget reached" : ""
          }) — unchecked files could have changed with mtime and size intact`,
    );
  }

  const quality = graphQuality(counts);
  notes.push(...quality.notes);

  return {
    trust,
    coverage: exhaustive ? "exhaustive" : "sampled",
    files_hashed: hashedCount,
    graph: quality.quality,
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

/**
 * Resolution quality, independent of freshness. A repo importing external
 * packages is permanently `degraded` — that is a true statement about the
 * graph, and keeping it off the freshness axis is the point of the split.
 */
function graphQuality(counts: ReturnType<GraphDb["counts"]>): {
  quality: GraphQuality;
  notes: string[];
} {
  const notes: string[] = [];
  let quality: GraphQuality = "ok";

  const unresolvedRatio =
    counts.edges === 0 ? 0 : counts.unresolved / counts.edges;
  if (unresolvedRatio > UNRESOLVED_RATIO_DEGRADED) {
    quality = "degraded";
    notes.push(
      `unresolved edge ratio ${(unresolvedRatio * 100).toFixed(0)}% — packages/dynamic may be missing`,
    );
  } else if (counts.unresolved > 0) {
    notes.push(
      `${counts.unresolved} unresolved edges — absence ≠ no reference`,
    );
  }

  const resolved = Math.max(0, counts.edges - counts.unresolved);
  // receiver-unknown is a same-file member guess and expected on OO code; only
  // the cross-file unique-global guess signals a degraded graph
  const globalGuess = Math.max(0, counts.weak - counts.weak_receiver_unknown);
  const weakRatio = resolved === 0 ? 0 : globalGuess / resolved;
  if (counts.weak > 0) {
    notes.push(
      `weak edges: ${counts.weak}/${resolved || 0} resolved ` +
        `(${counts.weak_receiver_unknown} receiver-unknown, ${globalGuess} unique-global ` +
        `= ${(weakRatio * 100).toFixed(0)}% of resolved) — verify before refactoring`,
    );
    if (weakRatio >= WEAK_RATIO_DEGRADED) quality = "degraded";
  }

  return { quality, notes };
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
