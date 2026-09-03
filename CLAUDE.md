# codescratch

Local TS/JS code structure graph for AI agents. SQLite under `<repo>/.codescratch/`. CLI + skill. No MCP.

## Hybrid freshness (host-owned)

Graph currency is **host** work (`codescratch ensure` + Claude Code hooks), not the model.

| Layer | Owner |
|-------|--------|
| Index freshness | Host: `codescratch watch` (session) + SessionStart `ensure` (catch-up) |
| Deep structure | Agent CLI: `codescratch explore` / `search` / `changes` |
| Emergency rebuild | `codescratch reindex` only if host path down or trust stuck |

### Agent protocol

1. Prefer `codescratch explore` / `search` over blind grep for “where defined / who calls / blast radius”.
2. Read all three axes every reply — `trust:` (freshness), `coverage:` (how much was verified), `graph:` (resolution quality).
   - `rebuilding` → host job in flight; **absence ≠ proof**; do not spam reindex.
   - `stale` → host ensure should catch up; `cs_reindex` only if stuck.
   - `coverage: sampled` / `graph: degraded` / `conf=weak` / `fresh but …` → do not treat absence as proof.
3. **Do not** call `cs_reindex` after every edit or every turn.
4. Critical paths (auth, money, deletes): verify by reading source. Graph misses dynamic `import()`, DI, proxies.

### Tools

| Tool | When |
|------|------|
| `codescratch status` | Graph missing/stale/rebuilding? |
| `codescratch explore` | One symbol: snippet + spine + members + calls + callers + routes/processes. Miss inlines nearby search hits. |
| `codescratch search` | Fuzzy find. |
| `codescratch reindex` | **Emergency** only (same lock as host ensure) |

Optional path arg / `CODESCRATCH_ROOT` / cwd. Cwd that is the unique parent of a registered group fans out.

### Multi-repo groups

A group is a named set of repo roots (`~/.codescratch/groups.json`). Each repo keeps its own `.codescratch/` index and its own lock; a group is a **fan-out at query time**, not a merged index.

```
codescratch group add --group pay --root ~/src/api
codescratch group add --group pay --root ~/src/web
codescratch ensure  --group pay      # indexes each member, sequential
codescratch status  --group pay      # merged banner + one line per repo
codescratch search  helper --group pay
codescratch explore chargeCard --group pay
codescratch setup   --group pay      # skill is global; validates group exists
```

`--group` on any command, or `CODESCRATCH_GROUP=pay` to pin it.

`--group` works on `ensure`, `reindex`, `status`, `search`, `explore`, `changes`, `watch`, `setup`.

- Group banner = worst-wins per axis + summed counts, suffixed `[group: N repos]`. One `stale` member makes the group `stale`.
- `search --group` prefixes hits `[repo]`. Ranking is per-repo; scores across separate indexes are not comparable.
- `explore --group` returns a union of per-repo payloads under `# repo \`name\``, plus `not found in: …`.
- **No cross-repo edges.** Each index resolves inside its own root, so an `api → web` call is invisible. Same-named symbols in two repos are two answers, not one.
- A dead/unreadable member is reported inline (`unavailable`) and never hides the rest.
- Single-root output is unchanged: the group form only kicks in with >1 root.

### Trust / confidence

- `strong` — a binding or lexical fact. **Never a type check.**
- `weak` — a name guess. Verify before acting.
- `rebuilding` — host ensure holds the lock; wait.
- Unresolved package imports stay open
- Axes are orthogonal: `graph: degraded` is normal with external deps and says nothing about freshness; `coverage: sampled` means unread files, not drift

Every resolved edge carries `reason=`:

| reason | conf | means |
|--------|------|-------|
| `same-file` | strong | unique non-method symbol in the same file |
| `import-binding` | strong | followed the import/re-export chain |
| `namespace-member` | strong | `NS.x` where `NS` is a namespace import |
| `this-member` | strong | `this.x` → method of the enclosing class |
| `unique-global` | weak | only one symbol repo-wide has that name |
| `receiver-unknown` | weak | `recv.x` with no type info — **navigational only** |

`receiver-unknown` is the common case on OO code: the target is whatever unique symbol shares the method name, which may be the wrong class. Read the source before trusting it.

### Resolve (v0.1+)

- Relative + `tsconfig` paths + workspace package names
- Dirty incremental: rebind dirty + importers; orphaned edges swept
- Host `ensure`: single-flight lock, pending coalesce, `indexed_head` meta (HEAD change → one incremental job, not full thrash)
- Stale detection: stat-all mtime+size gate → hash suspects first, stop on first drift; quiet files hashed within a byte budget; HEAD drift → stale

### Extractor misses (v1)

- dynamic `import()` / `require()`
- DI / proxies / reflection
- object-literal methods only partial
- JSX component identity syntactic only

## Layout

Single Rust crate (`Cargo.toml` at repo root; the original TS port has been removed).

```
src/
  extract/     tree-sitter TS/JS/Python + import bindings; FileFacts includes heritage, heuristic edges, routes
  index.rs     walk, hash, store; resolve owns specifier→file + edge honesty
  host.rs      ensure/reindex lock + git HEAD (host freshness)
  scope.rs     Scope: the roots a command acts on; owns the one-vs-many rule
  query.rs     explore/search/status (gather + markdown adapter live here)
  analysis/    communities (label propagation) + processes (call chains)
  embeddings.rs  local feature-hash embeddings + RRF hybrid search
  group.rs     multi-repo groups (~/.codescratch/groups.json)
  db.rs        rusqlite graph (bundled build → static, FTS5)
  main.rs      CLI (init | ensure | reindex | status | explore | search | setup | watch | changes | group)
tests/         golden.sh (e2e) + bench.sh (perf harness)
```

## Dev

```
cargo test
cargo build
cargo build --release
./target/debug/codescratch ensure <path>
```

## Agent skills

### Issue tracker

GitHub Issues on `dzulfiqarzaky19/codescratch` via `gh`. See `docs/agents/issue-tracker.md`.

### Triage labels

Canonical names as-is: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
