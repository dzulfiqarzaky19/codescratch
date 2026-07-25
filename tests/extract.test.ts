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
