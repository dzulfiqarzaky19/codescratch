# codescratch — goal

Portable agent code-brain. Install once. Drop on any machine, any repo, any agent. Graph lives in the repo. Agent pays less, gets more.

This file is the north star. `README.md` / `CLAUDE.md` describe **today**. This file describes **where**.

Textbooks (ideas only, never copy):

- `/home/dzulfiqarzaky/projects/not mine/codegraph` — MIT, live SQLite, one fat explore, watcher
- `/home/dzulfiqarzaky/projects/not mine/GitNexus` — PolyForm NC, 19-phase precompute, impact/process/route

Archify maps of those two sit beside the clones (`codegraph-archify/`, `GitNexus-archify/`).

## Win condition

A stranger clones a random repo on a clean box.

```
<one install command>
cd their-repo
codescratch init && codescratch setup
# agent has the skill; CLI is the surface
```

Agent asks: “what breaks if I change X?”

First call is enough. No grep crawl. No 17-tool menu. Cost down. Answer is structure, not a file dump.

| Must | Means |
|---|---|
| Easy install | One command. No C++ toolchain. No Node-engine lottery. Bundled runtime **or** `npx` that just works. |
| Move it | Copy the binary + `<repo>/.codescratch/`. New PC, new agent, new folder. Same commands. No re-wire ritual. |
| Cheap for the agent | CLI + skill. Zero standing MCP process. One explore beats 20 greps. |
| More helpful | Payload is path + blast + (later) process/route — not source the model still has to assemble. |
| Honest | Freshness, coverage, resolution quality stay visible. Weak edges stay labeled. Absence ≠ proof. |

Not the win: pretty UI, wiki, cloud, MCP, kabana-only glue, forking GitNexus.

## Already true (keep)

codescratch `0.1.0` is this product, not a prototype of a different one.

- MIT. SQLite under `<repo>/.codescratch/graph.db`. `node:sqlite`, no native add-on.
- Host-owned freshness: `ensure` + lock + Claude Code hooks. Agent does **not** reindex every turn.
- Trust is three axes: `trust` (fresh/stale/rebuilding/missing) × `coverage` (exhaustive/sampled) × `graph` (ok/degraded).
- Edges carry `reason=` and `strong`/`weak`. `receiver-unknown` is navigational only.
- TS/JS resolve: relative, `tsconfig` paths, workspace packages, `export *` barrels. Incremental dirty + importers.
- CLI already exists. Agent surface is a skill, not MCP.

Do not throw this away for a CodeGraph clone. The trust banner is the thing neither parent does well.

## Steal (ideas, rewrite)

From **CodeGraph**:

- Default CLI surface = **explore**. `search` / `status` / `changes` stay CLI. Skill teaches the agent to pick them.
- Native FS watcher as the host path, not only editor hooks. Debounce. Dirty-file re-resolve. Graph never waits on SessionStart.
- Heuristic dispatch edges (callbacks, events, `setState`→render) with `provenance: heuristic` — never pretend they are AST.
- Install that a non-Node user can run. curl script **or** `npx` with wasm already in the tarball (today `postinstall` only *copies* wasm from the bundled `tree-sitter-wasms` dep — no network — but `scripts/` isn't in package.json `files`, so the published tarball ships a postinstall that can't find its own script).
- Bundled runtime later. v1 can stay Node ≥22.5 if `npx codescratch` is the whole install.

From **GitNexus** (behavior, not code, not license):

- Index-time structure so one call is complete: blast grouped by depth, later communities / processes / routes.
- `detect_changes` on git diff → affected symbols, as a **section of explore** (and a CLI), not a 12th tool.
- Hybrid search later: FTS (have it) + optional local embeddings + RRF. Default stays FTS.
- Multi-repo groups as v2: named set of `.codescratch/` indexes. Core stays one-repo.

From **neither**: wiki, Ladybug/Kuzu, PolyForm, Scarf, Spring/COBOL as v1, uploading source.

## Gap vs goal (today)

