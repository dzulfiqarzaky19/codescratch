#!/usr/bin/env node
// Fail-soft host ensure for codescratch. Never blocks the turn.
// SessionStart / PostToolUse → spawn `codescratch ensure` (detached).

const fs = require("fs");
const path = require("path");
const os = require("os");
const { spawn, spawnSync } = require("child_process");

const LOG_PATH = path.join(
  os.homedir(),
  ".claude",
  "hooks",
  "logs",
  "codescratch-ensure.jsonl",
);

function log(event) {
  try {
    fs.mkdirSync(path.dirname(LOG_PATH), { recursive: true });
    fs.appendFileSync(
      LOG_PATH,
      JSON.stringify({ ts: new Date().toISOString(), ...event }) + "\n",
    );
  } catch {
    /* never block */
  }
}

function readStdin() {
  try {
    return JSON.parse(fs.readFileSync(0, "utf8") || "{}");
  } catch {
    return {};
  }
}

function resolveRoot(input) {
  const fromEnv =
    process.env.CODESCRATCH_ROOT ||
    process.env.CLAUDE_PROJECT_DIR ||
    input.cwd ||
    process.cwd();
  return path.resolve(fromEnv);
}

function findCli() {
  // 1) PATH
  const which = process.platform === "win32" ? "where" : "which";
  try {
    const r = spawnSync(which, ["codescratch"], {
      encoding: "utf8",
      windowsHide: true,
    });
    if (r.status === 0) {
      const line = (r.stdout || "").split(/\r?\n/).map((s) => s.trim()).find(Boolean);
      if (line) return { cmd: line, argsPrefix: [] };
    }
  } catch {
    /* fall through */
  }

  // 2) plugin / package relative dist
  const here = __dirname;
  const candidates = [
    path.resolve(here, "..", "..", "..", "dist", "cli.js"),
    path.resolve(here, "..", "..", "dist", "cli.js"),
    path.resolve(here, "..", "..", "..", "..", "dist", "cli.js"),
  ];
  for (const c of candidates) {
    if (fs.existsSync(c)) return { cmd: process.execPath, argsPrefix: [c] };
  }
  return null;
}

const notify = process.argv.includes("--notify");
const input = readStdin();
const root = resolveRoot(input);
const cli = findCli();

if (!cli) {
  log({ stage: "no-cli", root });
  process.exit(0);
}

const args = [...cli.argsPrefix, "ensure", root];
if (notify) args.push("--notify");

try {
  const child = spawn(cli.cmd, args, {
    detached: true,
    stdio: notify ? ["ignore", "pipe", "ignore"] : "ignore",
    windowsHide: true,
    env: {
      ...process.env,
      CODESCRATCH_ROOT: root,
    },
  });

  if (notify && child.stdout) {
    let out = "";
    child.stdout.on("data", (b) => {
      out += b.toString("utf8");
    });
    child.on("close", () => {
      const line = out.trim().split(/\r?\n/).filter(Boolean).pop();
      if (line) {
        // SessionStart may surface stdout; keep one short line.
        process.stdout.write(line + "\n");
      }
      log({ stage: "spawned-notify", root, pid: child.pid, line: line || null });
      process.exit(0);
    });
    child.unref();
    // Cap wait so SessionStart cannot hang forever.
    setTimeout(() => {
      log({ stage: "notify-timeout", root, pid: child.pid });
      process.exit(0);
    }, 8000).unref?.();
  } else {
    child.unref();
    log({ stage: "spawned", root, pid: child.pid });
    process.exit(0);
  }
} catch (e) {
  log({ stage: "spawn-error", root, error: String(e && e.message ? e.message : e) });
  process.exit(0);
}
