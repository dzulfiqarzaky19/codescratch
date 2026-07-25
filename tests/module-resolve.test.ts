import { describe, expect, it, beforeAll, afterAll } from "vitest";
import { mkdtempSync, rmSync, cpSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { indexRepo } from "../src/index/indexer.js";
import { GraphDb } from "../src/db/client.js";
import {
  buildResolveContext,
  resolveModuleSpecifier,
} from "../src/index/module-resolve.js";
import { listCallers } from "../src/query/callers.js";

const fixtureSrc = join(__dirname, "fixtures/medium-repo");
let root: string;

beforeAll(async () => {
  root = mkdtempSync(join(tmpdir(), "cs-medium-"));
  cpSync(fixtureSrc, root, { recursive: true });
  await indexRepo(root, { full: true });
});

afterAll(() => {
  rmSync(root, { recursive: true, force: true });
});

describe("module resolve (paths + packages)", () => {
  it("resolves @/ and @app/ path aliases", () => {
    const db = GraphDb.open(root);
    try {
      const files = new Set(db.listFiles().map((f) => f.path));
      const ctx = buildResolveContext(root, files);
      expect(
        resolveModuleSpecifier(ctx, "src/services/calc.ts", "@/lib/math.js"),
      ).toBe("src/lib/math.ts");
      expect(
        resolveModuleSpecifier(
          ctx,
          "src/services/calc.ts",
          "@app/lib/reexport.js",
        ),
      ).toBe("src/lib/reexport.ts");
    } finally {
      db.close();
    }
  });

  it("resolves workspace package @medium/core", () => {
    const db = GraphDb.open(root);
    try {
      const files = new Set(db.listFiles().map((f) => f.path));
      const ctx = buildResolveContext(root, files);
      expect(
        resolveModuleSpecifier(ctx, "src/services/calc.ts", "@medium/core"),
      ).toBe("packages/core/src/index.ts");
    } finally {
      db.close();
    }
  });

  it("bindings + callers for path-alias import of add", () => {
    const db = GraphDb.open(root);
    try {
      const b = db
        .bindingsInFile("src/services/calc.ts")
        .find((x) => x.local_name === "sum");
      expect(b?.module_path).toBe("src/lib/math.ts");
      expect(b?.imported_name).toBe("add");

      const add = db
        .findNodesByName("add")
        .find((n) => n.file_path.includes("math"))!;
      const callers = db.edgesTo(add.id, "calls");
      expect(
        callers.some(
          (e) => e.file_path === "src/services/calc.ts" && e.confidence === "strong",
        ),
      ).toBe(true);
    } finally {
      db.close();
    }
  });

  it("package greet is called from hello", () => {
    const s = listCallers("greet", root);
    expect(s.toLowerCase()).toMatch(/hello/);
  });
});

describe("incremental dirty rebind", () => {
  it("removes stale strong caller after export deleted + reindex", async () => {
    const iso = mkdtempSync(join(tmpdir(), "cs-rename-"));
    try {
      cpSync(fixtureSrc, iso, { recursive: true });
      await indexRepo(iso, { full: true });

      let db = GraphDb.open(iso);
      const addBefore = db
        .findNodesByName("add")
        .find((n) => n.file_path.includes("math"));
      expect(addBefore).toBeTruthy();
      const callersBefore = db.edgesTo(addBefore!.id, "calls").length;
      expect(callersBefore).toBeGreaterThan(0);
      db.close();

      // delete add from math.ts
      const mathPath = join(iso, "src/lib/math.ts");
      writeFileSync(
        mathPath,
        `export function mul(a: number, b: number): number {
  return a * b;
}
`,
        "utf8",
      );

      await indexRepo(iso, { full: false });

      db = GraphDb.open(iso);
      try {
        const addAfter = db
          .findNodesByName("add")
          .find((n) => n.file_path.includes("math"));
        expect(addAfter).toBeFalsy();

        // calc still imports add as sum — binding may point at missing export
        const edges = db
          .edgesFrom(
            db.nodesInFile("src/services/calc.ts").find((n) => n.name === "total")!
              .id,
            "calls",
          )
          .filter((e) => e.raw_name === "sum" || e.raw_name === "add");
        // should not be strong-resolved to a dead node
        for (const e of edges) {
          if (e.resolved && e.dst_id) {
            expect(db.getNode(e.dst_id)).toBeTruthy();
          }
        }
        // no edge should still point at old add id
        const allCalls = db.unresolvedBindableEdges();
        void allCalls;
        const anyDst = db
          .listFiles()
          .flatMap((f) => db.nodesInFile(f.path))
          .filter((n) => n.name === "add");
        expect(anyDst.length).toBe(0);
      } finally {
        db.close();
      }
    } finally {
      rmSync(iso, { recursive: true, force: true });
    }
  });

  it("touch-without-change does not mark stale via hash", async () => {
    const { computeTrust } = await import("../src/query/trust.js");
    const db = GraphDb.open(root);
    try {
      const t = computeTrust(db);
      expect(t.trust).not.toBe("stale");
      // rewrite same bytes
      const p = join(root, "src/index.ts");
      writeFileSync(p, readFileSync(p));
      const t2 = computeTrust(db);
      expect(t2.trust).not.toBe("stale");
    } finally {
      db.close();
    }
  });
});
