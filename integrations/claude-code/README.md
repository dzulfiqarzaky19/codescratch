# codescratch — Claude Code host hooks

Keeps `<repo>/.codescratch/graph.db` fresh **without** the model calling `cs_reindex`.

## Prerequisites

```bash
cd codescratch
npm install && npm run build
npm link   # codescratch on PATH
```

MCP (deep queries) stays separate — configure as today via `.mcp.json` / user settings.

## What runs

| Hook | Action |
|------|--------|
| `SessionStart` | `codescratch ensure <root> --notify` (one short status line) |
| `PostToolUse` `Edit\|Write\|MultiEdit` | detached `codescratch ensure <root>` (coalesced) |

Single-flight lock + pending marker under `.codescratch/`. Branch checkout → at most one job (HEAD compared inside ensure). Fail-soft: hook always exits 0.

Logs: `~/.claude/hooks/logs/codescratch-ensure.jsonl`

## Install

Point a Claude Code plugin/hooks entry at this folder, or copy the hook commands into user `settings.json`:

```json
{
  "hooks": {
    "SessionStart": [{
      "hooks": [{
        "type": "command",
        "command": "node \"/abs/path/to/codescratch/integrations/claude-code/scripts/ensure-hook.cjs\" --notify"
      }]
    }],
    "PostToolUse": [{
      "matcher": "Edit|Write|MultiEdit",
      "hooks": [{
        "type": "command",
        "command": "node \"/abs/path/to/codescratch/integrations/claude-code/scripts/ensure-hook.cjs\""
      }]
    }]
  }
}
```

Env:

| Var | Purpose |
|-----|---------|
| `CODESCRATCH_ROOT` | Repo root (else `CLAUDE_PROJECT_DIR` / hook cwd) |

## Agent protocol

- **Do not** routine-`cs_reindex` after every edit.
- Use `cs_explore` / `cs_callers` / `cs_impact` / `cs_search` for structure.
- `cs_reindex` only if trust stuck `stale`/`rebuilding` or host hooks are absent.
- Read `trust:` — `rebuilding` means absence ≠ proof.
