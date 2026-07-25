import { describe, expect, it, beforeAll, afterAll } from "vitest";
import {
  mkdtempSync,
  rmSync,
  cpSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { indexRepo } from "../src/index/indexer.js";
import { GraphDb, nodeId } from "../src/db/client.js";
import {
  buildResolveContext,
  resolveModuleSpecifier,
} from "../src/index/module-resolve.js";
import { listCallers } from "../src/query/callers.js";
import { extractTypeScriptFile } from "../src/extract/typescript.js";

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
          (e) =>
            e.file_path === "src/services/calc.ts" && e.confidence === "strong",
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

describe("export * star reexport", () => {
  it("extracts export * binding", async () => {
    const src = readFileSync(join(fixtureSrc, "src/lib/barrel.ts"), "utf8");
    const ex = await extractTypeScriptFile(
      "src/lib/barrel.ts",
      src,
      "typescript",
    );
    expect(
      ex.bindings.some(
        (b) => b.isStarReexport && b.moduleSpecifier.includes("math"),
      ),
    ).toBe(true);
  });

  it("resolves plus → add through export * barrel", () => {
    const db = GraphDb.open(root);
    try {
      const stars = db.starReexportsFrom("src/lib/barrel.ts");
      expect(stars.some((s) => s.module_path === "src/lib/math.ts")).toBe(true);

      const b = db
        .bindingsInFile("src/services/via-barrel.ts")
        .find((x) => x.local_name === "plus");
      expect(b?.module_path).toBe("src/lib/barrel.ts");
      expect(b?.imported_name).toBe("add");

      const add = db
        .findNodesByName("add")
        .find((n) => n.file_path.includes("math"))!;
      const callers = db.edgesTo(add.id, "calls");
      expect(
        callers.some(
          (e) =>
            e.file_path === "src/services/via-barrel.ts" &&
            e.confidence === "strong",
        ),
      ).toBe(true);
    } finally {
      db.close();
    }
  });
});

describe("incremental dirty rebind", () => {
  it("sum call is not strong-resolved after add export deleted", async () => {
    const iso = mkdtempSync(join(tmpdir(), "cs-rename-"));
    try {
      cpSync(fixtureSrc, iso, { recursive: true });
      await indexRepo(iso, { full: true });

      let db = GraphDb.open(iso);
      const addBefore = db
        .findNodesByName("add")
        .find((n) => n.file_path.includes("math"));
      expect(addBefore).toBeTruthy();
      const oldAddId = addBefore!.id;
      const totalBefore = db
        .nodesInFile("src/services/calc.ts")
        .find((n) => n.name === "total")!;
      const sumEdgesBefore = db
        .edgesFrom(totalBefore.id, "calls")
        .filter((e) => e.raw_name === "sum" || e.raw_name === "add");
      expect(
        sumEdgesBefore.some(
          (e) => e.resolved && e.confidence === "strong" && e.dst_id === oldAddId,
        ),
      ).toBe(true);
      db.close();

      writeFileSync(
        join(iso, "src/lib/math.ts"),
        `export function mul(a: number, b: number): number {
  return a * b;
}
`,
        "utf8",
      );

      await indexRepo(iso, { full: false });

      db = GraphDb.open(iso);
      try {
        expect(
          db.findNodesByName("add").find((n) => n.file_path.includes("math")),
        ).toBeFalsy();
        expect(db.getNode(oldAddId)).toBeNull();

        const total = db
          .nodesInFile("src/services/calc.ts")
          .find((n) => n.name === "total")!;
        const sumEdges = db
          .edgesFrom(total.id, "calls")
          .filter((e) => e.raw_name === "sum" || e.raw_name === "add");

        // Core claim: must NOT stay strong-resolved to a live target named add
        for (const e of sumEdges) {
          if (e.resolved && e.confidence === "strong" && e.dst_id) {
            const dst = db.getNode(e.dst_id);
            expect(dst).toBeTruthy();
            expect(dst!.name).not.toBe("add");
            expect(e.dst_id).not.toBe(oldAddId);
          }
        }
        // Prefer: unresolved or non-strong after delete
        const stillStrongToMissing = sumEdges.filter(
          (e) =>
            e.resolved &&
            e.confidence === "strong" &&
            e.dst_id === oldAddId,
        );
        expect(stillStrongToMissing.length).toBe(0);

        const strongSum = sumEdges.filter(
          (e) => e.resolved && e.confidence === "strong",
        );
        // After delete, binding sum←add cannot resolve — edge should be unresolved or weak-only
        expect(strongSum.length).toBe(0);
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
      const p = join(root, "src/index.ts");
      writeFileSync(p, readFileSync(p));
      const t2 = computeTrust(db);
      expect(t2.trust).not.toBe("stale");
    } finally {
      db.close();
    }
  });
});
