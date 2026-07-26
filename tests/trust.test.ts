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
import { formatTrustLine } from "../src/query/format.js";
import { exploreSymbol } from "../src/query/explore.js";

const FILE_COUNT = 120;

/** The tool payload is the fenced block, not the first brace in the body. */
function fencedJson(out: string): string {
  const m = out.match(/```json\n([\s\S]*?)\n```/);
  if (!m?.[1]) throw new Error("no fenced json payload in output");
  return m[1];
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
  it("detects an edit to any file, regardless of position", async () => {
    const root = makeRepo();
    try {
      await indexRepo(root, { full: true });

      let db = GraphDb.open(root);
      try {
        expect(db.listFiles().length).toBe(FILE_COUNT);
        expect(computeTrust(db).trust).not.toBe("stale");
      } finally {
        db.close();
      }

      // index 2 was skipped by the old 80-file stride sample
      writeFileSync(
        join(root, `src/f002.ts`),
        `export function fn002(): number { return 999; }\n`,
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

  it("verifies every file under the budget, so fresh is exhaustive", async () => {
    const root = makeRepo();
    try {
      await indexRepo(root, { full: true });
      const db = GraphDb.open(root);
      try {
        const t = computeTrust(db);
        // 120 files is well inside the hash budget — no sampling, no hedge
        expect(t.trust).toBe("fresh");
        expect(t.coverage).toBe("exhaustive");
        expect(t.files_hashed).toBe(FILE_COUNT);
        expect(t.notes.join("\n")).not.toMatch(/hash-checked/);
      } finally {
        db.close();
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("reports coverage=sampled past the budget, still fresh not stale", async () => {
    const root = makeRepo();
    try {
      await indexRepo(root, { full: true });
      const db = GraphDb.open(root);
      try {
        // force sampling: budget below file count, nothing actually edited
        const t = computeTrust(db, { hashBudget: 10 });
        expect(t.coverage).toBe("sampled");
        expect(t.files_hashed).toBe(10);
        // coverage must not masquerade as staleness
        expect(t.trust).toBe("fresh");
        expect(t.notes.join("\n")).toMatch(/hash-checked 10\/120/);
      } finally {
        db.close();
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("still detects a real edit when coverage is sampled", async () => {
    const root = makeRepo();
    try {
      await indexRepo(root, { full: true });
      // mtime+size gate puts the edited file in the suspect set, which is
      // hashed before any budget is spent on quiet files
      writeFileSync(
        join(root, "src/f077.ts"),
        `export function fn077(): number { return 424242; }\n`,
      );
      const db = GraphDb.open(root);
      try {
        const t = computeTrust(db, { hashBudget: 3 });
        expect(t.trust).toBe("stale");
        expect(t.coverage).toBe("sampled");
      } finally {
        db.close();
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("stops hashing once drift is proven", async () => {
    const root = makeRepo();
    try {
      await indexRepo(root, { full: true });
      writeFileSync(
        join(root, "src/f003.ts"),
        `export function fn003(): number { return 5150; }\n`,
      );
      const db = GraphDb.open(root);
      try {
        const t = computeTrust(db);
        expect(t.trust).toBe("stale");
        // suspects are hashed first; the answer is known after one read, so the
        // remaining 119 quiet files must not be read on every query
        expect(t.files_hashed).toBeLessThan(FILE_COUNT);
        expect(t.notes.join("\n")).toMatch(/stopped early/);
      } finally {
        db.close();
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("qualifies a fresh banner in the first token, for readers who stop there", async () => {
    const root = makeRepo();
    try {
      await indexRepo(root, { full: true });
      const db = GraphDb.open(root);
      try {
        const sampled = computeTrust(db, { hashBudget: 10 });
        expect(sampled.trust).toBe("fresh");
        // the skim-stop token must carry the caveat itself
        const line = formatTrustLine(sampled);
        expect(line.split("|")[0]).toMatch(/fresh but unverified/);
        expect(line).toMatch(/absence ≠ proof/);

        // a genuinely clean graph stays unqualified — no cry-wolf
        const clean = computeTrust(db);
        expect(clean.coverage).toBe("exhaustive");
        expect(clean.graph).toBe("ok");
        expect(formatTrustLine(clean).split("|")[0]!.trim()).toBe(
          "trust: fresh",
        );
      } finally {
        db.close();
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("emits JSON warnings so a one-field parser sees the caveat", async () => {
    const root = makeRepo();
    try {
      await indexRepo(root, { full: true });
      // degraded graph + exhaustive coverage: JSON must still carry a warning
      const withDeps = mkdtempSync(join(tmpdir(), "cs-warn-"));
      try {
        mkdirSync(join(withDeps, "src"), { recursive: true });
        writeFileSync(
          join(withDeps, "src/app.ts"),
          [
            `import express from "express";`,
            `import { readFileSync } from "node:fs";`,
            `export function go(): string { return express() + readFileSync("x"); }`,
          ].join("\n"),
        );
        await indexRepo(withDeps, { full: true });
        const out = exploreSymbol("go", withDeps);
        const json = JSON.parse(fencedJson(out));
        expect(json.trust).toBe("fresh");
        expect(json.graph).toBe("degraded");
        expect(Array.isArray(json.warnings)).toBe(true);
        expect(json.warnings.join(" ")).toMatch(/degraded/);
      } finally {
        rmSync(withDeps, { recursive: true, force: true });
      }

      // a clean graph must not carry an empty/noisy warnings key
      const clean = mkdtempSync(join(tmpdir(), "cs-warn-clean-"));
      try {
        mkdirSync(join(clean, "src"), { recursive: true });
        writeFileSync(
          join(clean, "src/a.ts"),
          `export function a(): number { return 1; }\nexport function b(): number { return a(); }\n`,
        );
        await indexRepo(clean, { full: true });
        const out = exploreSymbol("a", clean);
        const json = JSON.parse(fencedJson(out));
        expect(json.trust).toBe("fresh");
        expect(json.warnings).toBeUndefined();
      } finally {
        rmSync(clean, { recursive: true, force: true });
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("keeps graph quality independent of freshness", async () => {
    // a degraded graph (unresolved external imports) is still perfectly fresh
    const root = mkdtempSync(join(tmpdir(), "cs-trust-dep-"));
    try {
      mkdirSync(join(root, "src"), { recursive: true });
      writeFileSync(
        join(root, "src/app.ts"),
        [
          `import { readFileSync } from "node:fs";`,
          `import { join } from "node:path";`,
          `import express from "express";`,
          `export function go(): string {`,
          `  return join(String(readFileSync("x")), express());`,
          `}`,
        ].join("\n"),
      );
      await indexRepo(root, { full: true });
      const db = GraphDb.open(root);
      try {
        const t = computeTrust(db);
        expect(t.trust).toBe("fresh");
        expect(t.coverage).toBe("exhaustive");
        expect(t.graph).toBe("degraded");
        expect(t.unresolved_edge_count).toBeGreaterThan(0);
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
      const p = join(root, `src/f002.ts`);

      // rewrite with a different length, then restore the timestamps
      const st = statSync(p);
      writeFileSync(p, `export function fn002(): number { return 1234567; }\n`);
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
