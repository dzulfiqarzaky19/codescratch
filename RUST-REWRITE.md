# Rust rewrite — a cheaper/lighter/better code-graph for agents

> Implementation plan. [GOAL.md](GOAL.md) stays the principles north-star; this file is the concrete build.

## Context

**Why:** Three tools solve "give an AI agent a structural map of a codebase so it stops grep-crawling and burning tokens": the user's own **codescratch** (TS, Node ≥22.5, `node:sqlite`), **codegraph** (`@colbymchenry/codegraph` v1.6.0, MIT, Node bundle), and **GitNexus** (TS, PolyForm-NC, native LadybugDB/Kuzu + 125MB vendored grammars). Each has one thing the others lack. The user wants one tool that is **cheaper for the agent, lighter to install/run, and better** than all three.

**Decision:** *Fresh rewrite*, not evolve codescratch. Language: **Rust**, compiled to a single static binary — the lightest possible install (one downloaded file, no runtime, no npm, no wasm sidecar, no C/C++ toolchain for the user).

**Outcome:** A stranger clones a random TS repo on a clean box, runs one install command + `init`, and their agent answers "what breaks if I change X?" in **one MCP call** — structure + call-path + blast radius + a freshness lie-detector, not a file dump.

**Naming:** `codegraph` is taken on npm (and likely crates.io). Pick a distinct crate/binary name before publish (keep `codescratch`, or new). Non-blocking — placeholder `codegraph` used below.

---

## What each parent contributes (all three read, grounded)

