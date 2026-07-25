import {
  existsSync,
  readFileSync,
  readdirSync,
  statSync,
} from "node:fs";
import { dirname, join, normalize, relative } from "node:path";
import { SUPPORTED_EXTENSIONS } from "../config.js";
import { extnamePosix, toPosix } from "../util/paths.js";

export interface PathAlias {
  /** e.g. `@/` or `@app/` */
  prefix: string;
  /** repo-relative dir */
  target: string;
  star: boolean;
}

export interface ResolveContext {
  root: string;
  fileSet: Set<string>;
  aliases: PathAlias[];
  baseUrl: string | null;
  packages: Map<string, string>;
}

interface TsconfigShape {
  compilerOptions?: {
    baseUrl?: string;
    paths?: Record<string, string[]>;
  };
  extends?: string;
}

interface PkgJson {
  name?: string;
  main?: string;
  module?: string;
  types?: string;
  exports?: unknown;
  workspaces?: string[] | { packages?: string[] };
}

export function buildResolveContext(
  root: string,
  fileSet: Set<string>,
): ResolveContext {
  const { aliases, baseUrl } = loadTsconfigPaths(root);
  const packages = discoverPackages(root);
  return { root, fileSet, aliases, baseUrl, packages };
}

export function resolveModuleSpecifier(
  ctx: ResolveContext,
  fromFile: string,
  specifier: string,
): string | null {
  if (specifier.startsWith(".") || specifier.startsWith("/")) {
    return resolveRelative(ctx, fromFile, specifier);
  }

  const viaAlias = resolveAlias(ctx, specifier);
  if (viaAlias) return viaAlias;

  if (ctx.baseUrl) {
    const viaBase = hitFile(ctx, joinPosix(ctx.baseUrl, specifier));
    if (viaBase) return viaBase;
  }

  return resolvePackage(ctx, specifier);
}

function resolveRelative(
  ctx: ResolveContext,
  fromFile: string,
  specifier: string,
): string | null {
  const fromAbs = join(ctx.root, fromFile);
  const targetAbs = normalize(join(dirname(fromAbs), specifier));
  const rel = toPosix(relative(ctx.root, targetAbs));
  if (rel.startsWith("..")) return null;
  return hitFile(ctx, rel);
}

function resolveAlias(ctx: ResolveContext, specifier: string): string | null {
  for (const a of ctx.aliases) {
    if (a.star) {
      if (!specifier.startsWith(a.prefix)) continue;
      const rest = specifier.slice(a.prefix.length);
      const candidate = a.target ? joinPosix(a.target, rest) : rest;
      const hit = hitFile(ctx, candidate);
      if (hit) return hit;
    } else if (specifier === a.prefix || specifier.startsWith(`${a.prefix}/`)) {
      const rest =
        specifier === a.prefix ? "" : specifier.slice(a.prefix.length + 1);
      const candidate = rest ? joinPosix(a.target, rest) : a.target;
      const hit = hitFile(ctx, candidate);
      if (hit) return hit;
    }
  }
  return null;
}

function resolvePackage(
  ctx: ResolveContext,
  specifier: string,
): string | null {
  let pkgName: string;
  let subpath: string;
  if (specifier.startsWith("@")) {
    const parts = specifier.split("/");
    if (parts.length < 2) return null;
    pkgName = `${parts[0]}/${parts[1]}`;
    subpath = parts.slice(2).join("/");
  } else {
    const i = specifier.indexOf("/");
    pkgName = i === -1 ? specifier : specifier.slice(0, i);
    subpath = i === -1 ? "" : specifier.slice(i + 1);
  }

  const pkgRoot = ctx.packages.get(pkgName);
  if (!pkgRoot) return null;

  if (!subpath) return resolvePackageEntry(ctx, pkgRoot);
  return hitFile(ctx, joinPosix(pkgRoot, subpath));
}

