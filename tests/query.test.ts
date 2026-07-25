import { describe, expect, it, beforeAll, afterAll } from "vitest";
import { mkdtempSync, rmSync, cpSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { indexRepo } from "../src/index/indexer.js";
import { GraphDb } from "../src/db/client.js";
import { searchSymbols } from "../src/query/search.js";
import { exploreSymbol } from "../src/query/explore.js";
import { listCallers } from "../src/query/callers.js";
import { impactAnalysis } from "../src/query/impact.js";
import { statusReport } from "../src/query/status.js";

const fixtureSrc = join(__dirname, "fixtures/mini-repo");
let root: string;

beforeAll(async () => {
  root = mkdtempSync(join(tmpdir(), "cs-query-"));
  cpSync(fixtureSrc, root, { recursive: true });
  await indexRepo(root, { full: true });
});

afterAll(() => {
  rmSync(root, { recursive: true, force: true });
});

describe("query layer", () => {
  it("status reports trust and reindex cmd", () => {
    const s = statusReport(root);
    expect(s).toMatch(/trust:/);
    expect(s).toMatch(/reindex:/);
    expect(s).toMatch(/last_full_index_at:/);
  });

  it("search finds Calculator", () => {
    const s = searchSymbols("Calculator", root);
    expect(s).toMatch(/Calculator/);
  });

  it("explore add shows callers or calls", () => {
    const s = exploreSymbol("add", root);
    expect(s).toMatch(/add/);
    expect(s).toMatch(/trust:/);
  });

  it("callers of add is non-empty", () => {
    const s = listCallers("add", root);
    expect(s).toMatch(/callers of/);
    expect(s.toLowerCase()).toMatch(/double|sum|calculator|usealias/);
  });

  it("impact up of math.ts lists upstream", () => {
    const s = impactAnalysis("src/math.ts", root, "up");
    expect(s).toMatch(/direction=up/);
    expect(s).toMatch(/upstream/);
  });

  it("impact down of run lists callees", () => {
    const s = impactAnalysis("run", root, "down");
    expect(s).toMatch(/direction=down/);
    expect(s).toMatch(/downstream/);
  });

  it("incremental reindex does not rewrite last_full_index_at", async () => {
    const before = (() => {
      const db = GraphDb.open(root);
      try {
        return db.getMeta("last_full_index_at");
      } finally {
        db.close();
      }
    })();
    expect(before).toBeTruthy();
    await new Promise((r) => setTimeout(r, 20));
    await indexRepo(root, { full: false });
    const db = GraphDb.open(root);
    try {
      expect(db.getMeta("last_full_index_at")).toBe(before);
      expect(db.getMeta("last_index_at")).toBeTruthy();
    } finally {
      db.close();
    }
  });
});
