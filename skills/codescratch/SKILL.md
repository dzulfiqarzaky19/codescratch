---
name: codescratch
description: TS/JS structure graph. Prefer over grep/rg for where a symbol is defined, who calls it, or blast radius. CLI only — codescratch explore|search|status|changes.
---

Graph lives in `<repo>/.codescratch/graph.db`. Host keeps it fresh (`ensure` / `watch`). Do not reindex every turn.

```
codescratch status                         # trust × coverage × resolve
codescratch explore <Symbol>               # snippet + calls + callers (blast)
codescratch search <name>                  # fuzzy find
codescratch changes                        # git diff → symbols + blast
codescratch ensure                         # catch-up if banner says trust: stale
```

`--group NAME` fans out over that group's repos (each keeps its own db). Omit it: cwd is one repo, unless cwd is the unique parent of a registered group (e.g. `/kabana` → group `kabana`).

Read the banner on every answer. Three axes, do not mix them:
- `trust:` freshness only (`fresh` / `stale` / `rebuilding` / `missing`). `stale` = HEAD moved since last `ensure`. Run `ensure`. Never treat `resolve:` as stale.
- `coverage:` how much was walked (`exhaustive` / `sampled`).
- `resolve:` in-repo bind rate (`ok` / `partial`). Weak / unbound calls. Not freshness.
`conf=weak` on an edge is a name guess. Auth/money/deletes: read source anyway. Graph misses `import()`, DI, proxies.

Never `reindex` unless trust is stuck. `rg`/`grep` for a single identifier (`Foo`) is a miss — `explore Foo` first. Strings, regex, `TODO`, path filters: grep is fine.
