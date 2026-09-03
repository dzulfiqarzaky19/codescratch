# Do not join host+index, git.rs, analysis/, or split resolve.rs

Five architecture passes already joined leftover hops (extract internals, init through Scope, leftover `pub`). The next fold that does not cut hops for a real behaviour is waste. Future reviews must not re-suggest the list below unless new friction appears.

## Status

accepted

## Considered Options

- **Join `host.rs` + `index.rs`.** Rejected: lock/freshness and write/parse are two behaviours. A reader changing the dirty-gate should not load the lock; a reader changing the lock should not load extract.
- **Join `git.rs` into its callers.** Rejected: three callers on opposite sides (`host` writes `indexed_head`, `trust` reads HEAD, `changes` diffs). Deletion reappears in three places.
- **Fold `analysis/` into `host`.** Rejected: community/process materialization is a write-side swamp, not freshness. Host already calls `analysis::materialize`; that one call is the seam.
- **Split `resolve.rs`.** Rejected: undoes the pass-2 join. Specifier resolution and honesty live in one place on purpose.
- **Delete `ChangeSpec::Staged` / `Compare`.** Rejected: product flags, not depth. Wiring `--staged` / `--compare` later would just put them back.

Keep `embeddings.rs` as its own file: write (`host` → `materialize`) and read (`query` → `hybrid_search`) sit on opposite sides of a seam.
