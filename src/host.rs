//! Host-owned freshness. `ensure` runs under a single-flight lock so readers
//! never see a half-written graph. Ported from codescratch `src/host/`.

use crate::{analysis, db, embeddings, git, index};
use anyhow::{anyhow, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const LOCK_TTL_MS: u128 = 120_000; // steal a lock older than this (crashed writer)

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

/// RAII single-flight lock via `O_EXCL` create. Steals a stale lock.
struct Lock {
    path: PathBuf,
}

impl Lock {
    fn acquire(root: &Path) -> Result<Lock> {
        let path = db::dir(root).join("ensure.lock");
        fs::create_dir_all(db::dir(root))?;
        for _ in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    let _ = writeln!(f, "{}:{}", std::process::id(), now_ms());
                    return Ok(Lock { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Self::is_stale(&path) {
                        let _ = fs::remove_file(&path);
                        continue; // retry once
                    }
                    return Err(anyhow!("ensure already running (lock held)"));
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(anyhow!("could not acquire ensure lock"))
    }

    fn is_stale(path: &Path) -> bool {
        let Ok(body) = fs::read_to_string(path) else {
            return true;
        };
        match body
            .trim()
            .split(':')
            .nth(1)
            .and_then(|t| t.parse::<u128>().ok())
        {
            Some(ts) => now_ms().saturating_sub(ts) > LOCK_TTL_MS,
            None => true,
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Bring the graph up to date under the lock. `reindex_state=rebuilding` is set
/// so concurrent readers report `trust: rebuilding` instead of reading torn data.
/// The dirty-gate inside `index::ensure_current` short-circuits to a no-op when
/// nothing changed (the common SessionStart/PostToolUse case).
pub fn ensure(root: &Path) -> Result<()> {
    run_under_lock(root, false)
}

/// Emergency full rebuild: bypass the dirty-gate and re-parse everything, even
/// when `mtime`+`size` say nothing changed. Same lock as `ensure`.
fn reindex(root: &Path) -> Result<()> {
    run_under_lock(root, true)
}

/// Ensure every root in a group, sequentially (each root has its own lock and
/// its own `.codescratch/`). One failing repo does not abort the rest; all
/// failures are collected and reported together.
pub fn ensure_many(roots: &[std::path::PathBuf], force: bool) -> Result<()> {
    let mut failures = Vec::new();
    for root in roots {
        let r = if force { reindex(root) } else { ensure(root) };
        if let Err(e) = r {
            failures.push(format!("{}: {e}", root.display()));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{} repo(s) failed: {}",
            failures.len(),
            failures.join("; ")
        ))
    }
}

fn run_under_lock(root: &Path, force: bool) -> Result<()> {
    let _lock = Lock::acquire(root)?;
    let mut conn = db::open(root)?;

    db::set_meta(&conn, "reindex_state", "rebuilding")?;

    let result = (|| -> Result<bool> {
        if force {
            index::index_all(&mut conn, root)?;
            Ok(true)
        } else {
            Ok(matches!(
                index::ensure_current(&mut conn, root)?,
                index::IndexOutcome::Wrote
            ))
        }
    })();

    // Always clear the rebuilding flag, even on failure or skip.
    db::set_meta(&conn, "reindex_state", "idle")?;
    let wrote = result?;

    // Heuristic aggregations only after a real write. A skipped dirty-gate must
    // not walk the calls graph or rebuild embeddings.
    if wrote {
        analysis::materialize(&mut conn)?;
        embeddings::materialize(&mut conn)?;
    }

    // HEAD is cheap and must stay current even on skip (a README-only commit
    // would otherwise leave trust:stale forever).
    match git::head(root) {
        Some(h) => db::set_meta(&conn, "indexed_head", &h)?,
        None => {
            let _ = conn.execute("DELETE FROM meta WHERE key = 'indexed_head'", []);
        }
    }
    Ok(())
}