**Port from codescratch (the user's prior art — designs, reimplement in Rust):**
- **Trust banner = the moat.** Three orthogonal axes, computed in [src/query/trust.ts:38-254](src/query/trust.ts#L38-L254): `trust` (fresh/stale/rebuilding/missing) × `coverage` (exhaustive/sampled) × `graph` (ok/degraded). Rendered in [src/query/format.ts:12-81](src/query/format.ts#L12-L81). Neither parent has this. Ports cheaply — it's ~250 lines of logic.
- **Edge honesty:** every resolved edge carries `reason` (same-file / import-binding / namespace-member / this-member / unique-global / receiver-unknown) + `conf` (strong/weak). Resolution precedence in [src/index/resolve.ts:159-242](src/index/resolve.ts#L159-L242).
- **Host-owned freshness:** `ensure` with single-flight `O_EXCL` lock + PID/TTL steal ([src/host/lock.ts](src/host/lock.ts)), git HEAD → `indexed_head` meta, `reindex_state=rebuilding` so readers never see a half-written graph ([src/host/ensure.ts:114-188](src/host/ensure.ts#L114-L188)).
- **Incremental resolve:** dirty ∪ importers-of-dirty ∪ importers-of-removed ∪ orphan-edge files; full-rebind the scope ([src/index/resolve.ts:31-127](src/index/resolve.ts#L31-L127)).
- **Module resolution:** relative + tsconfig `paths`/`baseUrl` (with `extends`) + workspace packages + `export *` barrels ([src/index/module-resolve.ts](src/index/module-resolve.ts)).

**Steal from codegraph (ideas, not code — MIT but we rewrite):**
- **One listed MCP tool.** `DEFAULT_MCP_TOOLS = {'explore'}`; the rest exist but are hidden behind an env allowlist. Agents stop mis-picking.
- **The fat explore payload = markdown, one call:** header → **call-path spine** (named symbols, heuristic hops inline) → relationships → **verbatim source, byte-budgeted per file** → **blast radius** (caller files + test files, locations only). This is the product surface.
- **Native watcher, no dependency weight:** codegraph uses bare `fs.watch`, 2s debounce, 300ms quick-path when ≤2 files pending, scoped sync when ≤500 files. We get the same for free from Rust's `notify` crate.
- **Heuristic dispatch edges tagged `provenance: heuristic`** vs `tree-sitter` (callbacks, EventEmitter, `setState`→render) — surfaced to the agent with human labels, never faked as AST.
- **`install` command writes MCP config** for many agents (Claude/Cursor/Codex/Copilot/Gemini/opencode…). Single-binary distribution via curl + GitHub Releases.

**Steal from GitNexus (behavior only — PolyForm-NC, do not read for code):**
- **Index-time precompute so query is 1–3 reads:** routes, Leiden communities, processes are materialized at index time; only impact/context do a bounded live BFS. This is the "one call is complete" payoff.
- **`detect_changes`:** `git diff` → hunks → map line-ranges to symbols by overlap → join to precomputed processes. Returns changed + affected + risk, with a `partial` flag (anti-false-negative).
- **Single edge table with a `type` discriminator** (their `CodeRelation`) — simpler than one table per edge kind.
- **Hybrid search:** BM25 + optional local embeddings fused with RRF (K=60). Default stays FTS.

**Explicitly reject:** LadybugDB/Kuzu native dep (+125MB vendored grammars), web UI / `serve` / auto-wiki (~10% of GitNexus is UI you'd never want), 17-tool surface, `--pdg` taint machinery, PolyForm license, `@scarf/scarf` install beacon, ANY telemetry (codegraph's is opt-out; we ship none).

---

## Rust stack (grounded, all stable crates)

| Concern | Crate | Note |
|---|---|---|
| Parse | `tree-sitter` + `tree-sitter-typescript`, `tree-sitter-javascript` | Native, **compiled into the binary**. No wasm files to ship. |
| Store | `rusqlite` (features `bundled`, `fts5`) | Static SQLite in the binary. WAL + FTS5, same as parents, zero system dep. |
| Walk | `ignore` (ripgrep's walker) | `.gitignore`-aware, parallel, fast. |
| Hash | `blake3` | Fast content hashing for the stale-gate. |
| Watch | `notify` | Cross-platform (FSEvents/inotify/ReadDirectoryChangesW) — native, no chokidar equivalent needed. |
| CLI | `clap` (derive) | Subcommands: init / ensure / explore / search / status / mcp / setup / watch. |
| MCP | `rmcp` (official Rust MCP SDK) **if it builds clean**, else ~200-line hand-rolled JSON-RPC/stdio | codegraph hand-rolled it; only 2 listed tools, so minimal either way. |
| JSON | `serde` / `serde_json` | Config + MCP wire. |
| Parallelism | `rayon` | Parallel per-file parse. |
| Git HEAD | shell out to `git rev-parse HEAD` | Avoids libgit2 native dep; fail-soft to `null` on non-git (matches codescratch). |

**No C/C++/Rust toolchain for the user** — they download a prebuilt static binary. Toolchain is a *build-time, our-CI* concern only.

---

## Module layout (new crate)

```
codegraph/
  src/
    main.rs        clap dispatch
    extract/       tree-sitter TS/JS → symbols + import bindings   (port src/extract/)
    index/         ignore-walk, blake3, incremental dirty+importers (port src/index/)
    resolve/       relative + tsconfig paths + workspace pkgs + barrels
    host/          ensure: single-flight lock, git HEAD, indexed_head meta
    watch/         notify watcher, debounce, scoped sync
    trust/         3-axis compute + render                          (port src/query/trust.ts, format.ts)
    query/         explore (fat), search, callers/callees/impact
    db/            rusqlite schema, migrations, FTS5
    mcp/           JSON-RPC stdio; 2 listed tools + hidden allowlist
    setup/         write MCP config for detected agents
  install.sh                        curl installer → GH Releases binary
  .github/workflows/release.yml     cross-compile matrix (darwin/linux/win × arm64/x64)
```

## Schema (one edge table, honesty fields from day one)

- `files(path PK, hash, mtime_ms, size, language, indexed_at)`
- `nodes(id PK, kind, name, qualified_name, file_path, start_line, end_line, exported, signature)`
- `edges(id PK, src_id, dst_id, kind, raw_name, resolved, conf, reason, provenance, file_path, line)` — `kind` discriminator (GitNexus pattern); `provenance ast|heuristic` present from v0.1 (codegraph idea).
- `bindings(...)` — import binding rows for resolution.
- `meta(key, value)` — `schema_version`, `indexed_head`, `reindex_state`.
- `nodes_fts` — FTS5 over name/qualified_name/file_path, trigger-synced.
- **Later:** `route` / `community` / `process` node kinds; `handles_route` / `member_of` / `step_in` edges. Frameworks are plugins that only emit `route`/`handles_route`; core never knows Next/Prisma/Express.

## The explore payload (one markdown response — the product)

1. **Trust banner** (3 axes) — first line, always. The signature.
2. Target node + **verbatim snippet**, byte-budgeted.
3. **Call-path spine** — named symbols, ≤1 unnamed bridge, heuristic hops labeled.
4. members / calls / callers / imports / bindings / heritage.
5. **Depth-grouped blast radius** — caller files + test files, locations only.
6. *(v0.3+)* routes / processes touching this symbol.

---

## Ship order

**v0.1 — the spine (a single binary that already beats codescratch on weight/install)**
- clap CLI; `ignore` walk + `blake3`; tree-sitter TS/JS extract; rusqlite schema + FTS5.
- resolve: relative + tsconfig paths + workspace pkgs + `export *` barrels; incremental dirty + importers.
- host `ensure` + single-flight lock + git HEAD/`indexed_head` + `reindex_state`.
- 3-axis trust banner.
- MCP stdio: **`explore` + `status` only**; `search`/`callers`/`callees`/`impact` hidden behind `CODEGRAPH_MCP_TOOLS=`.
- `setup` writes MCP config (Claude first; Cursor/Codex next). `install.sh` + GH Releases cross-compile.

**v0.2 — one fat explore + native watcher**
- explore inlines the **call-path spine** + **depth-grouped blast radius**.
- `notify` watcher: debounce, 300ms quick-path, scoped re-resolve of changed files; Claude hooks become a backup, not the only path.

**v0.3 — more helpful**
- heuristic dispatch edges (`provenance=heuristic`, labeled).
- `detect_changes`: git diff → symbols → (later) processes; as an explore section + CLI.
- route plugin: Express + Next app-router (optional extractors; core unchanged).

**v0.4 — broader**
- 2nd language extractor (Python), same schema.
- Leiden communities + process traces folded into explore.
- optional local embeddings; default stays FTS.

**v1.0**
- multi-repo groups; polished curl installer.
- measured harness: fewer tool calls than grep-only on a public TS repo. Publish the harness.

## Hard rules (carried from GOAL.md)

1. New code, new names, own schema. Read parents for ideas; never paste (GitNexus especially — PolyForm-NC).
2. Graph never leaves the machine. **Zero telemetry** (go further than codegraph's opt-out).
3. Host owns freshness; the agent is never taught to reindex every turn.
4. Weak edges stay labeled. A missing caller ≠ "does not exist."
5. An install that needs a user-side toolchain is a bug. Static binary only.

---

## Verification (end-to-end)

- **Unit:** `cargo test` per module — resolve precedence (mirror codescratch's fixtures), trust-axis transitions (fresh→stale on HEAD drift, coverage sampled on budget hit), incremental dirty+importers.
- **Golden repo:** check in a small TS fixture repo; assert explore output = trust banner + spine + blast for a known exported symbol.
- **Real repo, clean-box sim:** on a public TS repo — `codegraph init`, then via an MCP client call `explore <exported fn>`. **Pass:** one call, `trust: fresh`, a spine, a blast, agent opens 0 extra files. **Fail:** "run npm link", "graph stale, reindex", "which of 7 tools?", or a raw file dump.
- **Weight check:** `ls -lh` the release binary (target: <15MB), confirm it runs on a box with no Node, no npm, no compiler.
- **Cost check (v1):** run the published harness; fewer tool calls + fewer tokens than grep-only baseline.
