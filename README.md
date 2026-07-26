# codescratch

Local-first **code structure graph** for AI agents (TypeScript/JavaScript).

Tree-sitter → SQLite → MCP/CLI. Host-owned freshness + on-demand deep queries. Not a tsserver replacement.

**Fit today:** TS trees with relative imports, `tsconfig` paths, workspace packages, `export *` barrels.  
**Not yet:** treat callers as exhaustive on huge monorepos without verifying trust + source.

## Install

```bash
cd codescratch
npm install
npm run build
npm link   # puts `codescratch` on PATH
```

Requires **Node ≥ 22.5** (`node:sqlite`). MIT.

## Hybrid model

| Layer | Who | How |
|-------|-----|-----|
| Keep graph fresh | **Host** | `codescratch ensure` + Claude Code hooks (single-flight, debounced) |
| Deep structure | **Agent** | MCP `cs_explore` / `cs_callers` / `cs_impact` / … |
| Emergency rebuild | Agent/human | `cs_reindex` / `ensure --full` if host path down |

Branch checkout → **one** ensure job (HEAD meta + lock). No agent reindex storm.

## CLI

```bash
codescratch ensure /path/to/repo     # host path (preferred)
codescratch ensure /path/to/repo --full
codescratch init /path/to/repo       # ensure --full
codescratch reindex /path/to/repo    # same lock as ensure
codescratch status
codescratch search add -r /path/to/repo
codescratch explore Calculator -r /path/to/repo
codescratch callers add -r /path/to/repo
codescratch impact add -r /path/to/repo -d both
codescratch mcp
```

Graph: `<repo>/.codescratch/graph.db`  
Lock: `<repo>/.codescratch/reindex.lock`

## Claude Code

### MCP (deep queries)

```json
{
  "mcpServers": {
    "codescratch": {
      "command": "codescratch",
      "args": ["mcp"],
      "env": { "CODESCRATCH_ROOT": "${workspaceFolder}" }
    }
  }
}
```

Tools: `cs_status`, `cs_search`, `cs_explore`, `cs_callers`, `cs_callees`, `cs_impact`, `cs_reindex` (emergency).

### Host hooks (freshness)

See [integrations/claude-code/README.md](integrations/claude-code/README.md) — SessionStart + PostToolUse → detached `ensure`.

## Resolve

- Relative `./` / `../` (`.js` → `.ts`)
- `tsconfig` `baseUrl` + `paths` (`@/*`, …)
- Workspace package names via `exports`/`main`
- `export *` / named `export { x } from` barrels
- Import bindings → **strong**; unique-global / unknown receiver → **weak** (`reason=`)
- Incremental dirty: full rebind of dirty files + importers

## Trust

- **rebuilding** — host ensure in progress (absence ≠ proof)
- **stale** — content hash drift and/or HEAD moved
- **partial** — weak unique-global ratio / unresolved / non-exhaustive hash
- **fresh** — exhaustive content verification
- Query tools: one-line trust; `cs_status`: full notes

## License

MIT
