#!/usr/bin/env node
/**
 * codescratch host for Claude Code: the CC twin of host/pi-codescratch.ts.
 * Installed by `codescratch setup` as ~/.claude/hooks/codescratch-host.cjs. No MCP.
 *
 * SessionStart            → ensure (catch-up) + one short note, only inside a codescratch scope
 * PreToolUse Grep|Bash    → bare-identifier grep answered by `codescratch explore`:
 *                           hit  = deny, the reason carries the answer (no extra turn)
 *                           miss = grep runs untouched; a repeat of a served grep runs untouched
 * PostToolUse Edit|Write  → ensure for the edited file's repo (host lock coalesces bursts)
 *
 * A scope is the nearest ancestor holding `.codescratch/`, or the unique parent of a
 * registered group (the CLI fans out from there). Outside a scope the hook is silent.
 *
 * Every decision lands in the context-economy ledger (`type: 'guard'`,
 * `hook: 'codescratch-host'`) so the scoreboard can net its cost against its value.
 * Never throws, always exits 0.
 */

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawn, spawnSync } = require("node:child_process");

const HOOK = "codescratch-host";
const IDENT = /^[A-Za-z_][A-Za-z0-9_]*$/;
/** Convention markers — grep these, do not `explore`. */
const NOT_SYMBOL = new Set(["TODO", "FIXME", "HACK", "XXX"]);
const CODE_FILE = /\.(tsx?|jsx?|mjs|cjs|mts|cts|py)\b|\{[^}]*\b(tsx?|jsx?|py)\b/;
const CODE_TYPE = new Set(["ts", "js", "tsx", "jsx", "py", "python", "typescript", "javascript"]);
/** 10k is Claude Code's hook-output cap; leave room for the lead-in. */
const MAX_ANSWER = 6000;

function isSymbolIdent(q) {
  return IDENT.test(q) && !NOT_SYMBOL.has(q.toUpperCase());
}

function bin() {
  const fromEnv = process.env.CODESCRATCH_BIN;
  if (fromEnv && fs.existsSync(fromEnv)) return fromEnv;
  const fallback = path.join(os.homedir(), ".local/bin/codescratch");
  return fs.existsSync(fallback) ? fallback : "codescratch";
}

