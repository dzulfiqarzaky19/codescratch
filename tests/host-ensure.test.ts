import { describe, expect, it, afterEach } from "vitest";
import {
  mkdirSync,
  mkdtempSync,
  rmSync,
  writeFileSync,
  existsSync,
  readFileSync,
} from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";
import { ensureRepo } from "../src/host/ensure.js";
import {
  hasPending,
  isLocked,
  markPending,
  tryAcquireLock,
  releaseLock,
  writeLockForTest,
} from "../src/host/lock.js";
import { setHeadReaderForTest } from "../src/host/git.js";
import { GraphDb } from "../src/db/client.js";
import { computeTrust } from "../src/query/trust.js";
import { indexRepo } from "../src/index/indexer.js";
import {
  META_INDEXED_HEAD,
  META_REINDEX_STATE,
} from "../src/host/ensure.js";

const roots: string[] = [];

function tempRoot(): string {
  const root = mkdtempSync(join(tmpdir(), "cs-ensure-"));
  roots.push(root);
  mkdirSync(join(root, "src"), { recursive: true });
  writeFileSync(
    join(root, "src/a.ts"),
    "export function a(): number { return 1; }\n",
  );
  return root;
}

afterEach(() => {
  setHeadReaderForTest(null);
  for (const r of roots.splice(0)) {
    try {
      rmSync(r, { recursive: true, force: true });
    } catch {
      /* win lock */
    }
  }
});

describe("lock", () => {
  it("second acquire fails while held; pending can be marked", () => {
    const root = tempRoot();
    const h = tryAcquireLock(root, "abc");
    expect(h).toBeTruthy();
    expect(tryAcquireLock(root, "abc")).toBeNull();
    markPending(root);
    expect(hasPending(root)).toBe(true);
    releaseLock(h!);
    expect(isLocked(root)).toBe(false);
  });

  it("steals lock from dead pid", () => {
    const root = tempRoot();
    writeLockForTest(root, {
      pid: 99999999,
      startedAt: new Date(0).toISOString(),
      root,
      head: "",
    });
    const h = tryAcquireLock(root, "");
    expect(h).toBeTruthy();
    releaseLock(h!);
  });
});

describe("ensureRepo single-flight", () => {
  it("coalesces concurrent ensure into one indexer + pending drain", async () => {
    const root = tempRoot();
    let active = 0;
    let maxActive = 0;
    let calls = 0;

    const slowIndex: typeof indexRepo = async (r, opts) => {
      calls++;
      active++;
      maxActive = Math.max(maxActive, active);
      await new Promise((res) => setTimeout(res, 80));
      active--;
      return indexRepo(r, opts);
    };

    const p1 = ensureRepo(root, { indexFn: slowIndex });
    await new Promise((r) => setTimeout(r, 20));
    const p2 = ensureRepo(root, { indexFn: slowIndex });
    const [a, b] = await Promise.all([p1, p2]);

    expect(maxActive).toBe(1);
    // one holder runs; other coalesces. Holder may drain pending → ≤2 passes total.
    expect(calls).toBeGreaterThanOrEqual(1);
    expect(calls).toBeLessThanOrEqual(2);
    expect(a.coalesced || b.coalesced).toBe(true);
    expect(a.ran || b.ran).toBe(true);
    expect(isLocked(root)).toBe(false);
  });

  it("edit burst while locked yields at most one trailing pass", async () => {
    const root = tempRoot();
    let calls = 0;
    const slowIndex: typeof indexRepo = async (r, opts) => {
      calls++;
      await new Promise((res) => setTimeout(res, 60));
      return indexRepo(r, opts);
    };

    const holder = ensureRepo(root, { indexFn: slowIndex });
    await new Promise((r) => setTimeout(r, 15));
    const extras = await Promise.all(
      Array.from({ length: 5 }, () =>
        ensureRepo(root, { indexFn: slowIndex }),
      ),
    );
    await holder;
    expect(extras.every((e) => e.coalesced)).toBe(true);
    // 1 initial + optional 1 drain
    expect(calls).toBeLessThanOrEqual(2);
  });

  it("records indexed_head after success", async () => {
    const root = tempRoot();
    setHeadReaderForTest(() => "deadbeefcafebabe000000000000000000000001");
    const r = await ensureRepo(root, { full: true });
    expect(r.ran).toBe(true);
    expect(r.head).toMatch(/^deadbeef/);
    const db = GraphDb.open(root);
    try {
      expect(db.getMeta(META_INDEXED_HEAD)).toBe(r.head);
      expect(db.getMeta(META_REINDEX_STATE)).toBe("idle");
    } finally {
      db.close();
    }
  });

  it("HEAD change alone is one incremental job (not parallel)", async () => {
    const root = tempRoot();
    let head = "aaa1111111111111111111111111111111111111";
    setHeadReaderForTest(() => head);
    let calls = 0;
    const counting: typeof indexRepo = async (r, opts) => {
      calls++;
      return indexRepo(r, opts);
    };
    await ensureRepo(root, { full: true, indexFn: counting });
    expect(calls).toBe(1);
    head = "bbb2222222222222222222222222222222222222";
    writeFileSync(
      join(root, "src/a.ts"),
      "export function a(): number { return 2; }\n",
    );
    const before = calls;
    await ensureRepo(root, { indexFn: counting });
    expect(calls - before).toBe(1);
    const db = GraphDb.open(root);
    try {
      expect(db.getMeta(META_INDEXED_HEAD)).toBe(head);
    } finally {
      db.close();
    }
  });

  it("non-git root still ensures", async () => {
    const root = tempRoot();
    setHeadReaderForTest(() => null);
    const r = await ensureRepo(root, { full: true });
    expect(r.ran).toBe(true);
    expect(r.head).toBe("");
    expect(existsSync(join(root, ".codescratch", "graph.db"))).toBe(true);
  });
});

