import { describe, expect, it } from "vitest";
import { DatabaseSync } from "node:sqlite";
import { mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { GraphDb } from "../src/db/client.js";
import { SCHEMA_VERSION } from "../src/models.js";

/** Build a graph.db at an older schema, then open it with the current client. */
function seedLegacyDb(root: string, drop: RegExp[], version: string): void {
  mkdirSync(join(root, ".codescratch"), { recursive: true });
  let sql = readFileSync(
    join(__dirname, "../src/db/schema.sql"),
    "utf8",
  );
  for (const re of drop) sql = sql.replace(re, "");
  const db = new DatabaseSync(join(root, ".codescratch", "graph.db"));
  db.exec(sql);
  db.exec(`INSERT INTO meta(key,value) VALUES('schema_version','${version}')`);
  db.exec(
    `INSERT INTO files(path,hash,mtime_ms,language,indexed_at)
     VALUES('src/old.ts','deadbeef',1,'typescript','2020-01-01')`,
  );
  db.exec(
    `INSERT INTO nodes(id,kind,name,qualified_name,file_path,start_line,end_line,exported,signature)
     VALUES('src/old.ts::oldFn','function','oldFn','oldFn','src/old.ts',1,1,1,NULL)`,
  );
  db.exec(
    `INSERT INTO edges(src_id,dst_id,kind,raw_name,resolved,confidence,file_path,line)
     VALUES('src/old.ts::oldFn','src/old.ts::oldFn','calls','oldFn',1,'strong','src/old.ts',1)`,
  );
  db.close();
}

describe("schema migration", () => {
  it("upgrades a v2 graph (no reason, no size_bytes) without data loss", () => {
    const root = mkdtempSync(join(tmpdir(), "cs-mig-"));
    try {
      seedLegacyDb(
        root,
        [/  reason     TEXT,\n/, /  size_bytes INTEGER NOT NULL DEFAULT 0,\n/],
        "2",
      );

      const db = GraphDb.open(root);
      try {
        const edgeCols = (
          db.db.prepare(`PRAGMA table_info(edges)`).all() as { name: string }[]
        ).map((c) => c.name);
        const fileCols = (
          db.db.prepare(`PRAGMA table_info(files)`).all() as { name: string }[]
        ).map((c) => c.name);
        expect(edgeCols).toContain("reason");
        expect(fileCols).toContain("size_bytes");
        expect(db.getMeta("schema_version")).toBe(SCHEMA_VERSION);

        // legacy rows survive, with the new columns unset rather than invented
        const e = db.edgesFrom("src/old.ts::oldFn", "calls")[0]!;
        expect(e.confidence).toBe("strong");
        expect(e.reason).toBeNull();
        const f = db.getFile("src/old.ts")!;
        expect(f.hash).toBe("deadbeef");
        expect(f.size_bytes).toBe(0);
        // a 0 size must not be read as "empty file matches" downstream
        expect(db.counts().nodes).toBe(1);
      } finally {
        db.close();
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("is idempotent when reopened at the current schema", () => {
    const root = mkdtempSync(join(tmpdir(), "cs-mig2-"));
    try {
      seedLegacyDb(root, [/  reason     TEXT,\n/], "3");
      GraphDb.open(root).close();
      const db = GraphDb.open(root);
      try {
        expect(db.getMeta("schema_version")).toBe(SCHEMA_VERSION);
        expect(db.counts().edges).toBe(1);
      } finally {
        db.close();
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