function resolvePackageEntry(
  ctx: ResolveContext,
  pkgRoot: string,
): string | null {
  const pkgJsonPath = join(ctx.root, pkgRoot, "package.json");
  if (existsSync(pkgJsonPath)) {
    try {
      const pkg = JSON.parse(readFileSync(pkgJsonPath, "utf8")) as PkgJson;
      const candidates: string[] = [];
      const exp = pkg.exports;
      if (typeof exp === "string") candidates.push(exp);
      else if (exp && typeof exp === "object" && !Array.isArray(exp)) {
        const rootExp = (exp as Record<string, unknown>)["."];
        if (typeof rootExp === "string") candidates.push(rootExp);
        else if (rootExp && typeof rootExp === "object") {
          const o = rootExp as Record<string, unknown>;
          for (const k of ["import", "require", "default", "types"]) {
            const v = o[k];
            if (typeof v === "string") candidates.push(v);
          }
        }
      }
      if (pkg.module) candidates.push(pkg.module);
      if (pkg.main) candidates.push(pkg.main);
      if (pkg.types) candidates.push(pkg.types);
      for (const c of candidates) {
        const rel = c.replace(/^\.\//, "");
        const hit = hitFile(ctx, joinPosix(pkgRoot, rel));
        if (hit) return hit;
      }
    } catch {
      /* ignore */
    }
  }
  for (const c of ["src/index", "index", "dist/index", "lib/index"]) {
    const hit = hitFile(ctx, joinPosix(pkgRoot, c));
    if (hit) return hit;
  }
  return null;
}

function hitFile(ctx: ResolveContext, relRaw: string): string | null {
  const rel = toPosix(relRaw).replace(/^\.\//, "");
  for (const c of expandTsCandidates(rel)) {
    if (ctx.fileSet.has(c)) return c;
  }
  for (const c of expandTsCandidates(rel)) {
    const abs = join(ctx.root, c);
    if (existsSync(abs)) {
      const posix = toPosix(relative(ctx.root, abs));
      if (ctx.fileSet.has(posix)) return posix;
    }
  }
  return null;
}

export function expandTsCandidates(rel: string): string[] {
  const posix = toPosix(rel);
  const out: string[] = [];
  const exts = [...SUPPORTED_EXTENSIONS];
  const ext = extnamePosix(posix);

  if (ext === ".js" || ext === ".jsx" || ext === ".mjs" || ext === ".cjs") {
    out.push(posix);
    const stem = posix.slice(0, -ext.length);
    const remap =
      ext === ".jsx" || ext === ".js"
        ? [".ts", ".tsx", ".js", ".jsx", ".mts", ".cts"]
        : [".ts", ".mts", ".cts", ".js", ".mjs", ".cjs"];
    for (const e of remap) out.push(stem + e);
    for (const e of exts) out.push(`${stem}/index${e}`);
  } else if (SUPPORTED_EXTENSIONS.has(ext)) {
    out.push(posix);
  } else {
    for (const e of exts) out.push(posix + e);
    for (const e of exts) out.push(`${posix}/index${e}`);
  }
  return out;
}

function loadTsconfigPaths(root: string): {
  aliases: PathAlias[];
  baseUrl: string | null;
} {
  const aliases: PathAlias[] = [];
  let baseUrl: string | null = null;
  const cfg = readTsconfig(root, "tsconfig.json");
  if (!cfg?.compilerOptions) return { aliases, baseUrl };

  if (cfg.compilerOptions.baseUrl) {
    baseUrl = toPosix(cfg.compilerOptions.baseUrl).replace(/\/$/, "");
  }

  const paths = cfg.compilerOptions.paths ?? {};
  for (const [pattern, targets] of Object.entries(paths)) {
    const target0 = targets[0];
    if (!target0) continue;
    const star = pattern.endsWith("/*");
    const prefix = star ? pattern.slice(0, -1) : pattern;
    let target = target0;
    if (star && target.endsWith("/*")) target = target.slice(0, -2);
    else if (star && target.endsWith("*")) target = target.slice(0, -1);
    target = toPosix(target).replace(/^\.\//, "").replace(/\/$/, "");
    if (baseUrl && !isAbsLike(target)) {
      target = joinPosix(baseUrl, target).replace(/\/$/, "");
    }
    aliases.push({ prefix, target, star });
  }

  aliases.sort((a, b) => b.prefix.length - a.prefix.length);
  return { aliases, baseUrl };
}

function readTsconfig(root: string, rel: string): TsconfigShape | null {
  const abs = join(root, rel);
  if (!existsSync(abs)) return null;
  try {
    const raw = readFileSync(abs, "utf8");
    const json = raw
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .replace(/^\s*\/\/.*$/gm, "");
    const cfg = JSON.parse(json) as TsconfigShape;
    if (cfg.extends) {
      const parentRel = toPosix(join(dirname(rel), cfg.extends));
      const parent = readTsconfig(root, parentRel);
      if (parent) {
        return {
          compilerOptions: {
            ...parent.compilerOptions,
            ...cfg.compilerOptions,
            paths: {
              ...parent.compilerOptions?.paths,
              ...cfg.compilerOptions?.paths,
            },
          },
        };
      }
    }
    return cfg;
  } catch {
    return null;
  }
}

function discoverPackages(root: string): Map<string, string> {
  const map = new Map<string, string>();
  const rootPkgPath = join(root, "package.json");
  if (!existsSync(rootPkgPath)) return map;

  let rootPkg: PkgJson;
  try {
    rootPkg = JSON.parse(readFileSync(rootPkgPath, "utf8")) as PkgJson;
  } catch {
    return map;
  }

  if (rootPkg.name) map.set(rootPkg.name, ".");

  const ws = Array.isArray(rootPkg.workspaces)
    ? rootPkg.workspaces
    : (rootPkg.workspaces?.packages ?? []);
  const globs = ws.length > 0 ? ws : ["packages/*", "apps/*", "libs/*"];

  for (const g of globs) {
    const isStar = g.endsWith("/*");
    const pattern = isStar ? g.slice(0, -2) : g;
    const dir = join(root, pattern);
    if (!existsSync(dir)) continue;
    if (isStar) {
      try {
        for (const name of readdirSync(dir)) {
          const child = join(dir, name);
          if (!statSync(child).isDirectory()) continue;
          registerPkg(map, root, toPosix(join(pattern, name)));
        }
      } catch {
        /* ignore */
      }
    } else {
      registerPkg(map, root, toPosix(pattern));
    }
  }
  return map;
}

function registerPkg(
  map: Map<string, string>,
  root: string,
  relDir: string,
): void {
  const pkgPath = join(root, relDir, "package.json");
  if (!existsSync(pkgPath)) return;
  try {
    const pkg = JSON.parse(readFileSync(pkgPath, "utf8")) as PkgJson;
    if (pkg.name) map.set(pkg.name, toPosix(relDir));
  } catch {
    /* ignore */
  }
}

function joinPosix(...parts: string[]): string {
  return toPosix(parts.filter(Boolean).join("/")).replace(/\/+/g, "/");
}

function isAbsLike(p: string): boolean {
  return /^[a-zA-Z]:/.test(p) || p.startsWith("/");
}
