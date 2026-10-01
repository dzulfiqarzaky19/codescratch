#!/usr/bin/env node
// Regression: the Pi host takes its grep rules from the Claude Code host: a symbol grep is
// answered from the graph, any other grep is folded, and string/TODO searches still run.
// Usage: node --experimental-strip-types tests/pi-rewrite.mjs [path-to-codescratch-binary]
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const BIN = path.resolve(process.argv[2] || path.join(here, "..", "target", "debug", "codescratch"));

let failed = 0;
const check = (label, ok) => {
  if (!ok) {
    console.error(`FAIL ${label}`);
    failed++;
  }
};

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "cs-pi-"));
const repo = path.join(tmp, "repo");
const plain = path.join(tmp, "plain");
fs.mkdirSync(path.join(repo, "src"), { recursive: true });
fs.mkdirSync(plain);
fs.writeFileSync(path.join(repo, "src", "a.ts"), "export function helper() { return 1; }\nexport function run() { return helper(); }\n");
// One function holding 30 lines of the same word: a grep result worth folding.
fs.writeFileSync(
  path.join(repo, "src", "big.ts"),
  `export function big() {\n${Array.from({ length: 30 }, (_, i) => `  const needleWord${i} = ${i}; // padding padding`).join("\n")}\n}\n`,
);
fs.writeFileSync(path.join(plain, "a.ts"), "export function helper() { return 1; }\n");
spawnSync(BIN, ["ensure"], { cwd: repo });

// The engine runs in this process: point it at the fixture before it loads.
process.env.HOME = tmp;
process.env.CODESCRATCH_BIN = BIN;
const state = path.join(tmp, "state");
const { decide } = await import("../host/pi-codescratch.ts");
const grep = (cwd, input, session = "p1") => decide("grep", input, cwd, session, state);
const bash = (command, session = "p1", cwd = repo) => decide("bash", { command }, cwd, session, state);

const hit = grep(repo, { pattern: "helper" });
check("grep tool: symbol is blocked with the answer", /## .*helper/.test(hit?.block || ""));
check("grep tool: a repeat runs", grep(repo, { pattern: "helper" }) === null);
check("grep tool: TODO runs", grep(repo, { pattern: "TODO" }, "p2") === null);
check("grep tool: ignoreCase runs", grep(repo, { pattern: "helper", ignoreCase: true }, "p2") === null);
check("grep tool: a miss runs", grep(repo, { pattern: "nothingHere" }, "p2") === null);
check("grep tool: outside a scope runs", grep(plain, { pattern: "helper" }, "p2") === null);
check("other tools are left alone", decide("read", { path: "src/a.ts" }, repo, "p2", state) === null);

check("bash: a lone symbol grep is blocked with the answer", /## .*helper/.test(bash("grep -rn helper src", "p3")?.block || ""));
const batch = bash("ls src; grep -rn helper src", "p4")?.command || "";
check("bash: in a batch the symbol grep becomes a cat, the rest stays", /^ls src; cat '.*helper\.md'$/.test(batch));
check("bash: TODO is still a grep", /^rg TODO \| .* fold /.test(bash("rg TODO", "p4")?.command || ""));
check("bash: outside a scope runs", bash("rg -n needleWord src | head -50", "p4", plain) === null);

const big = "rg -n needleWord src | head -50";
const folded = bash(big, "p5")?.command || "";
check("bash: any other grep is piped through fold", / fold --tag \w+ --log '.*' --head 50 \| head -50$/.test(folded) && folded.includes(state));
const ran = spawnSync("sh", ["-c", folded], { cwd: repo, encoding: "utf8" });
check("bash: the folded command prints a fold", /^fold: 30 hits/.test(ran.stdout));
check("bash: a repeat of a folded grep runs raw", bash(big, "p5") === null);

fs.rmSync(tmp, { recursive: true, force: true });
if (failed) process.exit(1);
console.log("ok: pi host blocks, rewrites and folds like the claude host");
