import { describe, expect, it, beforeAll, afterAll } from "vitest";
import { mkdtempSync, rmSync, cpSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { indexRepo } from "../src/index/indexer.js";
import { GraphDb } from "../src/db/client.js";
import { resolveImportPath } from "../src/index/resolve.js";

const fixtureSrc = join(__dirname, "fixtures/mini-repo");
let root: string;

beforeAll(async () => {
  root = mkdtempSync(join(tmpdir(), "cs-resolve-"));
  cpSync(fixtureSrc, root, { recursive: true });
  await indexRepo(root, { full: true });
});

afterAll(() => {
  rmSync(root, { recursive: true, force: true });
});

describe("resolveImportPath", () => {
  it("maps relative .js specifier to .ts file", () => {
    const files = new Set([
      "src/math.ts",
      "src/service.ts",
      "src/index.ts",
      "src/alias.ts",
      "src/defaults.ts",
    ]);
    const hit = resolveImportPath(root, "src/service.ts", "./math.js", files);
    expect(hit).toBe("src/math.ts");
  });
});

describe("index + resolve", () => {
  it("resolves add callers across files", () => {
    const db = GraphDb.open(root);
    try {
      const add = db
        .findNodesByName("add")
        .find((n) => n.file_path.includes("math"));
      expect(add).toBeTruthy();
      const callers = db.edgesTo(add!.id, "calls");
      expect(callers.length).toBeGreaterThan(0);
      const fileId = db.fileModuleId("src/service.ts");
      expect(fileId).toBeTruthy();
      const imp = db.edgesFrom(fileId!, "imports");
      expect(imp.some((e) => e.resolved)).toBe(true);
    } finally {
      db.close();
    }
  });

  it("resolves aliased import calls via bindings", () => {
    const db = GraphDb.open(root);
    try {
      const bindings = db.bindingsInFile("src/alias.ts");
      expect(bindings.some((b) => b.local_name === "sum" && b.module_path === "src/math.ts")).toBe(
        true,
      );
      const add = db
        .findNodesByName("add")
        .find((n) => n.file_path.includes("math"))!;
      const callers = db.edgesTo(add.id, "calls");
      const fromAlias = callers.filter((e) => e.file_path === "src/alias.ts");
      expect(fromAlias.length).toBeGreaterThan(0);
      expect(fromAlias.every((e) => e.confidence === "strong")).toBe(true);
    } finally {
      db.close();
    }
  });

  it("resolves namespace member MathNs.mul", () => {
    const db = GraphDb.open(root);
    try {
      const mul = db
        .findNodesByName("mul")
        .find((n) => n.file_path.includes("math"))!;
      const callers = db.edgesTo(mul.id, "calls");
      expect(callers.some((e) => e.file_path === "src/alias.ts")).toBe(true);
    } finally {
      db.close();
    }
  });
});
