# codescratch

Local-first **code structure graph** for AI agents (TypeScript/JavaScript).

Tree-sitter → SQLite → MCP/CLI. Honest trust metadata. Not a tsserver replacement.

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

## CLI

```bash
codescratch init /path/to/repo
codescratch reindex /path/to/repo
codescratch status
codescratch search add -r /path/to/repo
codescratch explore Calculator -r /path/to/repo
codescratch callers add -r /path/to/repo
codescratch impact add -r /path/to/repo -d both
codescratch mcp
```

Graph: `<repo>/.codescratch/graph.db`

## Claude Code MCP

**Preferred** (after `npm link` or global install):

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

**Dev checkout without link:**

```json
{
  "mcpServers": {
    "codescratch": {
      "command": "node",
      "args": ["D:/dev/projects/codescratch/dist/mcp.js"],
      "env": { "CODESCRATCH_ROOT": "${workspaceFolder}" }
    }
  }
}
```

Tools: `cs_status`, `cs_search`, `cs_explore`, `cs_callers`, `cs_callees`, `cs_impact`, `cs_reindex`.  
Optional `root` on each tool for multi-root workspaces.

## Resolve

- Relative `./` / `../` (`.js` → `.ts`)
- `tsconfig` `baseUrl` + `paths` (`@/*`, …)
- Workspace package names via `exports`/`main`
- `export *` / named `export { x } from` barrels
- Import bindings (aliases, namespace) → **strong**; unique-global-name → **weak**
- Incremental dirty: full rebind of dirty files + importers

## Trust

- **Stale** = content hash drift (not mtime touch)
- **Weak rate** → `partial` only above threshold
- Query tools: one-line trust; `cs_status`: full notes

## License

MIT
