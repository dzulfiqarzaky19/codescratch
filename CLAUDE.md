# codescratch

Local TS/JS code structure graph for AI agents. SQLite under `<repo>/.codescratch/`. MCP + CLI.

**Not coupled to memory-agent.** Hosts may load both MCP servers; no shared state.

## Agent protocol

1. Prefer structural tools over blind grep for “where defined / who calls / blast radius”.
2. Read `trust:` every reply. `stale` → run `cs_reindex` (or the `reindex:` command in the banner). `partial` / unresolved / `conf=weak` → do not treat absence as proof.
3. Critical paths (auth, money, deletes): verify by reading source. Graph misses dynamic `import()`, DI, proxies.

### Tools

| Tool | When |
|------|------|
| `cs_status` | Graph missing/stale? Exact reindex command. |
| `cs_search` | Find symbol by name |
| `cs_explore` | One symbol/file: members, calls, callers, imports, **bindings** |
| `cs_callers` / `cs_callees` | Call edges (resolved; weak labeled) |
| `cs_impact` | Blast radius: `direction=up\|down\|both` (default up) |
| `cs_reindex` | Incremental reindex when stale (`full=true` for rebuild) |

Optional `root` on every tool for monorepos (else `CODESCRATCH_ROOT` / cwd).

### Trust / confidence

- `strong` — same-file or import-binding path
- `weak` — unique-global-name fallback only (verify)
- Unresolved package imports stay open

### Resolve (v0.1+)

- Relative + `tsconfig` paths + workspace package names
- Dirty incremental: rebind dirty + importers (clears settled call/import edges first)
- Stale detection = content hash sample, not mtime

### Extractor misses (v1)

- dynamic `import()` / `require()`
- DI / proxies / reflection
- object-literal methods only partial
- `export *` not fully expanded
- JSX component identity syntactic only


## Layout

```
src/
  extract/     tree-sitter TS/JS + import bindings
  index/       walk, hash, scoped resolve
  query/       explore/search/callers/impact + trust
  db/          node:sqlite graph (schema v2)
  mcp.ts       MCP stdio
  cli.ts       CLI
```

## Dev

```
npm install
npm test
npm run build
node dist/cli.js init <path>
```
