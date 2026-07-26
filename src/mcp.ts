#!/usr/bin/env node
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import { resolveRoot } from "./config.js";
import { ensureRepo } from "./host/ensure.js";
import { listCallees, listCallers } from "./query/callers.js";
import { exploreSymbol } from "./query/explore.js";
import { impactAnalysis } from "./query/impact.js";
import { searchSymbols } from "./query/search.js";
import { statusReport } from "./query/status.js";

const rootParam = z
  .string()
  .optional()
  .describe(
    "Repo root override (monorepo/multi-root). Defaults to CODESCRATCH_ROOT or cwd.",
  );

function pickRoot(explicit?: string): string {
  return resolveRoot(explicit ?? process.env.CODESCRATCH_ROOT);
}

function text(s: string) {
  return { content: [{ type: "text" as const, text: s }] };
}

function err(e: unknown) {
  const msg = e instanceof Error ? e.message : String(e);
  return {
    content: [{ type: "text" as const, text: `error: ${msg}` }],
    isError: true as const,
  };
}

const server = new McpServer({
  name: "codescratch",
  version: "0.1.0",
});

server.tool(
  "cs_status",
  "Index health: trust, counts, staleness, exact reindex command. Call first if unsure whether the graph exists.",
  { root: rootParam },
  async ({ root }) => {
    try {
      return text(statusReport(pickRoot(root)));
    } catch (e) {
      return err(e);
    }
  },
);

server.tool(
  "cs_search",
  "Search symbols by name (FTS). Prefer before explore when the name is uncertain.",
  {
    query: z.string().describe("Symbol name or partial name"),
    root: rootParam,
  },
  async ({ query, root }) => {
    try {
      return text(searchSymbols(query, pickRoot(root)));
    } catch (e) {
      return err(e);
    }
  },
);

server.tool(
  "cs_explore",
  "Inspect one symbol or file: snippet, members, calls, callers, imports, bindings, heritage. Trust banner included — graph incomplete under dynamic dispatch.",
  {
    target: z
      .string()
      .describe("Symbol name, qualified name, file path, or name@path"),
    root: rootParam,
  },
  async ({ target, root }) => {
    try {
      return text(exploreSymbol(target, pickRoot(root)));
    } catch (e) {
      return err(e);
    }
  },
);

server.tool(
  "cs_callers",
  "Who calls this symbol (resolved edges). Weak-confidence edges included but labeled.",
  {
    target: z.string(),
    depth: z.number().int().min(1).max(6).optional(),
    root: rootParam,
  },
  async ({ target, depth, root }) => {
    try {
      return text(listCallers(target, pickRoot(root), depth ?? 1));
    } catch (e) {
      return err(e);
    }
  },
);

server.tool(
  "cs_callees",
  "What this symbol calls (resolved edges).",
  {
    target: z.string(),
    depth: z.number().int().min(1).max(6).optional(),
    root: rootParam,
  },
  async ({ target, depth, root }) => {
    try {
      return text(listCallees(target, pickRoot(root), depth ?? 1));
    } catch (e) {
      return err(e);
    }
  },
);

server.tool(
  "cs_impact",
  "Blast radius. direction=up (dependents, default), down (dependencies), or both. Capped BFS. Use before refactors.",
  {
    target: z.string(),
    direction: z.enum(["up", "down", "both"]).optional(),
    root: rootParam,
  },
  async ({ target, direction, root }) => {
    try {
      return text(impactAnalysis(target, pickRoot(root), direction ?? "up"));
    } catch (e) {
      return err(e);
    }
  },
);

server.tool(
  "cs_reindex",
  "Emergency reindex via host single-flight lock. Prefer host hooks (codescratch ensure). Use when trust stuck stale/rebuilding or host path is down. full=true forces full rebuild.",
  {
    root: rootParam,
    full: z.boolean().optional().describe("Force full index (default false)"),
  },
  async ({ root, full }) => {
    try {
      const result = await ensureRepo(pickRoot(root), {
        full: full === true,
        waitMs: 15_000,
      });
      if (result.coalesced) {
        return text(
          [
            "reindex coalesced — another ensure holds the lock; pending set",
            `root: ${result.root}`,
            "",
            statusReport(result.root),
          ].join("\n"),
        );
      }
      if (result.error) {
        return err(new Error(result.error));
      }
      const stats = result.stats!;
      const body = [
        `reindex ${result.full ? "full" : "incremental"}  passes=${result.passes}`,
        `root: ${stats.root}`,
        `head: ${result.head || "(none)"}`,
        `files: ${stats.files_total}  indexed: ${stats.files_indexed}  skipped: ${stats.files_skipped}  removed: ${stats.files_removed}`,
        `nodes: ${stats.nodes}  edges: ${stats.edges}  unresolved: ${stats.unresolved_edges}  bindings: ${stats.bindings}`,
        `duration: ${stats.duration_ms}ms`,
        "",
        statusReport(stats.root),
      ].join("\n");
      return text(body);
    } catch (e) {
      return err(e);
    }
  },
);

const transport = new StdioServerTransport();
await server.connect(transport);
