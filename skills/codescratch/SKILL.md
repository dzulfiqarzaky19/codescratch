---
name: codescratch
description: TS/JS symbol graph CLI — where a symbol is defined, who calls it, blast radius. codescratch explore|search|changes.
---

```
codescratch explore <Symbol> [--brief]     # source + calls + callers (blast); --brief: no source
codescratch search <name>                  # fuzzy find
codescratch changes                        # git diff → symbols + blast
codescratch status                         # trust × coverage × resolve
codescratch ensure                         # only when the banner says trust: stale
```

`--group NAME` fans out over a group's repos; from the group's parent directory it is implied.

In Claude Code the host does the routing: a grep for symbol names is answered from the graph, and any other large grep result comes back folded, one row per enclosing symbol:
`  START-END kind name ×hits Lline,line: first hit` under its file path. Read that range, not the file. Repeat the same grep once for raw lines.

Banner, three separate axes: `trust:` freshness (`stale` → run `ensure`; never `reindex` unless stuck), `coverage:` how much was walked (`sampled`: absence is not proof), `resolve:` in-repo bind rate (`partial`: some calls unbound; not freshness).
`conf=weak` is a name guess. Auth/money/deletes: read the source anyway. The graph misses `import()`, DI, proxies. `uses ←` lists non-call references (JSX, types, values).
