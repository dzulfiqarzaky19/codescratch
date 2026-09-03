//! Install the global skill + Pi host extension. Strip leftover codescratch
//! MCP entries so an old `lifecycle: eager` config cannot spawn a server
//! we no longer ship.

use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};

const SKILL_MD: &str = include_str!("../skills/codescratch/SKILL.md");
const PI_EXT: &str = include_str!("../host/pi-codescratch.ts");

pub fn run(root: &Path, group: Option<&str>) -> Result<()> {
    let mut wrote = Vec::new();
    let mut stripped = Vec::new();

    wrote.extend(write_skill()?);
    wrote.extend(write_pi_extension()?);

    for p in mcp_candidates(root) {
        if strip_codescratch_mcp(&p)? {
            stripped.push(p);
        }
    }

    println!("codescratch setup");
    println!("  root:   {}", root.display());
    if let Some(g) = group {
        println!("  group:  {g} (validated; CLI auto-detects unique parent cwd)");
    }
    for p in &wrote {
        println!("  wrote:  {}", p.display());
    }
    for p in &stripped {
        println!("  stripped codescratch MCP: {}", p.display());
    }
    if wrote.is_empty() && stripped.is_empty() {
        println!("  (nothing to write)");
    }
    Ok(())
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn write_skill() -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    // Write into a harness that already exists. Create `skills/` if missing,
    // but do not invent ~/.pi on a Claude-only machine (and vice versa).
    let dests = [
        (home().join(".pi").join("agent"), home().join(".pi").join("agent").join("skills").join("codescratch")),
        (home().join(".claude"), home().join(".claude").join("skills").join("codescratch")),
    ];
    for (harness, dir) in dests {
        if !harness.exists() {
            continue;
        }
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("SKILL.md");
        std::fs::write(&path, SKILL_MD)?;
        out.push(path);
    }
    Ok(out)
}

fn write_pi_extension() -> Result<Vec<PathBuf>> {
    let dir = home().join(".pi").join("agent").join("extensions");
    if !dir.exists() && !home().join(".pi").join("agent").exists() {
        return Ok(vec![]);
    }
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("codescratch.ts");
    std::fs::write(&path, PI_EXT)?;
    // drop the old PostToolUse-only ensure hook if it is still sitting there
    let _ = std::fs::remove_file(dir.join("codescratch-ensure.ts"));
    let _ = std::fs::remove_file(dir.join("codescratch-ensure.ts.disabled"));
    Ok(vec![path])
}

fn mcp_candidates(root: &Path) -> Vec<PathBuf> {
    let mut v = vec![
        home().join(".claude").join("mcp.json"),
        home().join(".cursor").join("mcp.json"),
        home().join(".opencode").join("mcp.json"),
        home().join(".pi").join("agent").join("mcp.json"),
        home().join(".config").join("mcp").join("mcp.json"),
        root.join(".mcp.json"),
        root.join(".pi").join("mcp.json"),
        root.join(".codex").join("mcp.json"),
    ];
    if let Ok(codex) = std::env::var("CODEX_HOME") {
        v.push(PathBuf::from(codex).join("mcp.json"));
    }
    // walk one level of children so a workspace parent (e.g. /kabana) strips
    // leftover eager servers in kabana-app/.pi/mcp.json etc.
    if let Ok(rd) = std::fs::read_dir(root) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                v.push(p.join(".pi").join("mcp.json"));
                v.push(p.join(".mcp.json"));
            }
        }
    }
    v
}

/// Remove the `codescratch` key under `mcpServers` or `mcp`. Leaves other
/// servers alone. Missing / unreadable file → no-op. Returns true if a write happened.
fn strip_codescratch_mcp(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Ok(false),
    };
    let mut root_obj: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    if !root_obj.is_object() {
        return Ok(false);
    }
    let mut changed = false;
    for key in ["mcpServers", "mcp"] {
        if let Some(servers) = root_obj.get_mut(key).and_then(|v| v.as_object_mut()) {
            if servers.remove("codescratch").is_some() {
                changed = true;
            }
        }
    }
    if !changed {
        return Ok(false);
    }
    let pretty = serde_json::to_string_pretty(&root_obj)?;
    std::fs::write(path, pretty)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cs-setup-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn strip_removes_codescratch_keeps_others() {
        let dir = tmp("strip");
        let p = dir.join("mcp.json");
        fs::write(
            &p,
            r#"{"mcpServers":{"other":{"command":"x"},"codescratch":{"command":"cs"}}}"#,
        )
        .unwrap();
        assert!(strip_codescratch_mcp(&p).unwrap());
        let v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert!(v["mcpServers"]["other"].is_object());
        assert!(v["mcpServers"].get("codescratch").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_noop_when_absent() {
        let dir = tmp("absent");
        let p = dir.join("mcp.json");
        fs::write(&p, r#"{"mcpServers":{"other":{"command":"x"}}}"#).unwrap();
        assert!(!strip_codescratch_mcp(&p).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_missing_file_is_ok() {
        let dir = tmp("missing");
        assert!(!strip_codescratch_mcp(&dir.join("nope.json")).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_empty_object_ok() {
        let dir = tmp("empty");
        let p = dir.join("mcp.json");
        fs::write(&p, "{}").unwrap();
        assert!(!strip_codescratch_mcp(&p).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn include_skill_is_nonempty() {
        assert!(SKILL_MD.contains("codescratch explore"));
        assert!(PI_EXT.contains("session_start"));
    }
}
