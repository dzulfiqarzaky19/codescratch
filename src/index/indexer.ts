import { createHash } from "node:crypto";
import { readFileSync, statSync, existsSync } from "node:fs";
import { join } from "node:path";
import fg from "fast-glob";
import ignore from "ignore";
import {
  languageForPath,
  resolveRoot,
  SUPPORTED_EXTENSIONS,
} from "../config.js";
import { GraphDb, nodeId } from "../db/client.js";
import { extractTypeScriptFile } from "../extract/typescript.js";
import { SCHEMA_VERSION, type IndexStats } from "../models.js";
import { toPosix } from "../util/paths.js";
import { resolveEdges } from "./resolve.js";

const SKIP_DIRS = [
  "**/node_modules/**",
  "**/.git/**",
  "**/dist/**",
  "**/build/**",
  "**/.codescratch/**",
  "**/coverage/**",
  "**/.next/**",
  "**/out/**",
];

export async function indexRepo(
  rootInput?: string,
  opts?: { full?: boolean },
): Promise<IndexStats> {
  const started = Date.now();
  const root = resolveRoot(rootInput);
  const full = opts?.full === true;
  const db = GraphDb.open(root, { create: true });

  try {
    const ig = loadGitignore(root);
    const patterns = [...SUPPORTED_EXTENSIONS].map((e) => `**/*${e}`);
    const found = await fg(patterns, {
      cwd: root,
      onlyFiles: true,
      dot: false,
      ignore: SKIP_DIRS,
      absolute: false,
    });

    const files = found
      .map((p) => toPosix(p))
      .filter((p) => !ig.ignores(p))
      .sort();

    const existing = new Map(db.listFiles().map((f) => [f.path, f]));
    let indexed = 0;
    let skipped = 0;
    let removed = 0;
    const dirty: string[] = [];
    const seen = new Set<string>();

    for (const relPath of files) {
      seen.add(relPath);
      const abs = join(root, relPath);
      const st = statSync(abs);
      const source = readFileSync(abs, "utf8");
      const hash = sha256(source);
      const prev = existing.get(relPath);
      if (!full && prev && prev.hash === hash) {
        skipped++;
        continue;
      }

      const language = languageForPath(relPath);
      if (!language) {
        skipped++;
        continue;
      }

      const extraction = await extractTypeScriptFile(relPath, source, language);
      const now = new Date().toISOString();

      db.transaction(() => {
        if (prev) db.clearFileGraph(relPath);
        db.upsertFile({
          path: relPath,
          hash,
          mtime_ms: st.mtimeMs,
          language,
          indexed_at: now,
        });

        const fileQn = relPath;
        const fileNode = {
          id: nodeId(relPath, fileQn),
          kind: "file" as const,
          name: relPath.split("/").pop() ?? relPath,
          qualified_name: fileQn,
          file_path: relPath,
          start_line: 1,
          end_line: source.split(/\r?\n/).length,
          exported: false,
          signature: null,
        };
        db.insertNode(fileNode);

        const qnToId = new Map<string, string>();
        qnToId.set(fileQn, fileNode.id);

        for (const sym of extraction.symbols) {
          const id = nodeId(relPath, sym.qualifiedName);
          qnToId.set(sym.qualifiedName, id);
          db.insertNode({
            id,
            kind: sym.kind,
            name: sym.name,
            qualified_name: sym.qualifiedName,
            file_path: relPath,
            start_line: sym.startLine,
            end_line: sym.endLine,
            exported: sym.exported,
            signature: sym.signature,
          });
        }

        for (const b of extraction.bindings) {
          db.insertBinding({
            file_path: relPath,
            local_name: b.localName,
            imported_name: b.importedName,
            module_specifier: b.moduleSpecifier,
            module_path: null,
            is_type_only: b.isTypeOnly,
            is_namespace: b.isNamespace,
            line: b.line,
          });
        }

        for (const ref of extraction.refs) {
          if (ref.kind === "has_method" && ref.targetHint) {
            const src = ref.srcQualifiedName
              ? (qnToId.get(ref.srcQualifiedName) ?? null)
              : fileNode.id;
            const dst = qnToId.get(ref.targetHint) ?? null;
            db.insertEdge({
              src_id: src,
              dst_id: dst,
              kind: ref.kind,
              raw_name: ref.rawName,
              resolved: !!dst,
              confidence: dst ? "strong" : null,
              file_path: relPath,
              line: ref.line,
            });
            continue;
          }

          const src = ref.srcQualifiedName
            ? (qnToId.get(ref.srcQualifiedName) ?? fileNode.id)
            : fileNode.id;

          let dst: string | null = null;
          let resolved = false;
          if (
            ref.targetHint &&
            (ref.kind === "calls" ||
              ref.kind === "extends" ||
              ref.kind === "implements")
          ) {
            const hit =
              qnToId.get(ref.targetHint) ??
              qnToId.get(ref.rawName) ??
              findLocalByName(qnToId, extraction.symbols, ref.rawName);
            if (hit) {
              dst = hit;
              resolved = true;
            }
          }

          db.insertEdge({
            src_id: src,
            dst_id: dst,
            kind: ref.kind,
            raw_name: ref.rawName,
            resolved,
            confidence: resolved ? "strong" : null,
            file_path: relPath,
            line: ref.line,
          });
        }
      });

      dirty.push(relPath);
      indexed++;
    }

    for (const [path] of existing) {
      if (!seen.has(path)) {
        db.deleteFileCascade(path);
        removed++;
        dirty.push(path);
      }
    }

    db.transaction(() => {
      // Full graph resolve always when full index; on incremental dirty set,
      // full rebind of dirty + importers (clear settled edges first).
      if (full || dirty.length === 0) {
        resolveEdges(db, root);
      } else {
        resolveEdges(db, root, {
          dirtyFiles: dirty.filter((p) => seen.has(p)),
          fullRebind: true,
        });
      }
    });

    const counts = db.counts();
    const now = new Date().toISOString();
    db.setMeta("root_path", root);
    db.setMeta("schema_version", SCHEMA_VERSION);
    db.setMeta("last_index_at", now);
    if (full) {
      db.setMeta("last_full_index_at", now);
    }

    return {
      root,
      full,
      files_total: files.length,
      files_indexed: indexed,
      files_skipped: skipped,
      files_removed: removed,
      nodes: counts.nodes,
      edges: counts.edges,
      unresolved_edges: counts.unresolved,
      bindings: counts.bindings,
      duration_ms: Date.now() - started,
    };
  } finally {
    db.close();
  }
}

function findLocalByName(
  qnToId: Map<string, string>,
  symbols: { name: string; qualifiedName: string }[],
  name: string,
): string | null {
  const simple = name.includes(".") ? (name.split(".").pop() ?? name) : name;
  const matches = symbols.filter((s) => s.name === simple || s.name === name);
  if (matches.length === 1) {
    return qnToId.get(matches[0]!.qualifiedName) ?? null;
  }
  return null;
}

function loadGitignore(root: string) {
  const ig = ignore();
  const gi = join(root, ".gitignore");
  if (existsSync(gi)) {
    ig.add(readFileSync(gi, "utf8"));
  }
  ig.add(["node_modules", "dist", ".codescratch", "coverage"]);
  return ig;
}

function sha256(s: string): string {
  return createHash("sha256").update(s).digest("hex");
}
