//! codescratch — local code structure graph for AI agents.
//! Single static binary: no Node, no npm, no wasm sidecar, no user-side toolchain.

mod analysis;
mod blast;
mod changes;
mod db;
mod embeddings;
mod extract;
mod git;
mod group;
mod host;
mod index;
mod model;
mod query;
mod resolve;
mod scope;
mod setup;
mod trust;
mod walk;
mod watch;

use anyhow::Result;
use clap::{Parser, Subcommand};
use scope::Scope;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "codescratch",
    version,
    about = "Local code structure graph for AI agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create the graph store and build it once.
    Init { path: Option<PathBuf> },
    /// Bring the graph up to date (host-owned freshness, single-flight).
    Ensure {
        path: Option<PathBuf>,
        /// Ensure every repo in this group instead of one root.
        #[arg(long)]
        group: Option<String>,
    },
    /// Force a full rebuild (emergency; same lock as ensure).
    Reindex {
        path: Option<PathBuf>,
        #[arg(long)]
        group: Option<String>,
    },
    /// Print the trust banner.
    Status {
        path: Option<PathBuf>,
        /// Merged (worst-wins) banner across a group's repos.
        #[arg(long)]
        group: Option<String>,
    },
    /// One symbol → banner + source + calls + callers (blast radius).
    Explore {
        symbol: String,
        #[arg(long)]
        path: Option<PathBuf>,
        /// Explore across every repo in this group.
        #[arg(long)]
        group: Option<String>,
    },
    /// Fuzzy find a symbol by name.
    Search {
        query: String,
        #[arg(long)]
        path: Option<PathBuf>,
        /// Search every repo in this group.
        #[arg(long)]
        group: Option<String>,
    },
    /// Write the global skill + Pi host extension; strip leftover MCP entries.
    Setup {
        path: Option<PathBuf>,
        /// Validate this group exists (skill itself is global, not pinned).
        #[arg(long)]
        group: Option<String>,
    },
    /// Watch the repo and keep the graph fresh (WP-2C).
    Watch {
        path: Option<PathBuf>,
        /// Watch every repo in this group.
        #[arg(long)]
        group: Option<String>,
    },
    /// Show symbols changed vs git diff + their blast (WP-3B).
    Changes {
        path: Option<PathBuf>,
        /// Report changes across every repo in this group.
        #[arg(long)]
        group: Option<String>,
    },
    /// Manage multi-repo groups (registry at ~/.codescratch/groups.json) (WP-10A).
    Group {
        /// list | add | remove | rm-group | roots
        action: String,
        #[arg(long)]
        group: Option<String>,
        #[arg(long)]
        root: Option<PathBuf>,
    },
}

fn root_of(path: Option<PathBuf>) -> PathBuf {
    let raw = path
        .or_else(|| std::env::var_os("CODESCRATCH_ROOT").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::canonicalize(&raw).unwrap_or(raw)
}

/// The scope a command acts on: a group's members (`--group` or
/// `CODESCRATCH_GROUP`), else cwd if it is the unique parent of one group,
/// else the single resolved root.
fn scope_of(group: Option<String>, path: Option<PathBuf>) -> Result<Scope> {
    Scope::resolve(group.as_deref(), &root_of(path))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { path } => {
            let scope = scope_of(None, path.clone())?;
            scope.ensure(false)?;
            println!("{}", scope.status()?);
            let root = &scope.roots()[0];
            println!("graph ready at {}/.codescratch/graph.db", root.display());
        }
        Command::Ensure { path, group } => {
            let scope = scope_of(group, path)?;
            scope.ensure(false)?;
            println!("{}", scope.status()?);
        }
        Command::Reindex { path, group } => {
            let scope = scope_of(group, path)?;
            scope.ensure(true)?; // emergency: full rebuild, bypass dirty-gate
            println!("{}", scope.status()?);
        }
        Command::Status { path, group } => {
            println!("{}", scope_of(group, path)?.status()?);
        }
        Command::Explore {
            symbol,
            path,
            group,
        } => {
            println!("{}", scope_of(group, path)?.explore(&symbol)?);
        }
        Command::Search {
            query: q,
            path,
            group,
        } => {
            println!("{}", scope_of(group, path)?.search(&q)?);
        }
        Command::Setup { path, group } => {
            let g = group::from_env(group.as_deref());
            if let Some(name) = g.as_deref() {
                group::roots(Some(name), &root_of(None))?;
            }
            setup::run(&root_of(path), g.as_deref())?;
        }
        Command::Watch { path, group } => {
            watch::run(&scope_of(group, path)?)?;
        }
        Command::Changes { path, group } => {
            println!(
                "{}",
                scope_of(group, path)?.changes(changes::ChangeSpec::Unstaged)?
            );
        }
        Command::Group {
            action,
            group,
            root,
        } => {
            println!(
                "{}",
                group::run(&action, group.as_deref(), root.as_deref())?
            );
        }
    }
    Ok(())
}