describe("trust host states", () => {
  it("rebuilding meta → trust rebuilding", async () => {
    const root = tempRoot();
    await indexRepo(root, { full: true });
    const db = GraphDb.open(root);
    try {
      db.setMeta(META_REINDEX_STATE, "rebuilding");
      db.setMeta("reindex_started_at", new Date().toISOString());
      const t = computeTrust(db);
      expect(t.trust).toBe("rebuilding");
      expect(t.notes.join("\n")).toMatch(/in progress/);
    } finally {
      db.close();
    }
  });

  it("HEAD drift → stale", async () => {
    const root = tempRoot();
    await indexRepo(root, { full: true });
    const db = GraphDb.open(root);
    try {
      db.setMeta(META_INDEXED_HEAD, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
      db.setMeta(META_REINDEX_STATE, "idle");
    } finally {
      db.close();
    }
    setHeadReaderForTest(() => "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    const db2 = GraphDb.open(root);
    try {
      const t = computeTrust(db2);
      expect(t.trust).toBe("stale");
      expect(t.notes.join("\n")).toMatch(/HEAD moved/);
    } finally {
      db2.close();
    }
  });
});

describe("git branch hop", () => {
  it("checkout other branch → one ensure job", async () => {
    const root = tempRoot();
    try {
      execFileSync("git", ["init"], { cwd: root, stdio: "ignore" });
      execFileSync("git", ["config", "user.email", "t@t.com"], {
        cwd: root,
        stdio: "ignore",
      });
      execFileSync("git", ["config", "user.name", "t"], {
        cwd: root,
        stdio: "ignore",
      });
      execFileSync("git", ["add", "-A"], { cwd: root, stdio: "ignore" });
      execFileSync("git", ["commit", "-m", "m1"], {
        cwd: root,
        stdio: "ignore",
      });
      execFileSync("git", ["checkout", "-b", "feat"], {
        cwd: root,
        stdio: "ignore",
      });
      writeFileSync(
        join(root, "src/b.ts"),
        "export function b(): number { return 1; }\n",
      );
      execFileSync("git", ["add", "-A"], { cwd: root, stdio: "ignore" });
      execFileSync("git", ["commit", "-m", "m2"], {
        cwd: root,
        stdio: "ignore",
      });
    } catch {
      // git unavailable — skip
      return;
    }

    setHeadReaderForTest(null); // use real git
    let calls = 0;
    const counting: typeof indexRepo = async (r, opts) => {
      calls++;
      return indexRepo(r, opts);
    };
    await ensureRepo(root, { full: true, indexFn: counting });
    const afterInit = calls;
    // default branch is master or main depending on git config
    let hopped = false;
    for (const b of ["master", "main"]) {
      try {
        execFileSync("git", ["checkout", b], {
          cwd: root,
          stdio: "ignore",
        });
        hopped = true;
        break;
      } catch {
        /* try next */
      }
    }
    if (!hopped) return;
    await ensureRepo(root, { indexFn: counting });
    expect(calls - afterInit).toBe(1);
  });
});
