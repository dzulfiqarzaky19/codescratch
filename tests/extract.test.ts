import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { extractTypeScriptFile } from "../src/extract/typescript.js";

const fixture = join(__dirname, "fixtures/mini-repo/src");

describe("extractTypeScriptFile", () => {
  it("extracts functions and calls from math.ts", async () => {
    const source = readFileSync(join(fixture, "math.ts"), "utf8");
    const ex = await extractTypeScriptFile("src/math.ts", source, "typescript");
    const names = ex.symbols.map((s) => s.name).sort();
    expect(names).toContain("add");
    expect(names).toContain("mul");
    expect(names).toContain("double");
    expect(ex.symbols.find((s) => s.name === "add")?.exported).toBe(true);
    const calls = ex.refs.filter((r) => r.kind === "calls");
    expect(calls.some((c) => c.rawName === "add")).toBe(true);
  });

  it("extracts class methods and imports from service.ts", async () => {
    const source = readFileSync(join(fixture, "service.ts"), "utf8");
    const ex = await extractTypeScriptFile(
      "src/service.ts",
      source,
      "typescript",
    );
    expect(ex.symbols.some((s) => s.kind === "class" && s.name === "Calculator")).toBe(
      true,
    );
    expect(ex.symbols.some((s) => s.kind === "method" && s.name === "sum")).toBe(
      true,
    );
    expect(ex.refs.some((r) => r.kind === "imports" && r.rawName.includes("math"))).toBe(
      true,
    );
    expect(ex.bindings.some((b) => b.localName === "add" && b.importedName === "add")).toBe(
      true,
    );
  });

  it("extracts import aliases and namespace", async () => {
    const source = readFileSync(join(fixture, "alias.ts"), "utf8");
    const ex = await extractTypeScriptFile("src/alias.ts", source, "typescript");
    expect(
      ex.bindings.some(
        (b) => b.localName === "sum" && b.importedName === "add",
      ),
    ).toBe(true);
    expect(
      ex.bindings.some(
        (b) => b.localName === "twice" && b.importedName === "double",
      ),
    ).toBe(true);
    expect(
      ex.bindings.some((b) => b.localName === "MathNs" && b.isNamespace),
    ).toBe(true);
  });

  it("only extracts module-level locals (no duplicate ids from callbacks)", async () => {
    // enclosing is null inside an unnamed arrow too, so callback locals used to
    // be emitted as top-level symbols and collide on nodeId
    const source = [
      `import { it as t } from "vitest";`,
      `const topOnly = 1;`,
      `t("one", () => { const db = 1; const files = 2; });`,
      `t("two", () => { const db = 3; const files = 4; });`,
      `function fn() { const db = 5; }`,
      `if (topOnly) { const db = 6; }`,
    ].join("\n");
    const ex = await extractTypeScriptFile("src/dup.ts", source, "typescript");
    const qns = ex.symbols.map((s) => s.qualifiedName);
    expect(new Set(qns).size).toBe(qns.length); // no duplicate ids
    expect(qns).toContain("topOnly");
    expect(qns.filter((q) => q === "db")).toHaveLength(0);
    expect(qns.filter((q) => q === "files")).toHaveLength(0);
  });

  it("does not extract methods from type positions", async () => {
    const source = [
      "export function take(o: { run(): number }) { return o.run(); }",
      "type Handler = { onX(): void };",
      "export interface I { keep(): number }",
      "export class C { held() { return 1; } }",
    ].join("\n");
    const ex = await extractTypeScriptFile("src/shapes.ts", source, "typescript");
    const methods = ex.symbols.filter((s) => s.kind === "method");
    // shape members are not symbols
    expect(methods.some((s) => s.name === "run")).toBe(false);
    expect(methods.some((s) => s.name === "onX")).toBe(false);
    expect(ex.symbols.some((s) => s.qualifiedName === "take.run")).toBe(false);
    // real class/interface members still are
    expect(methods.some((s) => s.qualifiedName === "I.keep")).toBe(true);
    expect(methods.some((s) => s.qualifiedName === "C.held")).toBe(true);
    expect(
      ex.refs.some((r) => r.kind === "has_method" && r.rawName === "run"),
    ).toBe(false);
  });

  it("extracts export default", async () => {
    const source = readFileSync(join(fixture, "defaults.ts"), "utf8");
    const ex = await extractTypeScriptFile(
      "src/defaults.ts",
      source,
      "typescript",
    );
    expect(ex.symbols.some((s) => s.name === "default" && s.exported)).toBe(
      true,
    );
    expect(ex.symbols.some((s) => s.name === "greet")).toBe(true);
  });
});
