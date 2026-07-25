import { DatabaseSync } from "node:sqlite";
import { existsSync, mkdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type {
  EdgeConfidence,
  EdgeKind,
  FileRecord,
  GraphEdge,
  GraphNode,
  ImportBinding,
  NodeKind,
} from "../models.js";
import { SCHEMA_VERSION } from "../models.js";
import { graphDbPath, graphDir } from "../config.js";

function schemaPath(): string {
  const here = dirname(fileURLToPath(import.meta.url));
  const candidates = [
    join(here, "schema.sql"),
    join(here, "..", "..", "src", "db", "schema.sql"),
  ];
  for (const p of candidates) {
    if (existsSync(p)) return p;
  }
  throw new Error("schema.sql not found");
}

const EDGE_SELECT = `id, src_id, dst_id, kind, raw_name, resolved, confidence, file_path, line`;

export class GraphDb {
  readonly db: DatabaseSync;
  readonly root: string;

  private constructor(root: string, db: DatabaseSync) {
    this.root = root;
    this.db = db;
  }

  static open(root: string, opts?: { create?: boolean }): GraphDb {
    const path = graphDbPath(root);
    const dir = graphDir(root);
    if (!existsSync(path)) {
      if (!opts?.create) {
        throw new Error(
          `No graph at ${path}. Run: codescratch init ${root}`,
        );
      }
      mkdirSync(dir, { recursive: true });
    }
    const db = new DatabaseSync(path);
    db.exec("PRAGMA journal_mode = WAL;");
    db.exec("PRAGMA foreign_keys = ON;");
    if (opts?.create || !tableExists(db, "nodes")) {
      db.exec(readFileSync(schemaPath(), "utf8"));
    }
    migrate(db);
    return new GraphDb(root, db);
  }

  close(): void {
    this.db.close();
  }

  setMeta(key: string, value: string): void {
    this.db
      .prepare(
        `INSERT INTO meta(key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
      )
      .run(key, value);
  }

  getMeta(key: string): string | null {
    const r = row<{ value: string }>(
      this.db.prepare(`SELECT value FROM meta WHERE key = ?`).get(key),
    );
    return r?.value ?? null;
  }

  upsertFile(rec: FileRecord): void {
    this.db
      .prepare(
        `INSERT INTO files(path, hash, mtime_ms, language, indexed_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(path) DO UPDATE SET
           hash = excluded.hash,
           mtime_ms = excluded.mtime_ms,
           language = excluded.language,
           indexed_at = excluded.indexed_at`,
      )
      .run(rec.path, rec.hash, rec.mtime_ms, rec.language, rec.indexed_at);
  }

  getFile(path: string): FileRecord | null {
    return (
      row<FileRecord>(
        this.db
          .prepare(
            `SELECT path, hash, mtime_ms, language, indexed_at FROM files WHERE path = ?`,
          )
          .get(path),
      ) ?? null
    );
  }

  listFiles(): FileRecord[] {
    return rows<FileRecord>(
      this.db
        .prepare(
          `SELECT path, hash, mtime_ms, language, indexed_at FROM files ORDER BY path`,
        )
        .all(),
    );
  }

  deleteFileCascade(path: string): void {
    this.db.prepare(`DELETE FROM files WHERE path = ?`).run(path);
  }

  clearFileGraph(path: string): void {
    this.db.prepare(`DELETE FROM bindings WHERE file_path = ?`).run(path);
    this.db.prepare(`DELETE FROM edges WHERE file_path = ?`).run(path);
    this.db.prepare(`DELETE FROM nodes WHERE file_path = ?`).run(path);
  }

  insertNode(node: GraphNode): void {
    this.db
      .prepare(
        `INSERT INTO nodes(id, kind, name, qualified_name, file_path, start_line, end_line, exported, signature)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        node.id,
        node.kind,
        node.name,
        node.qualified_name,
        node.file_path,
        node.start_line,
        node.end_line,
        node.exported ? 1 : 0,
        node.signature,
      );
  }

  insertEdge(edge: {
    src_id: string | null;
    dst_id: string | null;
    kind: EdgeKind;
    raw_name: string;
    resolved: boolean;
    confidence?: EdgeConfidence | null;
    file_path: string;
    line: number;
  }): void {
    this.db
      .prepare(
        `INSERT INTO edges(src_id, dst_id, kind, raw_name, resolved, confidence, file_path, line)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        edge.src_id,
        edge.dst_id,
        edge.kind,
        edge.raw_name,
        edge.resolved ? 1 : 0,
        edge.confidence ?? null,
        edge.file_path,
        edge.line,
      );
  }

  updateEdgeResolution(
    id: number,
    dst_id: string,
    confidence: EdgeConfidence = "strong",
  ): void {
    this.db
      .prepare(
        `UPDATE edges SET dst_id = ?, resolved = 1, confidence = ? WHERE id = ?`,
      )
      .run(dst_id, confidence, id);
  }

  insertBinding(b: {
    file_path: string;
    local_name: string;
    imported_name: string;
    module_specifier: string;
    module_path: string | null;
    is_type_only: boolean;
    is_namespace: boolean;
    line: number;
  }): void {
    this.db
      .prepare(
        `INSERT INTO bindings(file_path, local_name, imported_name, module_specifier, module_path, is_type_only, is_namespace, line)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        b.file_path,
        b.local_name,
        b.imported_name,
        b.module_specifier,
        b.module_path,
        b.is_type_only ? 1 : 0,
        b.is_namespace ? 1 : 0,
        b.line,
      );
  }

  updateBindingModulePath(id: number, modulePath: string): void {
    this.db
      .prepare(`UPDATE bindings SET module_path = ? WHERE id = ?`)
      .run(modulePath, id);
  }

  bindingsInFile(filePath: string): ImportBinding[] {
    return rows<DbBinding>(
      this.db
        .prepare(
          `SELECT id, file_path, local_name, imported_name, module_specifier, module_path, is_type_only, is_namespace, line
           FROM bindings WHERE file_path = ?`,
        )
        .all(filePath),
    ).map(mapBinding);
  }

  bindingsForLocal(filePath: string, localName: string): ImportBinding[] {
    return rows<DbBinding>(
      this.db
        .prepare(
          `SELECT id, file_path, local_name, imported_name, module_specifier, module_path, is_type_only, is_namespace, line
           FROM bindings WHERE file_path = ? AND local_name = ?`,
        )
        .all(filePath, localName),
    ).map(mapBinding);
  }

  unresolvedBindings(filePaths?: string[]): ImportBinding[] {
    if (filePaths && filePaths.length > 0) {
      const ph = filePaths.map(() => "?").join(",");
      return rows<DbBinding>(
        this.db
          .prepare(
            `SELECT id, file_path, local_name, imported_name, module_specifier, module_path, is_type_only, is_namespace, line
             FROM bindings WHERE module_path IS NULL AND file_path IN (${ph})`,
          )
          .all(...filePaths),
      ).map(mapBinding);
    }
    return rows<DbBinding>(
      this.db
        .prepare(
          `SELECT id, file_path, local_name, imported_name, module_specifier, module_path, is_type_only, is_namespace, line
           FROM bindings WHERE module_path IS NULL`,
        )
        .all(),
    ).map(mapBinding);
  }

  getNode(id: string): GraphNode | null {
    const r = row<DbNode>(
      this.db
        .prepare(
          `SELECT id, kind, name, qualified_name, file_path, start_line, end_line, exported, signature
         FROM nodes WHERE id = ?`,
        )
        .get(id),
    );
    return r ? mapNode(r) : null;
  }

  findNodesByName(name: string, limit = 50): GraphNode[] {
    return rows<DbNode>(
      this.db
        .prepare(
          `SELECT id, kind, name, qualified_name, file_path, start_line, end_line, exported, signature
         FROM nodes WHERE name = ? COLLATE NOCASE
         ORDER BY exported DESC, kind, qualified_name
         LIMIT ?`,
        )
        .all(name, limit),
    ).map(mapNode);
  }

  findNodesByQualifiedName(qname: string): GraphNode[] {
    return rows<DbNode>(
      this.db
        .prepare(
          `SELECT id, kind, name, qualified_name, file_path, start_line, end_line, exported, signature
         FROM nodes WHERE qualified_name = ?`,
        )
        .all(qname),
    ).map(mapNode);
  }

  searchFts(query: string, limit = 25): GraphNode[] {
    const safe = sanitizeFts(query);
    if (!safe) return [];
    return rows<DbNode>(
      this.db
        .prepare(
          `SELECT n.id, n.kind, n.name, n.qualified_name, n.file_path, n.start_line, n.end_line, n.exported, n.signature
         FROM nodes_fts f
         JOIN nodes n ON n.rowid = f.rowid
         WHERE nodes_fts MATCH ?
         ORDER BY rank
         LIMIT ?`,
        )
        .all(safe, limit),
    ).map(mapNode);
  }

  nodesInFile(filePath: string): GraphNode[] {
    return rows<DbNode>(
      this.db
        .prepare(
          `SELECT id, kind, name, qualified_name, file_path, start_line, end_line, exported, signature
         FROM nodes WHERE file_path = ? ORDER BY start_line`,
        )
        .all(filePath),
    ).map(mapNode);
  }

  edgesFrom(srcId: string, kind?: EdgeKind): GraphEdge[] {
    if (kind) {
      return rows<DbEdge>(
        this.db
          .prepare(
            `SELECT ${EDGE_SELECT} FROM edges WHERE src_id = ? AND kind = ?`,
          )
          .all(srcId, kind),
      ).map(mapEdge);
    }
    return rows<DbEdge>(
      this.db
        .prepare(`SELECT ${EDGE_SELECT} FROM edges WHERE src_id = ?`)
        .all(srcId),
    ).map(mapEdge);
  }

  edgesTo(dstId: string, kind?: EdgeKind): GraphEdge[] {
    if (kind) {
      return rows<DbEdge>(
        this.db
          .prepare(
            `SELECT ${EDGE_SELECT} FROM edges WHERE dst_id = ? AND kind = ?`,
          )
          .all(dstId, kind),
      ).map(mapEdge);
    }
    return rows<DbEdge>(
      this.db
        .prepare(`SELECT ${EDGE_SELECT} FROM edges WHERE dst_id = ?`)
        .all(dstId),
    ).map(mapEdge);
  }

  unresolvedBindableEdges(filePaths?: string[]): GraphEdge[] {
    if (filePaths && filePaths.length > 0) {
      const ph = filePaths.map(() => "?").join(",");
      return rows<DbEdge>(
        this.db
          .prepare(
            `SELECT ${EDGE_SELECT} FROM edges
             WHERE resolved = 0 AND kind IN ('calls','imports','extends','implements')
             AND file_path IN (${ph})`,
          )
          .all(...filePaths),
      ).map(mapEdge);
    }
    return rows<DbEdge>(
      this.db
        .prepare(
          `SELECT ${EDGE_SELECT} FROM edges
           WHERE resolved = 0 AND kind IN ('calls','imports','extends','implements')`,
        )
        .all(),
    ).map(mapEdge);
  }

  /** Files that import any of the given module paths (dependents). */
  filesImportingModules(modulePaths: string[]): string[] {
    if (modulePaths.length === 0) return [];
    const ph = modulePaths.map(() => "?").join(",");
    const fromBindings = rows<{ file_path: string }>(
      this.db
        .prepare(
          `SELECT DISTINCT file_path FROM bindings WHERE module_path IN (${ph})`,
        )
        .all(...modulePaths),
    ).map((r) => r.file_path);

    const fileNodeIds = modulePaths.map((p) => nodeId(p, p));
    const ph2 = fileNodeIds.map(() => "?").join(",");
    const fromEdges = rows<{ file_path: string }>(
      this.db
        .prepare(
          `SELECT DISTINCT file_path FROM edges
           WHERE kind = 'imports' AND dst_id IN (${ph2})`,
        )
        .all(...fileNodeIds),
    ).map((r) => r.file_path);

    return [...new Set([...fromBindings, ...fromEdges])];
  }

  counts(): {
    files: number;
    nodes: number;
    edges: number;
    unresolved: number;
    weak: number;
    bindings: number;
  } {
    const files = row<{ c: number }>(
      this.db.prepare(`SELECT COUNT(*) AS c FROM files`).get(),
    )!.c;
    const nodes = row<{ c: number }>(
      this.db.prepare(`SELECT COUNT(*) AS c FROM nodes`).get(),
    )!.c;
    const edges = row<{ c: number }>(
      this.db.prepare(`SELECT COUNT(*) AS c FROM edges`).get(),
    )!.c;
    const unresolved = row<{ c: number }>(
      this.db
        .prepare(`SELECT COUNT(*) AS c FROM edges WHERE resolved = 0`)
        .get(),
    )!.c;
    const weak = row<{ c: number }>(
      this.db
        .prepare(
          `SELECT COUNT(*) AS c FROM edges WHERE resolved = 1 AND confidence = 'weak'`,
        )
        .get(),
    )!.c;
    const bindings = tableExists(this.db, "bindings")
      ? row<{ c: number }>(
          this.db.prepare(`SELECT COUNT(*) AS c FROM bindings`).get(),
        )!.c
      : 0;
    return { files, nodes, edges, unresolved, weak, bindings };
  }

  transaction<T>(fn: () => T): T {
    this.db.exec("BEGIN");
    try {
      const result = fn();
      this.db.exec("COMMIT");
      return result;
    } catch (e) {
      this.db.exec("ROLLBACK");
      throw e;
    }
  }

  exportedInFile(filePath: string): GraphNode[] {
    return rows<DbNode>(
      this.db
        .prepare(
          `SELECT id, kind, name, qualified_name, file_path, start_line, end_line, exported, signature
         FROM nodes WHERE file_path = ? AND exported = 1`,
        )
        .all(filePath),
    ).map(mapNode);
  }

  findDefaultExport(filePath: string): GraphNode | null {
    const byName = this.nodesInFile(filePath).filter(
      (n) => n.name === "default" || n.qualified_name.endsWith(".default"),
    );
    if (byName.length === 1) return byName[0] ?? null;
    const exported = this.exportedInFile(filePath);
    // single export often is default in CJS-ish files — leave null if ambiguous
    const def = exported.find(
      (n) => n.name === "default" || n.qualified_name === "default",
    );
    return def ?? null;
  }

  findUniqueByName(name: string): GraphNode | null {
    const found = this.findNodesByName(name, 5);
    if (found.length === 1) return found[0] ?? null;
    const exported = found.filter((r) => r.exported);
    if (exported.length === 1) return exported[0] ?? null;
    return null;
  }

  fileModuleId(filePath: string): string | null {
    const r = row<{ id: string }>(
      this.db
        .prepare(
          `SELECT id FROM nodes WHERE file_path = ? AND kind = 'file' LIMIT 1`,
        )
        .get(filePath),
    );
    return r?.id ?? null;
  }

  /** Drop resolved call-like edges in files so resolve can rebind (no stale strong). */
  clearResolvedCallLikeInFiles(filePaths: string[]): void {
    if (filePaths.length === 0) return;
    const ph = filePaths.map(() => "?").join(",");
    this.db
      .prepare(
        `UPDATE edges SET dst_id = NULL, resolved = 0, confidence = NULL
         WHERE file_path IN (${ph})
           AND kind IN ('calls','imports','extends','implements')
           AND resolved = 1`,
      )
      .run(...filePaths);
  }

  clearBindingModulePathsInFiles(filePaths: string[]): void {
    if (filePaths.length === 0) return;
    const ph = filePaths.map(() => "?").join(",");
    this.db
      .prepare(
        `UPDATE bindings SET module_path = NULL WHERE file_path IN (${ph})`,
      )
      .run(...filePaths);
  }
}

function migrate(db: DatabaseSync): void {
  // edges.confidence
  if (tableExists(db, "edges") && !columnExists(db, "edges", "confidence")) {
    db.exec(`ALTER TABLE edges ADD COLUMN confidence TEXT`);
  }
  // bindings table
  if (!tableExists(db, "bindings")) {
    db.exec(`
      CREATE TABLE bindings (
        id               INTEGER PRIMARY KEY AUTOINCREMENT,
        file_path        TEXT NOT NULL,
        local_name       TEXT NOT NULL,
        imported_name    TEXT NOT NULL,
        module_specifier TEXT NOT NULL,
        module_path      TEXT,
        is_type_only     INTEGER NOT NULL DEFAULT 0,
        is_namespace     INTEGER NOT NULL DEFAULT 0,
        line             INTEGER NOT NULL,
        FOREIGN KEY (file_path) REFERENCES files(path) ON DELETE CASCADE
      );
      CREATE INDEX IF NOT EXISTS idx_bindings_file ON bindings(file_path);
      CREATE INDEX IF NOT EXISTS idx_bindings_local ON bindings(file_path, local_name);
    `);
  }
  const ver = row<{ value: string }>(
    db.prepare(`SELECT value FROM meta WHERE key = 'schema_version'`).get(),
  )?.value;
  if (ver !== SCHEMA_VERSION) {
    db.prepare(
      `INSERT INTO meta(key, value) VALUES ('schema_version', ?)
       ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    ).run(SCHEMA_VERSION);
  }
}

interface DbNode {
  id: string;
  kind: NodeKind;
  name: string;
  qualified_name: string;
  file_path: string;
  start_line: number;
  end_line: number;
  exported: number;
  signature: string | null;
}

interface DbEdge {
  id: number;
  src_id: string | null;
  dst_id: string | null;
  kind: EdgeKind;
  raw_name: string;
  resolved: number;
  confidence: string | null;
  file_path: string;
  line: number;
}

interface DbBinding {
  id: number;
  file_path: string;
  local_name: string;
  imported_name: string;
  module_specifier: string;
  module_path: string | null;
  is_type_only: number;
  is_namespace: number;
  line: number;
}

function mapNode(r: DbNode): GraphNode {
  return {
    id: r.id,
    kind: r.kind,
    name: r.name,
    qualified_name: r.qualified_name,
    file_path: r.file_path,
    start_line: r.start_line,
    end_line: r.end_line,
    exported: !!r.exported,
    signature: r.signature,
  };
}

function mapEdge(r: DbEdge): GraphEdge {
  return {
    id: r.id,
    src_id: r.src_id,
    dst_id: r.dst_id,
    kind: r.kind,
    raw_name: r.raw_name,
    resolved: !!r.resolved,
    confidence: (r.confidence as EdgeConfidence | null) ?? null,
    file_path: r.file_path,
    line: r.line,
  };
}

function mapBinding(r: DbBinding): ImportBinding {
  return {
    id: r.id,
    file_path: r.file_path,
    local_name: r.local_name,
    imported_name: r.imported_name,
    module_specifier: r.module_specifier,
    module_path: r.module_path,
    is_type_only: !!r.is_type_only,
    is_namespace: !!r.is_namespace,
    line: r.line,
  };
}

function tableExists(db: DatabaseSync, name: string): boolean {
  const r = db
    .prepare(
      `SELECT name FROM sqlite_master WHERE type='table' AND name = ?`,
    )
    .get(name) as { name: string } | undefined;
  return !!r;
}

function columnExists(
  db: DatabaseSync,
  table: string,
  column: string,
): boolean {
  const cols = db.prepare(`PRAGMA table_info(${table})`).all() as {
    name: string;
  }[];
  return cols.some((c) => c.name === column);
}

function sanitizeFts(query: string): string | null {
  const tokens = query
    .trim()
    .split(/[\s/\\.:]+/)
    .map((t) => t.replace(/["'*^\s]/g, ""))
    .filter((t) => t.length > 0);
  if (tokens.length === 0) return null;
  return tokens.map((t) => `"${t}"*`).join(" ");
}

function rows<T>(result: unknown): T[] {
  return result as T[];
}

function row<T>(result: unknown): T | undefined {
  return result as T | undefined;
}

export function nodeId(filePath: string, qualifiedName: string): string {
  return `${filePath}::${qualifiedName}`;
}
