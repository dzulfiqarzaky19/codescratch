#!/usr/bin/env node
import { Command } from "commander";
import { resolveRoot } from "./config.js";
import { indexRepo } from "./index/indexer.js";
import type { ImpactDirection, IndexStats } from "./models.js";
import { listCallees, listCallers } from "./query/callers.js";
import { exploreSymbol } from "./query/explore.js";
import { impactAnalysis } from "./query/impact.js";
import { searchSymbols } from "./query/search.js";
import { statusReport } from "./query/status.js";

const program = new Command();

program
  .name("codescratch")
  .description("Local code structure graph for AI agents (TS/JS)")
  .version("0.1.0");

program
  .command("init")
  .argument("[path]", "repo root", process.cwd())
  .description("Full build graph under <path>/.codescratch/")
  .action(async (path: string) => {
    printStats(await indexRepo(path, { full: true }));
  });

program
  .command("reindex")
  .argument("[path]", "repo root", process.cwd())
  .option("--full", "force full index", false)
  .description("Incremental reindex by content hash")
  .action(async (path: string, opts: { full?: boolean }) => {
    printStats(await indexRepo(path, { full: !!opts.full }));
  });

program
  .command("status")
  .argument("[path]", "repo root", process.cwd())
  .action((path: string) => {
    console.log(statusReport(path));
  });

program
  .command("search")
  .argument("<query>")
  .option("-r, --root <path>", "repo root")
  .action((query: string, opts: { root?: string }) => {
    console.log(searchSymbols(query, opts.root));
  });

program
  .command("explore")
  .argument("<symbol>")
  .option("-r, --root <path>", "repo root")
  .action((symbol: string, opts: { root?: string }) => {
    console.log(exploreSymbol(symbol, opts.root));
  });

program
  .command("callers")
  .argument("<symbol>")
  .option("-r, --root <path>", "repo root")
  .option("-d, --depth <n>", "depth", "1")
  .action((symbol: string, opts: { root?: string; depth: string }) => {
    console.log(listCallers(symbol, opts.root, Number(opts.depth)));
  });

program
  .command("callees")
  .argument("<symbol>")
  .option("-r, --root <path>", "repo root")
  .option("-d, --depth <n>", "depth", "1")
  .action((symbol: string, opts: { root?: string; depth: string }) => {
    console.log(listCallees(symbol, opts.root, Number(opts.depth)));
  });

program
  .command("impact")
  .argument("<symbol>")
  .option("-r, --root <path>", "repo root")
  .option(
    "-d, --direction <dir>",
    "up | down | both (default up)",
    "up",
  )
  .action(
    (
      symbol: string,
      opts: { root?: string; direction: string },
    ) => {
      const dir = opts.direction as ImpactDirection;
      if (dir !== "up" && dir !== "down" && dir !== "both") {
        console.error("direction must be up|down|both");
        process.exitCode = 1;
        return;
      }
      console.log(impactAnalysis(symbol, opts.root, dir));
    },
  );

program
  .command("mcp")
  .description("Run MCP stdio server")
  .action(async () => {
    await import("./mcp.js");
  });

program.parse();

function printStats(stats: IndexStats): void {
  console.log(`root: ${resolveRoot(stats.root)}`);
  console.log(`mode: ${stats.full ? "full" : "incremental"}`);
  console.log(
    `files: ${stats.files_total}  indexed: ${stats.files_indexed}  skipped: ${stats.files_skipped}  removed: ${stats.files_removed}`,
  );
  console.log(
    `nodes: ${stats.nodes}  edges: ${stats.edges}  unresolved: ${stats.unresolved_edges}  bindings: ${stats.bindings}`,
  );
  console.log(`duration: ${stats.duration_ms}ms`);
}
