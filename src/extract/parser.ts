import { readFileSync, existsSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Language, Parser, type Tree } from "web-tree-sitter";

const require = createRequire(import.meta.url);

let initPromise: Promise<void> | null = null;
const languages = new Map<string, Language>();

async function ensureInit(): Promise<void> {
  if (!initPromise) {
    initPromise = (async () => {
      const wasm = findWebTreeSitterWasm();
      if (wasm) {
        await Parser.init({
          locateFile: (scriptName: string) => {
            if (scriptName.endsWith(".wasm")) return wasm;
            return scriptName;
          },
        });
      } else {
        await Parser.init();
      }
    })();
  }
  await initPromise;
}

function findWebTreeSitterWasm(): string | null {
  try {
    const pkg = dirname(require.resolve("web-tree-sitter/package.json"));
    const candidates = [
      join(pkg, "tree-sitter.wasm"),
      join(pkg, "debug", "tree-sitter.wasm"),
    ];
    for (const c of candidates) {
      if (existsSync(c)) return c;
    }
  } catch {
    /* ignore */
  }
  return null;
}

function findGrammarWasm(name: "typescript" | "tsx" | "javascript"): string {
  const here = dirname(fileURLToPath(import.meta.url));
  const root = join(here, "..", "..");
  const file =
    name === "typescript"
      ? "tree-sitter-typescript.wasm"
      : name === "tsx"
        ? "tree-sitter-tsx.wasm"
        : "tree-sitter-javascript.wasm";

  const candidates = [
    join(root, "wasm", file),
    join(root, "dist", "wasm", file),
    join(root, "node_modules", "tree-sitter-wasms", "out", file),
  ];
  for (const c of candidates) {
    if (existsSync(c)) return c;
  }
  throw new Error(
    `Missing grammar wasm: ${file}. Run npm install (postinstall copies wasm/).`,
  );
}

export type GrammarId = "typescript" | "tsx" | "javascript";

export function grammarForLanguage(language: string): GrammarId {
  if (language === "tsx" || language === "jsx") return "tsx";
  if (language === "javascript") return "javascript";
  return "typescript";
}

export async function getParser(language: string): Promise<Parser> {
  await ensureInit();
  const gid = grammarForLanguage(language);
  let lang = languages.get(gid);
  if (!lang) {
    const wasmPath = findGrammarWasm(
      gid === "javascript" ? "javascript" : gid === "tsx" ? "tsx" : "typescript",
    );
    lang = await Language.load(readFileSync(wasmPath));
    languages.set(gid, lang);
  }
  const parser = new Parser();
  parser.setLanguage(lang);
  return parser;
}

export async function parseSource(
  source: string,
  language: string,
): Promise<Tree> {
  const parser = await getParser(language);
  try {
    const tree = parser.parse(source);
    if (!tree) throw new Error("parse returned null");
    return tree;
  } finally {
    parser.delete();
  }
}
