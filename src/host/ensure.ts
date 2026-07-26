import { graphExists, resolveRoot } from "../config.js";
import { GraphDb } from "../db/client.js";
import { indexRepo } from "../index/indexer.js";
import { SCHEMA_VERSION, type IndexStats } from "../models.js";
import { getHead } from "./git.js";
import {
  clearPending,
  hasPending,
  markPending,
  releaseLock,
  tryAcquireLock,
  type LockHandle,
} from "./lock.js";

export const META_REINDEX_STATE = "reindex_state";
export const META_REINDEX_STARTED = "reindex_started_at";
export const META_INDEXED_HEAD = "indexed_head";

export type ReindexState = "idle" | "rebuilding";

export interface EnsureOptions {
  full?: boolean;
  /** When lock held by another live process: wait up to ms then fail (MCP path). */
  waitMs?: number;
  /**
   * Injected indexer for tests. Default: real indexRepo.
   * Called once per drain pass.
   */
  indexFn?: typeof indexRepo;
  /** Fail-soft for hooks: swallow errors, return status instead of throw. */
  failSoft?: boolean;
}

export interface EnsureResult {
  root: string;
  ran: boolean;
  coalesced: boolean;
  passes: number;
  full: boolean;
  head: string;
  stats: IndexStats | null;
  error: string | null;
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

async function acquireWithWait(
  root: string,
  head: string,
  waitMs: number,
): Promise<LockHandle | null> {
  const deadline = Date.now() + waitMs;
  for (;;) {
    const h = tryAcquireLock(root, head);
    if (h) return h;
    markPending(root);
    if (Date.now() >= deadline) return null;
    await sleep(Math.min(200, Math.max(50, deadline - Date.now())));
  }
}

function setRebuilding(root: string, on: boolean): void {
  // Tiny separate open so MCP readers see rebuilding before long index.
  const db = GraphDb.open(root, { create: true });
  try {
    if (on) {
      db.setMeta(META_REINDEX_STATE, "rebuilding");
      db.setMeta(META_REINDEX_STARTED, new Date().toISOString());
    } else {
      db.setMeta(META_REINDEX_STATE, "idle");
      db.setMeta(META_REINDEX_STARTED, "");
    }
  } finally {
    db.close();
  }
}

function setIndexedHead(root: string, head: string): void {
  const db = GraphDb.open(root, { create: true });
  try {
    db.setMeta(META_INDEXED_HEAD, head);
    db.setMeta(META_REINDEX_STATE, "idle");
    db.setMeta(META_REINDEX_STARTED, "");
  } finally {
    db.close();
  }
}

function needsFullRebuild(root: string, forceFull: boolean): boolean {
  if (forceFull) return true;
  if (!graphExists(root)) return true;
  try {
    const db = GraphDb.open(root);
    try {
      const ver = db.getMeta("schema_version");
      if (ver !== SCHEMA_VERSION) return true;
      const counts = db.counts();
      if (counts.files === 0 && counts.nodes === 0) return true;
      return false;
    } finally {
      db.close();
    }
  } catch {
    return true;
  }
}

/**
 * Host single-flight reindex. Coalesces concurrent callers via lock + pending.
 * HEAD change alone does not force full — incremental hash rebind handles churn.
 */
export async function ensureRepo(
  rootInput?: string,
  opts?: EnsureOptions,
): Promise<EnsureResult> {
  const root = resolveRoot(rootInput);
  const indexFn = opts?.indexFn ?? indexRepo;
  const forceFull = opts?.full === true;
  const waitMs = opts?.waitMs ?? 0;
  const failSoft = opts?.failSoft === true;

  const headPeek = getHead(root) ?? "";
  let handle =
    waitMs > 0
      ? await acquireWithWait(root, headPeek, waitMs)
      : tryAcquireLock(root, headPeek);

  if (!handle) {
    markPending(root);
    return {
      root,
      ran: false,
      coalesced: true,
      passes: 0,
      full: forceFull,
      head: headPeek,
      stats: null,
      error: waitMs > 0 ? "reindex lock busy" : null,
    };
  }

  let passes = 0;
  let lastStats: IndexStats | null = null;
  let lastFull = forceFull;
  let lastHead = headPeek;
  let error: string | null = null;

  try {
    setRebuilding(root, true);
    do {
      clearPending(root);
      const head = getHead(root) ?? "";
      lastHead = head;
      const full = needsFullRebuild(root, forceFull && passes === 0);
      lastFull = full;
      lastStats = await indexFn(root, { full });
      passes++;
      setIndexedHead(root, head);
      // Only honor forced full on first pass of this hold.
    } while (hasPending(root));
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
    try {
      setRebuilding(root, false);
    } catch {
      /* ignore */
    }
    if (!failSoft) {
      releaseLock(handle);
      throw e;
    }
  } finally {
    releaseLock(handle);
  }

  return {
    root,
    ran: passes > 0,
    coalesced: false,
    passes,
    full: lastFull,
    head: lastHead,
    stats: lastStats,
    error,
  };
}