| Goal | Today | Gap |
|---|---|---|
| One-command install | `npm i && npm run build && npm link`, wasm fetch in postinstall | Fail on clean box / offline. No curl installer. Node 22.5 required and undocumented for agents. |
| Move to another machine | Graph is portable; **tooling is not**. Skill + binary path. Claude-only hooks. | `setup` writes the skill everywhere. Cursor, Codex, OpenCode, Copilot, pi. Same `init`. |
| Cheap for agent | MCP was 2 listed tools + schema tax + eager-process landmine | CLI + skill. Zero standing process. |
| More helpful | explore = snippet + members + calls + callers + imports + bindings | No call-path spine, no depth-grouped blast, no routes, no processes. Agent still assembles. |
| Live anywhere | Host = Claude hooks | No native watcher. No Codex/Cursor hooks. Checkout on a machine without those hooks → stale until someone runs `ensure`. |
| Any language | TS/JS extractor only | Extractor interface exists in spirit (`src/extract/`) — plug Python/Go later. Do not bake Next/Prisma into core. |
| Honest on huge repos | Trust axes exist; callers not exhaustive | Keep. Never advertise “complete graph”. |

## CLI surface (target)

| Command | Job |
|---|---|
| `codescratch status` | Trust banner. Call when unsure the graph exists. |
| `codescratch explore` | One question → verbatim spine + path + blast (+ later process/route). |
| `codescratch search` | Fuzzy find. |
| `codescratch changes` | Git diff → symbols + blast. |

Every explore payload starts with the three-axis banner. Weak edges stay marked. That is the codescratch signature — CodeGraph dumps source; GitNexus dumps structure; we dump **structure with a lie detector**.

## Schema (yours, not theirs)

Keep current nodes/edges. Add only when a phase needs them:

- Nodes: `file`, `function`, `class`, `route`, `community`, `process` (last three = later)
- Edges: `contains`, `calls`, `imports`, `extends`, `implements`, `handles_route`, `member_of`, `step_in`
- On every resolved edge: `reason`, `conf`, `provenance: ast \| heuristic`

Languages = extractors. Frameworks = plugins that only emit `route` / `handles_route`. Core never knows kabana, Next, or Prisma.

## Ship order

**v0.2 — portable + cheap**

- One-command install: `npx codescratch` works; wasm in the published tarball; no postinstall network.
- `codescratch setup` writes the global skill + Pi host extension; strips leftover MCP.
- Explore inlines callers/callees/impact.
- Native FS watcher in `ensure` (hooks become a backup, not the only path).

**v0.3 — more helpful**

- Explore returns a call-path spine (named symbols, ≤1 unnamed bridge) + depth-grouped blast.
- `detect_changes` CLI + explore section (unstaged / staged / compare).
- Route plugin: Express + Next app router as optional extractors. Core unchanged.

**v0.4 — broader**

- Second language extractor (Python). Same schema.
- Leiden communities + process traces folded into explore.
- Optional local embeddings. Default still FTS.

**v1.0**

- curl installer / bundled runtime.
- Multi-repo groups.
- Measured: fewer tool calls than grep-only on a public TS repo. Publish the harness, not vibes.

## Hard rules

1. New code, new names, own schema. Read the textbooks. Do not paste GitNexus or CodeGraph.
2. Graph never leaves the machine. No repo name, path, symbol, or query in any telemetry. Prefer none.
3. Host owns freshness. Agent must not be taught to reindex every turn.
4. Weak is labeled. A missing caller is not “does not exist”.
5. Install that needs a C++ toolchain is a bug.
6. Kabana is a customer later. Not a feature.

## Test the goal

```bash
# clean machine, random public TS repo
npx codescratch setup
cd some-repo && npx codescratch init
# in any supported agent:
# “what breaks if I change <one exported function>?”
```

Pass: one explore, trust:fresh, a path, a blast, agent does not open 10 files to find callers.

Fail: “run npm link first”, “graph stale, please reindex”, “which of these 7 tools?”, “here are 12 file dumps, you figure it out”.
