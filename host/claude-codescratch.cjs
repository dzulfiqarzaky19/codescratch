#!/usr/bin/env node
/**
 * codescratch host for Claude Code: the CC twin of host/pi-codescratch.ts.
 * Installed by `codescratch setup` as ~/.claude/hooks/codescratch-host.cjs. No MCP.
 *
 * SessionStart            → ensure (catch-up) + one short note, only inside a codescratch scope
 * PreToolUse Grep|Bash    → bare-identifier grep answered by `codescratch explore`:
 *                           hit  = deny, the reason carries the answer (no extra turn)
 *                           hit inside a batch (`;` `&&` `||`) = that segment rewritten to a
 *                                  `cat` of the answer, the rest runs (updatedInput, no decision)
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

/** Output trims the graph answer makes moot: `| head -N`, `| tail -N`, `2>/dev/null`, `2>&1`. */
const TRIM_TAIL = /(?:\s*\|\s*(?:head|tail)(?:\s+-n)?(?:\s+-?\d+)?|\s+2>\s*\/dev\/null|\s+2>&1)+\s*$/;
/** Explore calls one hook run may spend (3s timeout each). */
const MAX_REWRITES = 4;

/** Quote-aware split on top-level `;` `&&` `||` and newlines → [{ text, sep }], joinable back
 *  byte-for-byte. null for substitutions, heredocs, process substitution or background jobs:
 *  never rewrite what this cannot parse. */
