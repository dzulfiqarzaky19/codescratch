import { describe, expect, it } from "vitest";
import {
  mkdirSync,
  mkdtempSync,
  rmSync,
  statSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { indexRepo } from "../src/index/indexer.js";
import { GraphDb } from "../src/db/client.js";
import { computeTrust } from "../src/query/trust.js";

const FILE_COUNT = 120;
const SAMPLE_CAP = 80;

/** Mirrors pickSample's stride so the test targets a genuinely skipped file. */
function strideIndices(total: number, n: number): Set<number> {
  const out = new Set<number>();
  const step = total / n;
  for (let i = 0; i < n; i++) out.add(Math.floor(i * step));
  return out;
}

function makeRepo(): string {
  const root = mkdtempSync(join(tmpdir(), "cs-trust-"));
  mkdirSync(join(root, "src"), { recursive: true });
  for (let i = 0; i < FILE_COUNT; i++) {
    const id = String(i).padStart(3, "0");
    writeFileSync(
      join(root, `src/f${id}.ts`),
      `export function fn${id}(): number { return ${i}; }\n`,
    );
  }
  return root;
}

describe("trust staleness", () => {
  it("detects an edit to a file the stride sample skips", async () => {
    const root = makeRepo();
    try {
      await indexRepo(root, { full: true });

      const sampled = strideIndices(FILE_COUNT, SAMPLE_CAP);
      const skipped = [...Array(FILE_COUNT).keys()].find(
        (i) => !sampled.has(i),
      );
      expect(skipped).toBeDefined();

      let db = GraphDb.open(root);
      try {
        expect(db.listFiles().length).toBe(FILE_COUNT);
        expect(computeTrust(db).trust).not.toBe("stale");
      } finally {
        db.close();
      }

      const id = String(skipped).padStart(3, "0");
      writeFileSync(
        join(root, `src/f${id}.ts`),
        `export function fn${id}(): number { return 999; }\n`,
      );

      db = GraphDb.open(root);
      try {
        const t = computeTrust(db);
        expect(t.trust).toBe("stale");
        expect(t.notes.join("\n")).toMatch(/content-changed/);
      } finally {
        db.close();
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("never claims fresh when hash coverage is non-exhaustive", async () => {
    const root = makeRepo();
    try {
      await indexRepo(root, { full: true });
      const db = GraphDb.open(root);
      try {
        const t = computeTrust(db);
        // unedited, but only 80/120 hashed — verification was partial, so the
        // level must not read as proof
        expect(t.trust).toBe("partial");
        expect(t.notes.join("\n")).toMatch(
          /hash-checked \d+\/120 \(mtime\+size-gated; not exhaustive\)/,
        );
      } finally {
        db.close();
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("is fresh when every file was hashed", async () => {
    // under the sample cap → exhaustive → fresh is an honest claim
    const root = mkdtempSync(join(tmpdir(), "cs-trust-small-"));
    try {
      mkdirSync(join(root, "src"), { recursive: true });
      for (let i = 0; i < 5; i++) {
        writeFileSync(
          join(root, `src/s${i}.ts`),
          `export function s${i}(): number { return ${i}; }\n`,
        );
      }
      await indexRepo(root, { full: true });
      const db = GraphDb.open(root);
      try {
        const t = computeTrust(db);
        expect(t.trust).toBe("fresh");
        expect(t.notes.join("\n")).not.toMatch(/not exhaustive/);
      } finally {
        db.close();
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("detects a timestamp-preserving rewrite via size, and never says fresh", async () => {
    const root = makeRepo();
    try {
      await indexRepo(root, { full: true });
      const sampled = strideIndices(FILE_COUNT, SAMPLE_CAP);
      const skipped = [...Array(FILE_COUNT).keys()].find((i) => !sampled.has(i))!;
      const id = String(skipped).padStart(3, "0");
      const p = join(root, `src/f${id}.ts`);

      // rewrite with a different length, then restore the timestamps
      const st = statSync(p);
      writeFileSync(p, `export function fn${id}(): number { return 1234567; }\n`);
      utimesSync(p, st.atime, st.mtime);

      const db = GraphDb.open(root);
      try {
        const t = computeTrust(db);
        expect(t.trust).toBe("stale");
      } finally {
        db.close();
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
