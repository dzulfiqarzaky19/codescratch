# codescratch

A local structure Graph of a repo (or a named Group of repos) that an agent reads before grepping identifiers.

## Language

**Graph**:
The indexed structure of one repo: symbols, edges, routes, processes. Lives next to the source.
_Avoid_: knowledge base, MCP, server

**Banner**:
The first line of every payload. Three orthogonal axes: Trust, Coverage, Resolve. Not itself a freshness signal.
_Avoid_: Trust (as the payload), status line, health

**Trust**:
Whether the Graph matches git HEAD. `fresh` | `stale` | `rebuilding` | `missing`.
_Avoid_: health, quality, degraded, resolve

**Coverage**:
How much of the repo was walked. `exhaustive` | `sampled`.
_Avoid_: completeness

**Resolve**:
How much of in-repo call and heritage bound to a symbol. `ok` | `partial`. Not freshness.
_Avoid_: graph (as an axis), degraded, stale

**Ensure**:
Host-owned catch-up that brings Trust to `fresh` under a lock.
_Avoid_: reindex (emergency full rebuild)

**Scope**:
The set of repo roots a command acts on. One repo, or every member of a Group.
_Avoid_: workspace, project, monorepo

**Group**:
A named set of repo roots. Fan-out at query time. Each root keeps its own Graph. No cross-repo edges.
_Avoid_: merged index, monorepo
