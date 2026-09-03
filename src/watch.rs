//! Native recursive filesystem watcher (WP-2C). Coalesces a burst of fs
//! events into a debounced `host::ensure`, so an interactive session doesn't
//! pay a full `ensure` per keystroke-adjacent save.

use crate::scope::Scope;
use crate::{host, walk};
use anyhow::{Context, Result};
use ignore::gitignore::Gitignore;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
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

fn relevant(rel: &str) -> bool {
    walk::tracked(rel).is_some()
}

/// Root `.gitignore` only — matches WalkBuilder's default skip of `node_modules`
/// (and whatever else the repo ignores) so a `npm i` storm does not enqueue.
fn gitignore_of(root: &Path) -> Gitignore {
    let (gi, _) = Gitignore::new(root.join(".gitignore"));
    gi
}

fn ignored(gi: &Gitignore, rel: &str) -> bool {
    gi.matched_path_or_any_parents(rel, false).is_ignore()
}

/// Convert an absolute event path to a repo-relative, forward-slash path.
/// Returns `None` for paths outside `root` (shouldn't happen for a recursive
/// watch rooted at `root`, but events can race a rename/move).
fn to_repo_relative(root: &Path, abs: &Path) -> Option<String> {
    let rel = abs.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

/// Which repo in scope an absolute event path belongs to, and its repo-relative
/// path. Returns `None` when the path is under no watched root, or is a file we
/// don't track. With one root this is the old single-repo behaviour.
fn owning_root<'a>(roots: &'a [PathBuf], abs: &Path) -> Option<(&'a PathBuf, String)> {
    roots.iter().find_map(|root| {
        let rel = to_repo_relative(root, abs)?;
        relevant(&rel).then_some((root, rel))
    })
}

/// Watch every repo in `scope` recursively and keep each graph fresh via
/// debounced `host::ensure` calls. Pending edits are tracked per repo, so a
/// burst in one repo never triggers an `ensure` in a quiet sibling. Runs until
/// interrupted (Ctrl-C) or the watcher channel closes.
pub fn run(scope: &Scope) -> Result<()> {
    let roots = scope.roots();
    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();

    // `mpsc::Sender<notify::Result<Event>>` implements `EventHandler`
    // directly, so it can be handed to the watcher as-is.
    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(tx).context("failed to create filesystem watcher")?;

    let ignores: Vec<Gitignore> = roots.iter().map(|r| gitignore_of(r)).collect();

    for root in roots {
        watcher
            .watch(root, RecursiveMode::Recursive)
            .with_context(|| format!("failed to watch {}", root.display()))?;
        eprintln!("watching {} for changes (ctrl-c to stop)", root.display());
    }

    // Per-repo pending sets: repo index → changed repo-relative paths.
    let mut pending: Vec<HashSet<String>> = vec![HashSet::new(); roots.len()];
    let mut first_pending_at: Vec<Option<Instant>> = vec![None; roots.len()];

    loop {
        match rx.recv_timeout(POLL) {
            Ok(Ok(event)) => {
                for abs in &event.paths {
                    let Some((root, rel)) = owning_root(roots, abs) else {
                        continue;
                    };
                    let i = roots.iter().position(|r| r == root).unwrap_or(0);
                    if ignored(&ignores[i], &rel) {
                        continue;
                    }
                    if pending[i].insert(rel) && first_pending_at[i].is_none() {
                        first_pending_at[i] = Some(Instant::now());
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

        for (i, root) in roots.iter().enumerate() {
            let Some(first) = first_pending_at[i] else {
                continue;
            };
            if !should_flush(pending[i].len(), first.elapsed()) {
                continue;
            }
            let n = pending[i].len();
            let where_ = if roots.len() > 1 {
                format!(" in {}", crate::trust::repo_label(root))
            } else {
                String::new()
            };
            eprintln!(
                "\u{21bb} {n} file{} changed{where_} \u{2192} ensure",
                if n == 1 { "" } else { "s" }
            );
            pending[i].clear();
            first_pending_at[i] = None;
            if let Err(e) = host::ensure(root) {
                eprintln!("ensure failed: {e}");
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

    #[test]
    fn owning_root_picks_the_repo_containing_the_path() {
        let roots = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let (root, rel) = owning_root(&roots, Path::new("/b/src/x.ts")).unwrap();
        assert_eq!(root, &PathBuf::from("/b"));
        assert_eq!(rel, "src/x.ts");
    }

    #[test]
    fn owning_root_none_for_untracked_or_outside_paths() {
        let roots = vec![PathBuf::from("/a")];
        assert!(owning_root(&roots, Path::new("/elsewhere/x.ts")).is_none());
        assert!(owning_root(&roots, Path::new("/a/README.md")).is_none());
        assert!(owning_root(&roots, Path::new("/a/.git/HEAD")).is_none());
    }

    #[test]
    fn gitignore_skips_node_modules() {
        let dir = std::env::temp_dir().join(format!("cs-watch-gi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".gitignore"), "node_modules\n").unwrap();
        let gi = gitignore_of(&dir);
        assert!(ignored(&gi, "node_modules/left-pad/index.js"));
        assert!(!ignored(&gi, "src/main.ts"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
