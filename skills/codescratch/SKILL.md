---
name: codescratch
description: TS/JS structure graph. Prefer over grep/rg for where a symbol is defined, who calls it, or blast radius. CLI only — codescratch explore|search|status|changes.
---

Graph lives in `<repo>/.codescratch/graph.db`. Host keeps it fresh (`ensure` / `watch`). Do not reindex every turn.

```
codescratch status                         # trust × coverage × graph
codescratch explore <Symbol>               # snippet + calls + callers (blast)
codescratch search <name>                  # fuzzy find
codescratch changes                        # git diff → symbols + blast
codescratch ensure                         # catch-up if banner says stale
```

`--group NAME` fans out over that group's repos (each keeps its own db). Omit it: cwd is one repo, unless cwd is the unique parent of a registered group (e.g. `/kabana` → group `kabana`).

Read the `trust:` banner on every answer. `rebuilding` / `stale` / `coverage: sampled` / `graph: degraded` / `conf=weak` → absence is not proof. Auth/money/deletes: read source anyway. Graph misses `import()`, DI, proxies.

Never `reindex` unless trust is stuck. `rg`/`grep` for a single identifier (`Foo`) is a miss — `explore Foo` first. Strings, regex, `TODO`, path filters: grep is fine.