function expand(p, cwd) {
  const s = String(p).replace(/^['"]|['"]$/g, "").replace(/^~(?=$|\/)/, os.homedir());
  return path.resolve(cwd, s);
}

function isDir(p) {
  try {
    return fs.statSync(p).isDirectory();
  } catch {
    return false;
  }
}

/** Nearest repo root below $HOME: `~/.codescratch` is the global config dir, not a repo. */
function findRoot(dir) {
  const home = os.homedir();
  let d = dir;
  for (;;) {
    if (d === home) return null;
    if (isDir(path.join(d, ".codescratch"))) return d;
    const up = path.dirname(d);
    if (up === d) return null;
    d = up;
  }
}

/** `dir` is the unique parent of exactly one registered group's roots. */
function isGroupParent(dir) {
  let groups = {};
  try {
    const f = path.join(os.homedir(), ".codescratch", "groups.json");
    groups = JSON.parse(fs.readFileSync(f, "utf8")).groups || {};
  } catch {
    return false;
  }
  const hits = Object.values(groups).filter(
    (g) => Array.isArray(g.roots) && g.roots.length > 0 && g.roots.every((r) => path.dirname(r) === dir),
  );
  return hits.length === 1;
}

function scopeOf(dir) {
  if (!dir) return null;
  const d = isDir(dir) ? dir : path.dirname(dir);
  return findRoot(d) || (isGroupParent(d) ? d : null);
}

function kick(args, cwd) {
  try {
    spawn(bin(), args, { cwd, detached: true, stdio: "ignore" }).unref();
  } catch {
    /* host freshness is best-effort */
  }
}

function metric(row) {
  try {
    const { appendMetric } = require(path.join(__dirname, "lib", "metrics.cjs"));
    appendMetric({ type: "guard", hook: HOOK, ts: Date.now(), ...row });
  } catch {
    /* telemetry optional */
  }
}

// --- per-session state: which scopes were announced, which symbols were served ---

function statePath(session) {
  const root = process.env.CLAUDE_HOOKS_STATE_DIR || path.join(os.homedir(), ".claude", "hooks", "state");
  const safe = String(session || "nosession").replace(/[^A-Za-z0-9_-]/g, "_").slice(0, 80);
  return path.join(root, HOOK, `${safe}.json`);
}

function loadState(session) {
  try {
    const s = JSON.parse(fs.readFileSync(statePath(session), "utf8"));
    return { scopes: s.scopes || [], served: s.served || {} };
  } catch {
    return { scopes: [], served: {} };
  }
}

function saveState(session, st) {
  try {
    const f = statePath(session);
    fs.mkdirSync(path.dirname(f), { recursive: true });
    fs.writeFileSync(f, JSON.stringify(st));
  } catch {
    /* state is an optimisation, not a requirement */
  }
}

function note(scope) {
  return (
    `codescratch graph active for \`${path.basename(scope)}\`. For where-defined / who-calls / blast radius ` +
    "use `codescratch explore <Symbol>` or `codescratch search <name>`; grep stays right for strings, regex, " +
    "fields, SQL and config. A bare-identifier grep is answered from the graph automatically; repeat the same " +
    "grep once to force raw matches. Read the `trust:` line: under `coverage: sampled` or `resolve: partial`, absence is not proof."
  );
}

// --- symbol-grep detection ---

/** Grep tool input → { ident, dir } or null. */
function fromGrepTool(input, cwd) {
  const q = String(input.pattern || "");
  if (!isSymbolIdent(q)) return null;
  if (input["-i"] || input.multiline || input.output_mode === "count") return null;
  if (input.type && !CODE_TYPE.has(String(input.type))) return null;
  if (input.glob && !CODE_FILE.test(String(input.glob))) return null;
  return { ident: q, dir: input.path ? expand(input.path, cwd) : cwd };
}

const VALUE_FLAGS = new Set(["-g", "--glob", "-t", "--type", "-T", "--type-not", "-A", "-B", "-C", "-m", "--max-count", "--include", "--exclude"]);
const SKIP_FLAG = /^-(?:[a-zA-Z]*[eiPFfvwcx][a-zA-Z]*|-(?:regexp|ignore-case|perl-regexp|fixed-strings|file|invert-match|word-regexp|count))(?:=|$)/;

/** `[cd DIR &&] rg|grep … IDENT [PATH]` → { ident, dir } or null. Pipes, chains, subshells: never. */
function fromBash(command, cwd) {
  const m = String(command).trim().match(/^(?:cd\s+(\S+)\s*&&\s*)?(?:rg|grep|ugrep|ag)\s+(.+)$/);
  if (!m) return null;
  const rest = m[2];
  if (/[|;&<>`$()]/.test(rest)) return null;
  const base = m[1] ? expand(m[1], cwd) : cwd;
  const tokens = rest.split(/\s+/).filter(Boolean);
  const positional = [];
  for (let i = 0; i < tokens.length; i++) {
    const t = tokens[i];
    if (t === "--") continue;
    if (VALUE_FLAGS.has(t)) {
      const v = tokens[++i] || "";
      if (/^(-g|--glob|--include)$/.test(t) && !CODE_FILE.test(v)) return null;
      if (/^(-t|--type)$/.test(t) && !CODE_TYPE.has(v)) return null;
      continue;
    }
    if (t.startsWith("-")) {
      if (SKIP_FLAG.test(t)) return null;
      const inc = t.match(/^--(?:include|glob)=(.+)$/);
      if (inc && !CODE_FILE.test(inc[1])) return null;
      continue;
    }
    positional.push(t);
  }
  if (positional.length < 1 || positional.length > 2) return null;
  const ident = positional[0].replace(/^['"]|['"]$/g, "");
  if (!isSymbolIdent(ident)) return null;
  return { ident, dir: positional[1] ? expand(positional[1], base) : base };
}

function explore(ident, scope) {
  const r = spawnSync(bin(), ["explore", ident], { cwd: scope, encoding: "utf8", timeout: 3000 });
  if (r.status !== 0 || typeof r.stdout !== "string") return null;
  return /^## /m.test(r.stdout) ? r.stdout : null;
}

// --- events ---

function emit(obj) {
  process.stdout.write(JSON.stringify(obj));
}

function onSessionStart(input) {
  const cwd = input.cwd || process.cwd();
  const scope = scopeOf(cwd);
  if (!scope) return;
  kick(["ensure"], scope);
  const st = loadState(input.session_id);
  if (!st.scopes.includes(scope)) st.scopes.push(scope);
  saveState(input.session_id, st);
  emit({ hookSpecificOutput: { hookEventName: "SessionStart", additionalContext: note(scope) } });
}

function onPreToolUse(input) {
  const cwd = input.cwd || process.cwd();
  const ti = input.tool_input || {};
  const target =
    input.tool_name === "Grep" ? fromGrepTool(ti, cwd) : input.tool_name === "Bash" ? fromBash(ti.command || "", cwd) : null;
  if (!target) return;
  const scope = scopeOf(target.dir);
  if (!scope) return;

  const st = loadState(input.session_id);
  let context = "";
  if (!st.scopes.includes(scope)) {
    // First touch of a repo the session did not start in: catch it up and say so once.
    st.scopes.push(scope);
    kick(["ensure"], scope);
    context = note(scope);
  }
  const key = `${scope}\0${target.ident}`;
  if (st.served[key]) {
    // Escape hatch: Claude asked twice, so it wants raw matches. Log it as waste.
    metric({ decision: "allow", rule: "regrep", ident: target.ident, repo: path.basename(scope), session: input.session_id });
    saveState(input.session_id, st);
    if (context) emit({ hookSpecificOutput: { hookEventName: "PreToolUse", additionalContext: context } });
    return;
  }

  const answer = explore(target.ident, scope);
  if (!answer) {
    metric({ decision: "allow", rule: "symbol-miss", ident: target.ident, repo: path.basename(scope), session: input.session_id });
    saveState(input.session_id, st);
    if (context) emit({ hookSpecificOutput: { hookEventName: "PreToolUse", additionalContext: context } });
    return;
  }

  st.served[key] = true;
  saveState(input.session_id, st);
  const body =
    answer.length > MAX_ANSWER
      ? `${answer.slice(0, MAX_ANSWER)}\n… (truncated; run \`codescratch explore ${target.ident}\` for the rest)`
      : answer;
  metric({
    decision: "deny",
    rule: "symbol-served",
    ident: target.ident,
    repo: path.basename(scope),
    session: input.session_id,
    explore_chars: body.length,
  });
  const out = {
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "deny",
      permissionDecisionReason:
        `codescratch answered this symbol lookup from the graph, so the grep was skipped. ` +
        `If you need raw text matches, repeat the same grep once and it will run.\n\n${body}`,
    },
  };
  if (context) out.hookSpecificOutput.additionalContext = context;
  emit(out);
}

function onPostToolUse(input) {
  const f = (input.tool_input || {}).file_path;
  if (!f) return;
  const root = findRoot(path.dirname(expand(f, input.cwd || process.cwd())));
  if (root) kick(["ensure"], root);
}

function main() {
  let input = {};
  try {
    input = JSON.parse(fs.readFileSync(0, "utf8") || "{}");
  } catch {
    return;
  }
  const ev = input.hook_event_name;
  if (ev === "SessionStart") onSessionStart(input);
  else if (ev === "PreToolUse") onPreToolUse(input);
  else if (ev === "PostToolUse") onPostToolUse(input);
}

if (require.main === module) {
  try {
    main();
  } catch {
    /* a host hook must never break the session */
  }
  process.exit(0);
}

module.exports = { fromBash, fromGrepTool, scopeOf, isSymbolIdent };
