PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS files (
  path       TEXT PRIMARY KEY,
  hash       TEXT NOT NULL,
  mtime_ms   INTEGER NOT NULL,
  language   TEXT NOT NULL,
  indexed_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS nodes (
  id             TEXT PRIMARY KEY,
  kind           TEXT NOT NULL,
  name           TEXT NOT NULL,
  qualified_name TEXT NOT NULL,
  file_path      TEXT NOT NULL,
  start_line     INTEGER NOT NULL,
  end_line       INTEGER NOT NULL,
  exported       INTEGER NOT NULL DEFAULT 0,
  signature      TEXT,
  FOREIGN KEY (file_path) REFERENCES files(path) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);
CREATE INDEX IF NOT EXISTS idx_nodes_qname ON nodes(qualified_name);
CREATE INDEX IF NOT EXISTS idx_nodes_file ON nodes(file_path);
CREATE INDEX IF NOT EXISTS idx_nodes_kind ON nodes(kind);

CREATE TABLE IF NOT EXISTS edges (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  src_id     TEXT,
  dst_id     TEXT,
  kind       TEXT NOT NULL,
  raw_name   TEXT NOT NULL,
  resolved   INTEGER NOT NULL DEFAULT 0,
  confidence TEXT,
  file_path  TEXT NOT NULL,
  line       INTEGER NOT NULL,
  FOREIGN KEY (src_id) REFERENCES nodes(id) ON DELETE CASCADE,
  FOREIGN KEY (dst_id) REFERENCES nodes(id) ON DELETE SET NULL,
  FOREIGN KEY (file_path) REFERENCES files(path) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(src_id);
CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst_id);
CREATE INDEX IF NOT EXISTS idx_edges_kind ON edges(kind);
CREATE INDEX IF NOT EXISTS idx_edges_raw ON edges(raw_name);
CREATE INDEX IF NOT EXISTS idx_edges_file ON edges(file_path);
CREATE INDEX IF NOT EXISTS idx_edges_resolved ON edges(resolved);

-- local binding → module export (import { add as x } → local x, imported add)
CREATE TABLE IF NOT EXISTS bindings (
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

CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
  name,
  qualified_name,
  file_path,
  content='nodes',
  content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS nodes_ai AFTER INSERT ON nodes BEGIN
  INSERT INTO nodes_fts(rowid, name, qualified_name, file_path)
  VALUES (new.rowid, new.name, new.qualified_name, new.file_path);
END;

CREATE TRIGGER IF NOT EXISTS nodes_ad AFTER DELETE ON nodes BEGIN
  INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, file_path)
  VALUES ('delete', old.rowid, old.name, old.qualified_name, old.file_path);
END;

CREATE TRIGGER IF NOT EXISTS nodes_au AFTER UPDATE ON nodes BEGIN
  INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, file_path)
  VALUES ('delete', old.rowid, old.name, old.qualified_name, old.file_path);
  INSERT INTO nodes_fts(rowid, name, qualified_name, file_path)
  VALUES (new.rowid, new.name, new.qualified_name, new.file_path);
END;
