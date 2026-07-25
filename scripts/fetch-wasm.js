import { cpSync, existsSync, mkdirSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const dest = join(root, "wasm");
mkdirSync(dest, { recursive: true });

const require = createRequire(import.meta.url);

const needed = [
  "tree-sitter-typescript.wasm",
  "tree-sitter-tsx.wasm",
  "tree-sitter-javascript.wasm",
];

function findWasmsDir() {
  try {
    const pkg = dirname(require.resolve("tree-sitter-wasms/package.json"));
    const out = join(pkg, "out");
    if (existsSync(out)) return out;
  } catch {
    /* not installed yet */
  }
  return null;
}

const src = findWasmsDir();
if (!src) {
  console.warn(
    "[codescratch] tree-sitter-wasms not found; skip wasm copy (run after npm install)",
  );
  process.exit(0);
}

for (const file of needed) {
  const from = join(src, file);
  if (!existsSync(from)) {
    // list available for debug
    const available = readdirSync(src).filter((f) => f.includes("typescript") || f.includes("tsx") || f.includes("javascript"));
    console.warn(`[codescratch] missing ${file}; available: ${available.join(", ")}`);
    continue;
  }
  cpSync(from, join(dest, file));
}

console.log(`[codescratch] wasm grammars → ${dest}`);
