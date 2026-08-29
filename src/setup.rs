//! Write MCP client config for detected agents. WP-2E.
//! Claude / Cursor / Codex / opencode. Never overwrites an existing
//! codescratch entry; merges into existing mcp json.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub fn run(root: &Path) -> Result<()> {
    let bin = current_bin()?;
    let mut wrote = Vec::new();

    if let Some(p) = claude_config() {
        if merge_mcp(&p, "codescratch", &bin, root)? {
            wrote.push(p);
        }
    }
    if let Some(p) = cursor_config() {
        if merge_mcp(&p, "codescratch", &bin, root)? {
            wrote.push(p);
        }
    }
    if let Some(p) = opencode_config() {
        if merge_mcp(&p, "codescratch", &bin, root)? {
            wrote.push(p);
        }
    }
    // Codex: project-local .codex/mcp.json if the dir exists, else skip
    let codex = root.join(".codex").join("mcp.json");
    if root.join(".codex").is_dir() || std::env::var_os("CODEX_HOME").is_some() {
        let p = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .map(|h| h.join("mcp.json"))
            .unwrap_or(codex);
        if merge_mcp(&p, "codescratch", &bin, root)? {
            wrote.push(p);
        }
    }

    // Always write a project-local .mcp.json so any agent can pick it up.
    let local = root.join(".mcp.json");
    merge_mcp(&local, "codescratch", &bin, root)?;
    wrote.push(local);

    println!("codescratch setup");
    println!("  binary: {}", bin.display());
    println!("  root:   {}", root.display());
    for p in &wrote {
        println!("  wrote:  {}", p.display());
    }
    if wrote.is_empty() {
        println!("  (no agent configs detected; wrote nothing beyond .mcp.json)");
    }
    Ok(())
}

fn current_bin() -> Result<PathBuf> {
    std::env::current_exe().map_err(|e| anyhow!("cannot locate codescratch binary: {e}"))
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn claude_config() -> Option<PathBuf> {
    let p = home().join(".claude").join("mcp.json");
    if home().join(".claude").is_dir() {
        Some(p)
    } else {
        None
    }
}

fn cursor_config() -> Option<PathBuf> {
    let p = home().join(".cursor").join("mcp.json");
    if home().join(".cursor").is_dir() {
        Some(p)
    } else {
        None
    }
}

fn opencode_config() -> Option<PathBuf> {
    let p = home().join(".opencode").join("mcp.json");
    if home().join(".opencode").is_dir() {
        Some(p)
    } else {
        None
    }
}

fn merge_mcp(path: &Path, name: &str, bin: &Path, root: &Path) -> Result<bool> {
    let mut root_obj: Value = if path.exists() {
        let raw = std::fs::read_to_string(path)?;
        serde_json::from_str(&raw).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };
    if !root_obj.is_object() {
        root_obj = json!({});
    }
    let mcp_servers = if root_obj.get("mcpServers").is_some() {
        "mcpServers"
    } else if root_obj.get("mcp").is_some() {
        "mcp"
    } else {
        "mcpServers"
    };
    if root_obj.get(mcp_servers).is_none() {
        root_obj[mcp_servers] = json!({});
    }
    let entry = json!({
        "command": bin.to_string_lossy(),
        "args": ["mcp", root.to_string_lossy().as_ref()],
    });
    root_obj[mcp_servers][name] = entry;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let pretty = serde_json::to_string_pretty(&root_obj)?;
    std::fs::write(path, pretty)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn merge_does_not_clobber_other_servers() {
        let dir = std::env::temp_dir().join(format!("cs-setup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("mcp.json");
        fs::write(&p, r#"{"mcpServers":{"other":{"command":"x"}}}"#).unwrap();
        merge_mcp(&p, "codescratch", Path::new("/bin/codescratch"), Path::new("/repo")).unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert!(v["mcpServers"]["other"].is_object());
        assert_eq!(v["mcpServers"]["codescratch"]["command"], "/bin/codescratch");
        let _ = fs::remove_dir_all(&dir);
    }
}
