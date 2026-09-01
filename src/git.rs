//! Git spawn. One adapter so host freshness and detect_changes share fail-soft.

use anyhow::Result;
use std::path::Path;

/// Current HEAD, or None on a non-git tree / failed spawn. Shells out to
/// avoid libgit2.
pub fn head(root: &Path) -> Option<String> {
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

/// `git diff --unified=0` plus extra args (`--cached`, a ref, …).
pub fn diff(root: &Path, extra: &[&str]) -> Result<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(root).arg("diff").arg("--unified=0");
    for a in extra {
        cmd.arg(a);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("git diff failed: {}", stderr.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn tmp_repo() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "cs-git-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&d)
            .args(["init", "-q"])
            .status()
            .unwrap();
        std::fs::write(d.join("a.txt"), "one\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&d)
            .args(["-c", "user.email=t@t", "-c", "user.name=t", "add", "a.txt"])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&d)
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "init",
            ])
            .status()
            .unwrap();
        d
    }

    #[test]
    fn head_is_some_on_a_git_repo_none_elsewhere() {
        let d = tmp_repo();
        let h = head(&d);
        assert!(h.as_ref().map(|s| s.len() == 40).unwrap_or(false), "{h:?}");
        let nowhere = std::env::temp_dir().join(format!(
            "cs-git-nogit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&nowhere);
        std::fs::create_dir_all(&nowhere).unwrap();
        assert!(head(&nowhere).is_none());
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&nowhere);
    }

    #[test]
    fn diff_empty_on_clean_tree() {
        let d = tmp_repo();
        let out = diff(&d, &[]).unwrap();
        assert!(out.is_empty(), "{out}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