function splitChain(command) {
  const s = String(command);
  if (/\$\(|`|<<|<\(|>\(/.test(s)) return null;
  const parts = [];
  let cur = "";
  let q = null;
  for (let i = 0; i < s.length; i++) {
    const ch = s[i];
    if (q) {
      cur += ch;
      if (ch === q) q = null;
      else if (ch === "\\" && q === '"') cur += s[++i] ?? "";
      continue;
    }
    if (ch === "\\") {
      cur += ch + (s[i + 1] ?? "");
      i++;
      continue;
    }
    if (ch === "'" || ch === '"') {
      q = ch;
      cur += ch;
      continue;
    }
    if (ch === ";" || ch === "\n") {
      parts.push({ text: cur, sep: ch });
      cur = "";
      continue;
    }
    if ((ch === "&" || ch === "|") && s[i + 1] === ch) {
      parts.push({ text: cur, sep: ch + ch });
      cur = "";
      i++;
      continue;
    }
    if (ch === "&") {
      if (s[i - 1] === ">" || s[i + 1] === ">") {
        cur += ch; // 2>&1, &>
        continue;
      }
      return null;
    }
    cur += ch;
  }
  if (q) return null;
  parts.push({ text: cur, sep: "" });
  return parts;
}

/** Bash command → { parts, hits: [{ i, ident, dir }], only } or null. `only`: nothing but
 *  `cd`s and one symbol grep, so the whole call can be answered; otherwise segments are rewritten. */
function planBash(command, cwd) {
  const parts = splitChain(command);
  if (!parts) return null;
  let dir = cwd;
  let others = 0;
  const hits = [];
  parts.forEach((p, i) => {
    const t = p.text.trim();
    if (!t) return;
    const cd = t.match(/^cd\s+(\S+)$/);
    if (cd) {
      dir = expand(cd[1], dir);
      return;
    }
    const target = fromBash(t.replace(TRIM_TAIL, ""), dir);
    if (target) hits.push({ i, ...target });
    else others++;
  });
  if (!hits.length) return null;
  return { parts, hits, only: others === 0 && hits.length === 1 };
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

/** One symbol grep → { key, ident, repo, root, body }, or null when the grep should run (a file
 *  path, outside a scope, a repeat, a miss). Pushes the first-touch note for a repo the session
 *  did not start in. */
function answerFor(target, st, session, notes) {
  // A grep scoped to one file wants that file's lines, not the repo-wide graph answer.
  if (!isDir(target.dir)) return null;
  const scope = scopeOf(target.dir);
  if (!scope) return null;
  if (!st.scopes.includes(scope)) {
    // First touch of a repo the session did not start in: catch it up and say so once.
    st.scopes.push(scope);
    kick(["ensure"], scope);
    notes.push(note(scope));
  }
  const key = `${scope}\0${target.ident}`;
  const repo = path.basename(scope);
  if (st.served[key]) {
    // Escape hatch: Claude asked twice, so it wants raw matches. Log it as waste.
    metric({ decision: "allow", rule: "regrep", ident: target.ident, repo, session });
    return null;
  }
  const answer = explore(target.ident, scope);
  if (!answer) {
    metric({ decision: "allow", rule: "symbol-miss", ident: target.ident, repo, session });
    return null;
  }
  const body =
    answer.length > MAX_ANSWER
      ? `${answer.slice(0, MAX_ANSWER)}\n… (truncated; run \`codescratch explore ${target.ident}\` for the rest)`
      : answer;
  return { key, ident: target.ident, repo, root: findRoot(target.dir), body };
}

function contextOnly(notes) {
  if (notes.length) emit({ hookSpecificOutput: { hookEventName: "PreToolUse", additionalContext: notes.join("\n") } });
}

/** The whole call is one symbol grep: deny it and hand back the answer (no extra turn). */
function serveDeny(input, target) {
  const st = loadState(input.session_id);
  const notes = [];
  const a = answerFor(target, st, input.session_id, notes);
  if (a) st.served[a.key] = true;
  saveState(input.session_id, st);
  if (!a) return contextOnly(notes);
  metric({ decision: "deny", rule: "symbol-served", ident: a.ident, repo: a.repo, session: input.session_id, explore_chars: a.body.length });
  const out = {
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "deny",
      permissionDecisionReason:
        `codescratch answered this symbol lookup from the graph, so the grep was skipped. ` +
        `If you need raw text matches, repeat the same grep once and it will run.\n\n${a.body}`,
    },
  };
  if (notes.length) out.hookSpecificOutput.additionalContext = notes.join("\n");
  emit(out);
}

/** Answers live in the grepped repo's own `.codescratch/`, so reading one needs exactly the
 *  directory access the grep needed: Claude Code's sandbox denies a `cat` outside the working
 *  dirs even when `Bash(cat:*)` is allowed, and that denial takes the whole batch with it. */
function answerFile(session, a) {
  if (!a.root) return null;
  const safe = String(session || "nosession").replace(/[^A-Za-z0-9_-]/g, "_").slice(0, 80);
  const f = path.join(a.root, ".codescratch", "answers", safe, `${a.ident}.md`);
  try {
    fs.mkdirSync(path.dirname(f), { recursive: true });
    fs.writeFileSync(
      f,
      `# codescratch: grep for \`${a.ident}\` answered from the graph (repeat the same grep once for raw matches)\n${a.body}\n`,
    );
    return f;
  } catch {
    return null;
  }
}

const shq = (s) => `'${String(s).replace(/'/g, `'\\''`)}'`;

/** Symbol greps inside a batch: swap each answered segment for a `cat` of its answer and let
 *  the rest run. No permissionDecision, so the rewritten command goes through the normal
 *  permission flow (verified on CC 2.1.282: an allow rule for the original did not cover it). */
function serveRewrite(input, plan) {
  const st = loadState(input.session_id);
  const notes = [];
  const texts = plan.parts.map((p) => p.text);
  const local = new Map();
  const served = [];
  for (const h of plan.hits.slice(0, MAX_REWRITES)) {
    const k = `${h.dir}\0${h.ident}`;
    if (!local.has(k)) {
      const a = answerFor(h, st, input.session_id, notes);
      const file = a && answerFile(input.session_id, a);
      local.set(k, file ? { ...a, file } : null);
      if (file) {
        st.served[a.key] = true;
        served.push(a);
      }
    }
    const got = local.get(k);
    if (got) texts[h.i] = texts[h.i].replace(texts[h.i].trim(), `cat ${shq(got.file)}`);
  }
  saveState(input.session_id, st);
  if (!served.length) return contextOnly(notes);
  for (const a of served)
    metric({ decision: "rewrite", rule: "symbol-rewrite", ident: a.ident, repo: a.repo, session: input.session_id, explore_chars: a.body.length });
  const command = texts.map((t, i) => t + plan.parts[i].sep).join("");
  const out = { hookSpecificOutput: { hookEventName: "PreToolUse", updatedInput: { ...input.tool_input, command } } };
  if (notes.length) out.hookSpecificOutput.additionalContext = notes.join("\n");
  emit(out);
}

function onPreToolUse(input) {
  const cwd = input.cwd || process.cwd();
  const ti = input.tool_input || {};
  if (input.tool_name === "Grep") {
    const target = fromGrepTool(ti, cwd);
    if (target) serveDeny(input, target);
    return;
  }
  if (input.tool_name !== "Bash") return;
  const plan = planBash(ti.command || "", cwd);
  if (!plan) return;
  if (plan.only) serveDeny(input, plan.hits[0]);
  else serveRewrite(input, plan);
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

module.exports = { fromBash, fromGrepTool, scopeOf, isSymbolIdent, splitChain, planBash };
