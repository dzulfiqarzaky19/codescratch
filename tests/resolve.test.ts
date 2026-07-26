import { describe, expect, it, beforeAll, afterAll } from "vitest";
import { mkdtempSync, rmSync, cpSync, mkdirSync, writeFileSync } from "node:fs";
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

  it("does not strong-bind a call on an unknown receiver", async () => {
    const iso = mkdtempSync(join(tmpdir(), "cs-recv-"));
    try {
      mkdirSync(join(iso, "src"), { recursive: true });
      writeFileSync(
        join(iso, "src/a.ts"),
        "export class Foo { bar(): number { return 1; } }\n" +
          "export function f(x: any): number { return x.bar(); }\n",
      );
      await indexRepo(iso, { full: true });
      const db = GraphDb.open(iso);
      try {
        const bar = db
          .findNodesByName("bar")
          .find((n) => n.qualified_name === "Foo.bar")!;
        expect(bar).toBeTruthy();
        const edges = db
          .edgesTo(bar.id, "calls")
          .filter((e) => e.raw_name === "x.bar");
        expect(edges.length).toBe(1);
        expect(edges[0]!.confidence).toBe("weak");
        expect(edges[0]!.reason).toBe("receiver-unknown");
      } finally {
        db.close();
      }
    } finally {
      rmSync(iso, { recursive: true, force: true });
    }
  });

  it("strong-binds this.x to the enclosing class method", async () => {
    const iso = mkdtempSync(join(tmpdir(), "cs-this-"));
    try {
      mkdirSync(join(iso, "src"), { recursive: true });
      writeFileSync(
        join(iso, "src/b.ts"),
        "export class Baz {\n" +
          "  a(): number { return this.b(); }\n" +
          "  b(): number { return 2; }\n" +
          "}\n",
      );
      await indexRepo(iso, { full: true });
      const db = GraphDb.open(iso);
      try {
        const b = db
          .findNodesByName("b")
          .find((n) => n.qualified_name === "Baz.b")!;
        expect(b).toBeTruthy();
        const edges = db
          .edgesTo(b.id, "calls")
          .filter((e) => e.raw_name === "this.b");
        expect(edges.length).toBe(1);
        expect(edges[0]!.confidence).toBe("strong");
        expect(edges[0]!.reason).toBe("this-member");
      } finally {
        db.close();
      }
    } finally {
      rmSync(iso, { recursive: true, force: true });
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
