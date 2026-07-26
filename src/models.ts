export type NodeKind =
  | "file"
  | "module"
  | "function"
  | "class"
  | "method"
  | "interface"
  | "type"
  | "variable";

export type EdgeKind =
  | "imports"
  | "exports"
  | "calls"
  | "extends"
  | "implements"
  | "has_method"
  | "has_member";

export type EdgeConfidence = "strong" | "weak";

/**
 * Why an edge bound to its target. `strong` is a binding/lexical fact, never a
 * type check — `receiver-unknown` and `unique-global` are guesses.
 */
export type EdgeReason =
  | "same-file"
  | "import-binding"
  | "namespace-member"
  | "this-member"
  | "unique-global"
  | "receiver-unknown";

/**
 * Freshness only: does the graph match the files on disk? Says nothing about
 * how completely that was checked (`CoverageLevel`) or how well the code
 * resolved (`GraphQuality`) — three independent axes, three fields.
 */
export type TrustLevel = "fresh" | "stale" | "missing" | "rebuilding";

/** How much of the freshness claim was actually verified by content hash. */
export type CoverageLevel = "exhaustive" | "sampled";

/** Resolution quality of the graph itself — orthogonal to freshness. */
export type GraphQuality = "ok" | "degraded";

export type ImpactDirection = "up" | "down" | "both";

export interface GraphNode {
  id: string;
  kind: NodeKind;
  name: string;
  qualified_name: string;
  file_path: string;
  start_line: number;
  end_line: number;
  exported: boolean;
  signature: string | null;
}

export interface GraphEdge {
  id: number;
  src_id: string | null;
  dst_id: string | null;
  kind: EdgeKind;
  raw_name: string;
  resolved: boolean;
  confidence: EdgeConfidence | null;
  /** null on pre-v3 rows */
  reason: EdgeReason | null;
  file_path: string;
  line: number;
}

export interface ImportBinding {
  id: number;
  file_path: string;
  local_name: string;
  imported_name: string;
  module_specifier: string;
  module_path: string | null;
  is_type_only: boolean;
  is_namespace: boolean;
  is_star_reexport: boolean;
  /** true for `export { x } from` / `export * from` — not plain imports */
  is_reexport: boolean;
  line: number;
}

export interface FileRecord {
  path: string;
  hash: string;
  mtime_ms: number;
  /** 0 on pre-v4 rows — treated as unknown, forcing a hash check */
  size_bytes: number;
  language: string;
  indexed_at: string;
}

export interface ExtractedSymbol {
  kind: Exclude<NodeKind, "file" | "module">;
  name: string;
  qualifiedName: string;
  startLine: number;
  endLine: number;
  exported: boolean;
  signature: string | null;
  parentQualifiedName?: string;
}

export interface ExtractedRef {
  kind: EdgeKind;
  srcQualifiedName: string | null;
  rawName: string;
  targetHint: string | null;
  line: number;
  localName?: string;
  isTypeOnly?: boolean;
}

export interface ExtractedBinding {
  localName: string;
  /** export name in module, 'default', or '*' for namespace / star-reexport */
  importedName: string;
  moduleSpecifier: string;
  isTypeOnly: boolean;
  isNamespace: boolean;
  /** `export * from '…'` — all names forwarded from module */
  isStarReexport?: boolean;
  /** `export { x } from` / star reexport — not a plain import */
  isReexport?: boolean;
  line: number;
}

export interface FileExtraction {
  filePath: string;
  language: string;
  symbols: ExtractedSymbol[];
  refs: ExtractedRef[];
  bindings: ExtractedBinding[];
}

export interface TrustInfo {
  /** freshness vs disk */
  trust: TrustLevel;
  /** how thoroughly `trust` was verified */
  coverage: CoverageLevel;
  /** files whose content was hashed, and the total considered */
  files_hashed: number;
  /** resolution quality — a degraded graph can still be perfectly fresh */
  graph: GraphQuality;
  indexed_at: string | null;
  last_full_index_at: string | null;
  file_count: number;
  node_count: number;
  edge_count: number;
  unresolved_edge_count: number;
  weak_edge_count: number;
  notes: string[];
  reindex_cmd: string;
}

export interface IndexStats {
  root: string;
  full: boolean;
  files_total: number;
  files_indexed: number;
  files_skipped: number;
  files_removed: number;
  nodes: number;
  edges: number;
  unresolved_edges: number;
  bindings: number;
  duration_ms: number;
}

/** Honest miss list — also surfaced in trust notes. */
export const EXTRACTOR_LIMITATIONS = [
  "dynamic import()/require() not modeled",
  "DI/proxies/reflection invisible",
  "object-literal methods only partial",
  "JSX component identity is syntactic only",
] as const;

export const SCHEMA_VERSION = "4";
