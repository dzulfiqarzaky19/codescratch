# codescratch (Rust rewrite — v0.1 spine)

Single static binary. No Node, no npm, no wasm sidecar, no user-side toolchain.
See [../RUST-REWRITE.md](../RUST-REWRITE.md) for the full plan. The TS tree at the
repo root stays as the port reference until this reaches parity.

## Build

```bash
cd rust
cargo build            # first build compiles bundled SQLite + tree-sitter (C) → slow
cargo build --release  # stripped/LTO static binary → target/release/codescratch
```

Needs a C compiler on PATH (`cc`/`gcc`) for bundled SQLite + tree-sitter grammars.

## Use

```bash
codescratch init                 # build the graph under ./.codescratch/
codescratch status               # trust banner
codescratch explore <Symbol>     # banner + source + calls + callers (blast)
codescratch search <name>        # FTS fuzzy find
codescratch ensure               # bring graph up to date (host-owned, single-flight)
codescratch mcp                  # MCP stdio server (explore + status listed)
```

Root selection: arg path → `CODESCRATCH_ROOT` env → cwd.
Extra MCP tools: `CODESCRATCH_MCP_TOOLS=search`.

## What's implemented (v0.1)

- clap CLI; `ignore` walk + `blake3` hash; tree-sitter TS/JS extract (recursive
  node walk); rusqlite schema + FTS5.
- resolve precedence with honest `reason`+`conf`: import-binding → same-file →
  receiver-unknown → unique-global → unresolved. Relative + `index` module resolution.
- host `ensure` under an `O_EXCL` single-flight lock; git HEAD → `indexed_head`;
  `reindex_state=rebuilding` so readers never see torn data.
- 3-axis trust banner (freshness × coverage × graph quality).
- MCP stdio: `explore` + `status` listed; `search` behind the env allowlist.

## Deferred (v0.2+)

- Incremental dirty+importers (v0.1 does a full rebuild per `ensure`).
- tsconfig `paths`/`baseUrl` + workspace-package + `export *` barrel resolution.
- `notify` native watcher; `rayon` parallel parse.
- call-path spine, depth-grouped blast, `detect_changes`, route/process nodes.
