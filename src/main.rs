//! codescratch — local code structure graph for AI agents.
//! Single static binary: no Node, no npm, no wasm sidecar, no user-side toolchain.

mod analysis;
mod changes;
mod db;
mod embeddings;
mod extract;
mod group;
mod host;
mod index;
mod mcp;
mod model;
mod plugin;
mod query;
mod resolve;
mod setup;
mod trust;
mod watch;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "codescratch", version, about = "Local code structure graph for AI agents")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create the graph store and build it once.
    Init { path: Option<PathBuf> },
    /// Bring the graph up to date (host-owned freshness, single-flight).
    Ensure { path: Option<PathBuf> },
    /// Force a full rebuild (emergency; same lock as ensure).
    Reindex { path: Option<PathBuf> },
    /// Print the trust banner.
    Status { path: Option<PathBuf> },
    /// One symbol → banner + source + calls + callers (blast radius).
    Explore {
        symbol: String,
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Fuzzy find a symbol by name.
    Search {
        query: String,
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Serve the MCP stdio server (explore + status listed).
    Mcp { path: Option<PathBuf> },
    /// Write MCP client config for detected agents (WP-2E).
    Setup { path: Option<PathBuf> },
    /// Watch the repo and keep the graph fresh (WP-2C).
    Watch { path: Option<PathBuf> },
    /// Show symbols changed vs git diff + their blast (WP-3B).
    Changes { path: Option<PathBuf> },
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { path } => {
            let root = root_of(path);
            db::open(&root)?; // create schema
            host::ensure(&root)?;
            println!("{}", query::status(&root)?);
            println!("graph ready at {}/.codescratch/graph.db", root.display());
        }
        Command::Ensure { path } => {
            let root = root_of(path);
            host::ensure(&root)?;
            println!("{}", query::status(&root)?);
        }
        Command::Reindex { path } => {
            let root = root_of(path);
            host::reindex(&root)?; // emergency: force a full rebuild (bypass dirty-gate)
            println!("{}", query::status(&root)?);
        }
        Command::Status { path } => {
            println!("{}", query::status(&root_of(path))?);
        }
        Command::Explore { symbol, path } => {
            println!("{}", query::explore(&root_of(path), &symbol)?);
        }
        Command::Search { query, path } => {
            println!("{}", query::search(&root_of(path), &query)?);
        }
        Command::Mcp { path } => {
            mcp::serve(&root_of(path))?;
        }
        Command::Setup { path } => {
            setup::run(&root_of(path))?;
        }
        Command::Watch { path } => {
            watch::run(&root_of(path))?;
        }
        Command::Changes { path } => {
            println!("{}", changes::detect(&root_of(path), changes::ChangeSpec::Unstaged)?);
        }
        Command::Group { action, group, root } => {
            println!("{}", group::run(&action, group.as_deref(), root.as_deref())?);
        }
    }
    Ok(())
}
