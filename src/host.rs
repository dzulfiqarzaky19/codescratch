//! Host-owned freshness. `ensure` runs under a single-flight lock so readers
//! never see a half-written graph. Ported from codescratch `src/host/`.

use crate::{analysis, db, embeddings, index};
use anyhow::{anyhow, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const LOCK_TTL_MS: u128 = 120_000; // steal a lock older than this (crashed writer)

/// Current git HEAD, or None on a non-git tree. Shells out to avoid libgit2.
pub fn git_head(root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()
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
        match body.trim().split(':').nth(1).and_then(|t| t.parse::<u128>().ok()) {
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
/// The dirty-gate inside `index_incremental` short-circuits to a no-op when
/// nothing changed (the common SessionStart/PostToolUse case).
pub fn ensure(root: &Path) -> Result<()> {
    run_under_lock(root, false)
}

/// Emergency full rebuild: bypass the dirty-gate and re-parse everything, even
/// when `mtime`+`size` say nothing changed. Same lock as `ensure`.
pub fn reindex(root: &Path) -> Result<()> {
    run_under_lock(root, true)
}

fn run_under_lock(root: &Path, force: bool) -> Result<()> {
    let _lock = Lock::acquire(root)?;
    let mut conn = db::open(root)?;

    db::set_meta(&conn, "reindex_state", "rebuilding")?;

    let result = if force {
        index::index_all(&mut conn, root)
    } else {
        index::index_incremental(&mut conn, root, &[])
    };

    // Always clear the rebuilding flag, even on failure.
    db::set_meta(&conn, "reindex_state", "idle")?;
    result?;

    // Heuristic aggregations over the freshly-written graph. Both are idempotent
    // and cheap; a full reindex wipes `nodes`, so these are the only thing that
    // repopulates community/process nodes + embeddings afterward.
    analysis::materialize(&mut conn)?;
    embeddings::materialize(&mut conn)?;

    match git_head(root) {
        Some(h) => db::set_meta(&conn, "indexed_head", &h)?,
        None => {
            let _ = conn.execute("DELETE FROM meta WHERE key = 'indexed_head'", []);
        }
    }
    Ok(())
}
