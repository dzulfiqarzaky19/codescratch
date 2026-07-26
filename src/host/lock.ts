import {
  existsSync,
  mkdirSync,
  openSync,
  closeSync,
  readFileSync,
  unlinkSync,
  writeFileSync,
  statSync,
} from "node:fs";
import { join } from "node:path";
import { graphDir } from "../config.js";

const LOCK_NAME = "reindex.lock";
const PENDING_NAME = "pending";
/** Steal lock if holder looks dead or older than this. */
export const LOCK_TTL_MS = 30 * 60 * 1000;

export interface LockInfo {
  pid: number;
  startedAt: string;
  root: string;
  head: string;
}

export interface LockHandle {
  root: string;
  path: string;
}

function lockPath(root: string): string {
  return join(graphDir(root), LOCK_NAME);
}

function pendingPath(root: string): string {
  return join(graphDir(root), PENDING_NAME);
}

function ensureDir(root: string): void {
  mkdirSync(graphDir(root), { recursive: true });
}

function pidAlive(pid: number): boolean {
  if (!Number.isFinite(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function readLock(root: string): LockInfo | null {
  const p = lockPath(root);
  if (!existsSync(p)) return null;
  try {
    return JSON.parse(readFileSync(p, "utf8")) as LockInfo;
  } catch {
    return null;
  }
}

function lockAgeMs(root: string): number {
  try {
    return Date.now() - statSync(lockPath(root)).mtimeMs;
  } catch {
    return Number.POSITIVE_INFINITY;
  }
}

function shouldSteal(root: string): boolean {
  const info = readLock(root);
  if (!info) return true;
  if (!pidAlive(info.pid)) return true;
  if (lockAgeMs(root) > LOCK_TTL_MS) return true;
  return false;
}

/**
 * Exclusive lock via O_EXCL create. On contention: if holder dead/stale, steal;
 * else return null (caller should mark pending).
 */
export function tryAcquireLock(
  root: string,
  head = "",
): LockHandle | null {
  ensureDir(root);
  const path = lockPath(root);
  const payload: LockInfo = {
    pid: process.pid,
    startedAt: new Date().toISOString(),
    root,
    head,
  };
  const body = JSON.stringify(payload);

  try {
    const fd = openSync(path, "wx");
    try {
      writeFileSync(fd, body);
    } finally {
      closeSync(fd);
    }
    return { root, path };
  } catch (e) {
    const err = e as NodeJS.ErrnoException;
    if (err.code !== "EEXIST") throw e;
    if (!shouldSteal(root)) return null;
    try {
      unlinkSync(path);
    } catch {
      return null;
    }
    try {
      const fd = openSync(path, "wx");
      try {
        writeFileSync(fd, body);
      } finally {
        closeSync(fd);
      }
      return { root, path };
    } catch {
      return null;
    }
  }
}

export function releaseLock(handle: LockHandle): void {
  try {
    const cur = readLock(handle.root);
    // Only unlink if we still own it (or file is corrupt).
    if (cur && cur.pid !== process.pid) return;
    unlinkSync(handle.path);
  } catch {
    /* ignore */
  }
}

export function markPending(root: string): void {
  ensureDir(root);
  writeFileSync(pendingPath(root), `${Date.now()}\n`);
}

export function clearPending(root: string): void {
  try {
    unlinkSync(pendingPath(root));
  } catch {
    /* ignore */
  }
}

export function hasPending(root: string): boolean {
  return existsSync(pendingPath(root));
}

export function isLocked(root: string): boolean {
  return existsSync(lockPath(root));
}

/** Test helper: write a lock file claiming an arbitrary pid. */
export function writeLockForTest(root: string, info: LockInfo): void {
  ensureDir(root);
  writeFileSync(lockPath(root), JSON.stringify(info));
}
