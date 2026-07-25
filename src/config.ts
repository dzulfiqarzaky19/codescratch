import { existsSync } from "node:fs";
import { isAbsolute, join, resolve } from "node:path";

const DIR_NAME = ".codescratch";
const DB_NAME = "graph.db";

export function resolveRoot(explicit?: string): string {
  const fromEnv = process.env.CODESCRATCH_ROOT;
  const raw = explicit ?? fromEnv ?? process.cwd();
  return resolve(raw);
}

export function graphDir(root: string): string {
  return join(root, DIR_NAME);
}

export function graphDbPath(root: string): string {
  return join(graphDir(root), DB_NAME);
}

export function graphExists(root: string): boolean {
  return existsSync(graphDbPath(root));
}

export function resolveMaybeRelative(base: string, p: string): string {
  return isAbsolute(p) ? p : resolve(base, p);
}

export const SUPPORTED_EXTENSIONS = new Set([
  ".ts",
  ".tsx",
  ".js",
  ".jsx",
  ".mts",
  ".cts",
  ".mjs",
  ".cjs",
]);

export function languageForPath(filePath: string): string | null {
  const lower = filePath.toLowerCase();
  if (lower.endsWith(".tsx")) return "tsx";
  if (lower.endsWith(".ts") || lower.endsWith(".mts") || lower.endsWith(".cts"))
    return "typescript";
  if (lower.endsWith(".jsx")) return "jsx";
  if (
    lower.endsWith(".js") ||
    lower.endsWith(".mjs") ||
    lower.endsWith(".cjs")
  )
    return "javascript";
  return null;
}

export const IMPACT_MAX_DEPTH = 4;
export const IMPACT_MAX_NODES = 200;
export const SEARCH_DEFAULT_LIMIT = 25;
