import type { GraphDb } from "../db/client.js";
import { nodeId } from "../db/client.js";
import type { EdgeConfidence, GraphNode } from "../models.js";
import {
  buildResolveContext,
  resolveModuleSpecifier,
  type ResolveContext,
} from "./module-resolve.js";

export interface ResolveOptions {
  /**
   * When true (default if dirtyFiles set): clear resolved call-like edges in
   * dirty files + importers, then full rebind. Correct interim vs fancy slicing.
   */
  fullRebind?: boolean;
  dirtyFiles?: string[];
  /**
   * Still-present files that must rebind even if not in dirtyFiles
   * (e.g. importers of deleted modules — deleted paths are already gone).
   */
  extraRebindFiles?: string[];
}

/**
 * Bind edges. Prefer import bindings + same-file.
 * Unique-global-name → confidence=weak.
 *
 * On any dirty set: full rebind of affected files (clear resolved → re-resolve)
 * so rename/delete cannot leave stale strong edges.
 */
export function resolveEdges(
  db: GraphDb,
  root: string,
  opts?: ResolveOptions,
): number {
  const files = db.listFiles().map((f) => f.path);
  const fileSet = new Set(files);
  const ctx = buildResolveContext(root, fileSet);

  let scope: string[] | undefined;
  const hasDirty = opts?.dirtyFiles && opts.dirtyFiles.length > 0;
  const hasExtra =
    opts?.extraRebindFiles && opts.extraRebindFiles.length > 0;
  if (hasDirty || hasExtra) {
    const dirty = (opts?.dirtyFiles ?? []).filter((p) => fileSet.has(p));
    const deps = dirty.length > 0 ? db.filesImportingModules(dirty) : [];
    const extra = (opts?.extraRebindFiles ?? []).filter((p) =>
      fileSet.has(p),
    );
    scope = [...new Set([...dirty, ...deps, ...extra])];
  }

  // Correctness: when anything dirty, unresolve call-like edges in scope so
  // targets are recomputed (full resolve-on-dirty interim).
  if (scope && scope.length > 0 && opts?.fullRebind !== false) {
    db.clearResolvedCallLikeInFiles(scope);
    // also re-open bindings module_path in scope for path changes
    db.clearBindingModulePathsInFiles(scope);
  }

  let resolved = 0;

  // Bindings → module_path
  const openBindings = db.unresolvedBindings(scope);
  for (const b of openBindings) {
    const target = resolveModuleSpecifier(ctx, b.file_path, b.module_specifier);
    if (target) {
      db.updateBindingModulePath(b.id, target);
      resolved++;
    }
  }

  // After clearing, re-fetch edges that need binding (unresolved in scope or all)
  const edges = db.unresolvedBindableEdges(scope);
  for (const edge of edges) {
    if (edge.kind === "imports") {
      const targetFile = resolveModuleSpecifier(
        ctx,
        edge.file_path,
        edge.raw_name,
      );
      if (!targetFile) continue;
      const fileNode = db.getNode(nodeId(targetFile, targetFile));
      if (!fileNode) continue;
      db.updateEdgeResolution(edge.id, fileNode.id, "strong");
      resolved++;
      continue;
    }

    if (
      edge.kind === "calls" ||
      edge.kind === "extends" ||
      edge.kind === "implements"
    ) {
      const hit = resolveCallLike(db, edge.file_path, edge.raw_name);
      if (!hit) continue;
      db.updateEdgeResolution(edge.id, hit.node.id, hit.confidence);
      resolved++;
    }
  }

  return resolved;
}

/** @deprecated use resolveModuleSpecifier via context — kept for tests */
export function resolveImportPath(
  root: string,
  fromFile: string,
  specifier: string,
  fileSet: Set<string>,
): string | null {
  const ctx = buildResolveContext(root, fileSet);
  return resolveModuleSpecifier(ctx, fromFile, specifier);
}

export function resolveWithContext(
  ctx: ResolveContext,
  fromFile: string,
  specifier: string,
): string | null {
  return resolveModuleSpecifier(ctx, fromFile, specifier);
}

function resolveCallLike(
  db: GraphDb,
  filePath: string,
  rawName: string,
): { node: GraphNode; confidence: EdgeConfidence } | null {
  const simple = rawName.includes(".")
    ? (rawName.split(".").pop() ?? rawName)
    : rawName;

  const local = db.nodesInFile(filePath).filter(
    (n) =>
      n.kind !== "file" &&
      (n.name === simple ||
        n.qualified_name === simple ||
        n.qualified_name === rawName ||
        n.qualified_name.endsWith(`.${simple}`)),
  );
  if (local.length === 1) {
    return { node: local[0]!, confidence: "strong" };
  }
  if (local.length > 1) {
    const nonMethod = local.filter((n) => n.kind !== "method");
    if (nonMethod.length === 1) {
      return { node: nonMethod[0]!, confidence: "strong" };
    }
    return null;
  }

  const bindings = db.bindingsForLocal(filePath, simple);
  if (bindings.length === 1) {
    const b = bindings[0]!;
    if (b.is_namespace) return null;
    if (!b.module_path) return null;
    const target = resolveExportInModule(db, b.module_path, b.imported_name);
    if (target) return { node: target, confidence: "strong" };
  }

  if (rawName.includes(".")) {
    const [ns, ...rest] = rawName.split(".");
    const prop = rest.join(".");
    if (ns && prop) {
      const nsBind = db
        .bindingsForLocal(filePath, ns)
        .filter((b) => b.is_namespace);
      if (nsBind.length === 1 && nsBind[0]!.module_path) {
        const target = resolveExportInModule(
          db,
          nsBind[0]!.module_path,
          prop,
        );
        if (target) return { node: target, confidence: "strong" };
      }
    }
  }

  const unique = db.findUniqueByName(simple);
  if (unique) return { node: unique, confidence: "weak" };
  return null;
}

function resolveExportInModule(
  db: GraphDb,
  modulePath: string,
  importedName: string,
  depth = 0,
): GraphNode | null {
  if (depth > 6) return null;

  if (importedName === "default") {
    return db.findDefaultExport(modulePath);
  }
  if (importedName === "*") return db.getNode(nodeId(modulePath, modulePath));

  const exported = db
    .exportedInFile(modulePath)
    .filter(
      (n) => n.name === importedName || n.qualified_name === importedName,
    );
  if (exported.length === 1) return exported[0] ?? null;

  // Named re-export only (`export { x } from`) — never plain imports
  const namedRe = db
    .bindingsInFile(modulePath)
    .filter(
      (b) =>
        b.is_reexport &&
        !b.is_star_reexport &&
        !b.is_namespace &&
        b.module_path &&
        (b.local_name === importedName || b.imported_name === importedName),
    );
  if (namedRe.length === 1 && namedRe[0]!.module_path) {
    const hit = resolveExportInModule(
      db,
      namedRe[0]!.module_path,
      namedRe[0]!.imported_name,
      depth + 1,
    );
    if (hit) return hit;
  }

  // export * from './math' — follow star reexports
  for (const star of db.starReexportsFrom(modulePath)) {
    if (!star.module_path) continue;
    const hit = resolveExportInModule(
      db,
      star.module_path,
      importedName,
      depth + 1,
    );
    if (hit) return hit;
  }

  const any = db
    .nodesInFile(modulePath)
    .filter(
      (n) =>
        n.kind !== "file" &&
        (n.name === importedName || n.qualified_name === importedName),
    );
  if (any.length === 1) return any[0] ?? null;
  return null;
}
