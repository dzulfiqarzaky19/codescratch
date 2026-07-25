import { cpSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

const distDb = join(root, "dist", "db");
mkdirSync(distDb, { recursive: true });
cpSync(join(root, "src", "db", "schema.sql"), join(distDb, "schema.sql"));

const wasmSrc = join(root, "wasm");
const wasmDest = join(root, "dist", "wasm");
if (existsSync(wasmSrc)) {
  mkdirSync(wasmDest, { recursive: true });
  for (const f of [
    "tree-sitter-typescript.wasm",
    "tree-sitter-tsx.wasm",
    "tree-sitter-javascript.wasm",
  ]) {
    const from = join(wasmSrc, f);
    if (existsSync(from)) cpSync(from, join(wasmDest, f));
  }
}
