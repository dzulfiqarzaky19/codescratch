//! Native recursive filesystem watcher (WP-2C). Coalesces a burst of fs
//! events into a debounced `host::ensure`, so an interactive session doesn't
//! pay a full `ensure` per keystroke-adjacent save.

use crate::{host, model};
use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Full debounce: keep coalescing while events keep arriving, up to this long
/// since the *first* pending change, before forcing a flush.
const DEBOUNCE: Duration = Duration::from_millis(2_000);
/// Quick-path: a small edit (≤ QUICK_PATH_MAX files) flushes sooner, so a
/// single-file save doesn't sit behind the full debounce window.
const QUICK_PATH: Duration = Duration::from_millis(300);
const QUICK_PATH_MAX_FILES: usize = 2;
/// How often to poll the pending set while waiting for it to settle.
const POLL: Duration = Duration::from_millis(100);

/// Pure decision: given how long a pending set has been accumulating and how
/// many files are in it, should we flush now? Factored out for unit testing.
fn should_flush(pending_count: usize, elapsed_since_first: Duration) -> bool {
    if pending_count == 0 {
        return false;
    }
    if pending_count <= QUICK_PATH_MAX_FILES && elapsed_since_first >= QUICK_PATH {
        return true;
    }
    elapsed_since_first >= DEBOUNCE
}

/// True if a repo-relative path should be tracked: not under `.codescratch/`
/// or `.git/`, and recognized as a source language.
fn relevant(rel: &str) -> bool {
    if rel.starts_with(".codescratch/") || rel.starts_with(".git/") {
        return false;
    }
    model::Lang::from_path(rel).is_some()
}

/// Convert an absolute event path to a repo-relative, forward-slash path.
/// Returns `None` for paths outside `root` (shouldn't happen for a recursive
/// watch rooted at `root`, but events can race a rename/move).
fn to_repo_relative(root: &Path, abs: &Path) -> Option<String> {
    let rel = abs.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

/// Watch `root` recursively and keep the graph fresh via debounced
/// `host::ensure` calls. Runs until interrupted (Ctrl-C) or the watcher
/// channel closes.
pub fn run(root: &Path) -> Result<()> {
    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();

    // `mpsc::Sender<notify::Result<Event>>` implements `EventHandler`
    // directly, so it can be handed to the watcher as-is.
    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(tx).context("failed to create filesystem watcher")?;

    watcher
        .watch(root, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", root.display()))?;

    eprintln!("watching {} for changes (ctrl-c to stop)", root.display());

    let mut pending: HashSet<String> = HashSet::new();
    let mut first_pending_at: Option<Instant> = None;

    loop {
        match rx.recv_timeout(POLL) {
            Ok(Ok(event)) => {
                for abs in &event.paths {
                    let Some(rel) = to_repo_relative(root, abs) else { continue };
                    if !relevant(&rel) {
                        continue;
                    }
                    if pending.insert(rel) && first_pending_at.is_none() {
                        first_pending_at = Some(Instant::now());
                    }
                }
            }
            Ok(Err(e)) => {
                eprintln!("watch error: {e}");
            }
            Err(RecvTimeoutError::Timeout) => {
                // fall through to the flush check below
            }
            Err(RecvTimeoutError::Disconnected) => {
                eprintln!("watcher channel closed, stopping");
                return Ok(());
            }
        }

        if let Some(first) = first_pending_at {
            if should_flush(pending.len(), first.elapsed()) {
                let n = pending.len();
                eprintln!("\u{21bb} {n} file{} changed \u{2192} ensure", if n == 1 { "" } else { "s" });
                pending.clear();
                first_pending_at = None;
                if let Err(e) = host::ensure(root) {
                    eprintln!("ensure failed: {e}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_flush_when_nothing_pending() {
        assert!(!should_flush(0, Duration::from_secs(10)));
    }

    #[test]
    fn quick_path_flushes_small_batch_after_300ms() {
        assert!(!should_flush(1, Duration::from_millis(299)));
        assert!(should_flush(1, Duration::from_millis(300)));
        assert!(should_flush(2, Duration::from_millis(300)));
    }

    #[test]
    fn large_batch_waits_for_full_debounce() {
        // 3 files exceeds the quick-path threshold, so it must wait out the
        // full debounce window even though quick-path time has passed.
        assert!(!should_flush(3, Duration::from_millis(300)));
        assert!(!should_flush(3, Duration::from_millis(1_999)));
        assert!(should_flush(3, Duration::from_millis(2_000)));
    }

    #[test]
    fn quick_path_boundary_is_exactly_two_files() {
        assert!(should_flush(QUICK_PATH_MAX_FILES, QUICK_PATH));
        assert!(!should_flush(QUICK_PATH_MAX_FILES + 1, QUICK_PATH));
    }

    #[test]
    fn relevant_ignores_codescratch_and_git_dirs() {
        assert!(!relevant(".codescratch/graph.db"));
        assert!(!relevant(".git/HEAD"));
        assert!(relevant("src/main.ts"));
    }

    #[test]
    fn relevant_ignores_unrecognized_languages() {
        assert!(!relevant("README.md"));
        assert!(!relevant("Cargo.lock"));
        assert!(relevant("src/app.py"));
    }

    #[test]
    fn to_repo_relative_strips_root_and_normalizes() {
        let root = Path::new("/repo");
        let abs = Path::new("/repo/src/main.ts");
        assert_eq!(to_repo_relative(root, abs), Some("src/main.ts".to_string()));
    }

    #[test]
    fn to_repo_relative_none_outside_root() {
        let root = Path::new("/repo");
        let abs = Path::new("/other/main.ts");
        assert_eq!(to_repo_relative(root, abs), None);
    }
}
