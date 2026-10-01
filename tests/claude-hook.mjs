#!/usr/bin/env node
// Regression: the Claude Code host answers symbol greps from the graph and leaves
// every other grep alone. Usage: tests/claude-hook.mjs [path-to-codescratch-binary]
import { createRequire } from "node:module";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const HOOK = path.join(here, "..", "host", "claude-codescratch.cjs");
const BIN = path.resolve(process.argv[2] || path.join(here, "..", "target", "debug", "codescratch"));
const { fromBash, fromGrepTool, splitChain, planBash, foldable, tailFlags } = createRequire(import.meta.url)(HOOK);

let failed = 0;
const check = (label, ok) => {
  if (!ok) {
    console.error(`FAIL ${label}`);
    failed++;
  }
};

// --- parse rules (cwd = /r) ---
const bash = {
  "rg helper": "helper",
  "grep -rn Foo src/": "Foo",
  "cd /x && rg Foo": "Foo",
  "rg -t ts Foo": "Foo",
  "rg -g '*.md' Foo": null,
  "grep -rn --include=*.sql Foo .": null,
  "rg TODO": null,
  "rg -i Foo": null,
  "rg -nw Foo": "Foo",
  "rg -x Foo": null,
  "rg -e Foo": null,
  "rg Foo | head": null,
  "rg 'foo.bar'": null,
  "rg Foo a b": null,
  "ls Foo": null,
  // dressing around one name still looks that name up
  'rg -n "Foo\\(" src': "Foo",
  "grep -rn '\\bFoo\\b' src": "Foo",
  'grep -rn "export const Foo" src': "Foo",
  "grep -rn 'Foo\\|export function Foo' src": "Foo",
  'grep -rn Foo "src/routes/(provider)/ams/"': "Foo",
  'grep -rn "type Foo\\|interface Foo" src': "Foo",
  "grep -rnE '(Foo|Bar)\\(' src": "Foo|Bar",
  "grep -rn 'A\\|B\\|C\\|D\\|E' src": null,
  'grep -rn ".Foo(" src': null,
  'grep -rn "$X" src': null,
  "grep -rn Foo $DIR": null,
};
for (const [cmd, want] of Object.entries(bash)) {
  const got = fromBash(cmd, "/r");
  check(`bash ${JSON.stringify(cmd)} => ${JSON.stringify(got)}`, (got ? got.idents.join("|") : null) === want);
}
check("bash dir from path arg", fromBash("grep -rn Foo src/", "/r").dir === "/r/src");
check("bash dir from cd", fromBash("cd /x && rg Foo", "/r").dir === "/x");
check("grep tool ident", fromGrepTool({ pattern: "Foo" }, "/r").idents.join() === "Foo");
check("grep tool -i", fromGrepTool({ pattern: "Foo", "-i": true }, "/r") === null);
check("grep tool md glob", fromGrepTool({ pattern: "Foo", glob: "*.md" }, "/r") === null);
check("grep tool call lookup", fromGrepTool({ pattern: "Foo\\(" }, "/r").idents.join() === "Foo");
check("grep tool regex", fromGrepTool({ pattern: "Foo.*Bar" }, "/r") === null);

// --- batches: split rejoins byte-for-byte, only symbol segments are planned ---
for (const cmd of [
  `echo "=== a; b ==="; rg -n Foo src && grep -rn 'x|y' . || true`,
  "cd /x && grep -rn Foo src 2>&1 | head -5\nls",
  // substitutions, subshells and heredoc bodies stay whole inside their segment
  `f=$(rg -l "a; b" src | head -1); echo "$(date; ls)" && (cd y; ls); rg Foo`,
  "cat <<'EOF' | wc -l; echo x\n;; && rg Foo\nEOF\nrg Foo",
]) {
  const parts = splitChain(cmd);
  check(`split rejoins ${JSON.stringify(cmd)}`, parts.map((p) => p.text + p.sep).join("") === cmd);
}
for (const cmd of ["echo $(rg Foo", "rg Foo &", "cat <<EOF\nFoo", "echo 'open"]) {
  check(`split refuses ${JSON.stringify(cmd)}`, splitChain(cmd) === null);
}
const plans = {
  "rg Foo src | head -30": { idents: ["Foo"], only: true },
  "cd /x && grep -rn \"Foo\" src --include=*.ts 2>/dev/null | head -n 20": { idents: ["Foo"], only: true },
  'echo "=== callers ==="; grep -rn Foo src; rg Bar': { idents: ["Foo", "Bar"], only: false },
  "ls prisma/ ; grep -rln OrderItem prisma/ | head": { idents: ["OrderItem"], only: false },
  "rg Foo src | grep -v test | head": { idents: ["Foo"], only: true },
  "grep -rn Foo src | grep -v '\\.test\\.' | grep -vE \"spec|mock\"": { idents: ["Foo"], only: true },
  "rg -l Foo src | grep -v test | sort -u | head": { idents: ["Foo"], only: true },
  "rg Foo src | sort -k2": null,
  "rg Foo src | grep import": null,
  "rg Foo src | wc -l": { idents: ["Foo"], only: true },
  "grep -rn Foo src | cut -c1-170": { idents: ["Foo"], only: true },
  "grep -rn Foo src 2>/dev/null | cut -c1-170 | head -12": { idents: ["Foo"], only: true },
  "grep -rln Foo src | cut -d':' -f1 | sort -u | wc -l": { idents: ["Foo"], only: true },
  // what reads on from the grep stays raw: a file, a positive filter, another program, another ref
  "rg Foo src > /tmp/out.txt": null,
  "rg Foo src | cut -c1-170 > /tmp/out.txt": null,
  "rg Foo src | wc -c": null,
  "rg Foo src | awk '{print $1}'": null,
  "rg Foo src | sed -n 1,5p": null,
  "git grep -n Foo origin/main -- src | cut -c1-170": null,
  "git show origin/main:src/a.ts | grep -n Foo": null,
  "grep -rn 'Foo\\|Bar' src; ls": { idents: ["Foo|Bar"], only: false },
  // a grep inside a substitution or a heredoc body is never a segment of its own
  "echo $(rg Foo)": null,
  "cat <<EOF\nrg Foo\nEOF": null,
  "f=$(rg -l Bar src | head -1); rg Foo src": { idents: ["Foo"], only: false },
  // a literal assignment is followed; anything else in a `$` is not
  'd="src/a b"\nrg Foo "$d"': { idents: ["Foo"], only: false },
  "d=$(pwd); rg Foo $d": null,
  "for d in a b; do rg Foo $d; done": null,
  "cd $HOME; rg Foo": null,
  "rg -i Foo; ls": null,
  "cat a | grep Foo": null,
};
for (const [cmd, want] of Object.entries(plans)) {
  const got = planBash(cmd, "/r");
  const shape = got ? { idents: got.hits.map((h) => h.idents.join("|")), only: got.only } : null;
  check(`plan ${JSON.stringify(cmd)} => ${JSON.stringify(shape)}`, JSON.stringify(shape) === JSON.stringify(want));
}
check("plan tracks cd", planBash("cd /x; rg Foo sub; ls", "/r").hits[0].dir === "/x/sub");
check("plan expands a literal variable", planBash('d="src/a b"; rg Foo "$d"', "/r").hits[0].dir === "/r/src/a b");

// --- text fold: a grep for matching lines, optionally cut by head / cut -c ---
const folds = {
  "rg -n Foo src": ["rg -n Foo src", ""],
  "grep -rn 'a|b' src 2>/dev/null | head -20": ["grep -rn 'a|b' src 2>/dev/null", "| head -20"],
  'grep -rn "x > y (z)" src | cut -c1-170 | head': ['grep -rn "x > y (z)" src', "| cut -c1-170 | head"],
  "rg -n Foo $DIR": ["rg -n Foo $DIR", ""],
  "rg -n 'cost$' src": ["rg -n 'cost$' src", ""],
  // output that is not matching lines
  "rg -l Foo src": null,
  "grep -rc Foo src": null,
  "rg -on 'a.b' src": null,
  "grep -rn -A2 Foo src": null,
  "rg -C3 Foo": null,
  "rg --json Foo": null,
  "rg -n Foo -r Bar": null,
  // another reader, a file, a substitution, another program
  "rg Foo src | wc -l": null,
  "rg Foo src | grep import": null,
  "rg Foo src | cut -d: -f1": null,
  "rg Foo src | sort": null,
  "rg Foo src > out.txt": null,
  "rg Foo src | head > out.txt": null,
  "rg Foo $(cat list)": null,
  "git grep -n Foo": null,
  "cat a | grep Foo": null,
};
for (const [cmd, want] of Object.entries(folds)) {
  const got = foldable(cmd);
  check(`fold ${JSON.stringify(cmd)} => ${JSON.stringify(got)}`, JSON.stringify(got && [got.head, got.tail]) === JSON.stringify(want));
}
// One file: grep prints no path, so the hook adds `-H` and names the file for fold to strip.
const files = {
  "grep -nE 'a|b' $F | cut -c1-140 | head -40": "$F",
  'grep -n "x y" src/a.ts': "src/a.ts",
  "grep -n -e Foo src/a.ts 2>/dev/null": "src/a.ts",
  "grep -rn Foo src": null,
  "grep -n Foo src/a.ts src/b.ts": null,
  "grep -n Foo src/*.ts": null,
  "grep -n -m 5 Foo src/a.ts": null,
  "rg -n Foo src/a.ts": null,
};
for (const [cmd, want] of Object.entries(files)) check(`fold file ${JSON.stringify(cmd)} => ${want}`, foldable(cmd)?.file === want);
const tails = {
  "": "",
  "| head -30": " --head 30",
  "| head": " --head 10",
  "| head -n 40": " --head 40",
  "| head -n -5": "",
  "| cut -c1-150 | head -40": " --head 40 --cut 1-150",
  "| head -50 | head -20": " --head 20",
};
for (const [tail, want] of Object.entries(tails)) check(`tail ${JSON.stringify(tail)} => ${JSON.stringify(want)}`, tailFlags(tail) === want);
check("no fold outside a scope", planBash("rg -n 'a.b' src", "/r") === null);

// --- end to end on a fixture repo ---
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "cs-hook-"));
const repo = path.join(tmp, "repo");
const plain = path.join(tmp, "plain");
fs.mkdirSync(path.join(repo, "src"), { recursive: true });
fs.mkdirSync(plain);
fs.mkdirSync(path.join(repo, "lib"));
fs.writeFileSync(path.join(repo, "src", "a.ts"), "export interface Pet { name: string }\nexport function helper() { return 1; }\nexport function run() { return helper(); }\nconst label = 1;\n");
// `label` is a word far more than it is a symbol: 13 files use it as a field.
for (let i = 0; i < 13; i++) fs.writeFileSync(path.join(repo, "lib", `w${i}.ts`), `export const w${i} = { label: ${i} };\n`);
// One function holding 30 lines of the same word: a grep result worth folding.
fs.writeFileSync(
  path.join(repo, "src", "big.ts"),
  `export function big() {\n${Array.from({ length: 30 }, (_, i) => `  const needleWord${i} = ${i}; // padding padding`).join("\n")}\n}\n`,
);
fs.writeFileSync(path.join(plain, "a.ts"), "export function helper() { return 1; }\n");
spawnSync(BIN, ["ensure"], { cwd: repo });

// HOME holds its own `.codescratch/` (global config) and must never count as a repo.
fs.mkdirSync(path.join(tmp, ".codescratch"));
const env = { ...process.env, CODESCRATCH_BIN: BIN, CLAUDE_HOOKS_STATE_DIR: path.join(tmp, "state"), HOME: tmp };
const run = (payload) => {
  const r = spawnSync("node", [HOOK], { input: JSON.stringify(payload), env, encoding: "utf8" });
  check(`exit 0 for ${payload.hook_event_name}`, r.status === 0);
  return r.stdout ? JSON.parse(r.stdout) : null;
};
const grep = (cwd, pattern, session = "s1") =>
  run({ hook_event_name: "PreToolUse", session_id: session, cwd, tool_name: "Grep", tool_input: { pattern } });

const start = run({ hook_event_name: "SessionStart", session_id: "s1", cwd: repo });
check("session note inside scope", /^codescratch graph active/.test(start?.hookSpecificOutput?.additionalContext || ""));
check("no session note outside scope", run({ hook_event_name: "SessionStart", session_id: "s2", cwd: plain }) === null);

const hit = grep(repo, "helper");
const reason = hit?.hookSpecificOutput?.permissionDecisionReason || "";
check("hit denies", hit?.hookSpecificOutput?.permissionDecision === "deny");
check("hit carries the brief answer", /## .*helper/.test(reason) && /- run  src\/a\.ts/.test(reason) && !reason.includes("return 1"));
check("repeat of a served grep runs", grep(repo, "helper") === null);
// A name nothing uses is still a whole answer: the definition, and no other file mentions it.
const pet = grep(repo, "Pet")?.hookSpecificOutput?.permissionDecisionReason || "";
check("unused type is answered", /## interface `Pet`/.test(pet) && pet.includes("no other file mentions this name"));
const both = grep(repo, "helper\\|run", "s6")?.hookSpecificOutput?.permissionDecisionReason || "";
check("alternation answers every name", /## .*helper/.test(both) && /## .*`run`/.test(both) && both.match(/^trust: /gm).length === 1);
check("alternation with one unknown name runs", grep(repo, "helper\\|nothingHere", "s6") === null);
check("a word used outside the graph runs", grep(repo, "label") === null);
check("miss runs", grep(repo, "nothingHere") === null);
check("TODO runs", grep(repo, "TODO") === null);
check("outside scope silent", grep(plain, "helper") === null);

const first = grep(repo, "helper", "s3");
check("first touch mid-session adds the note", /^codescratch graph active/.test(first?.hookSpecificOutput?.additionalContext || ""));

// Bash: a lone symbol grep with an output trim is denied like the Grep tool.
const sh = (command, session) =>
  run({ hook_event_name: "PreToolUse", session_id: session, cwd: repo, tool_name: "Bash", tool_input: { command, description: "d" } });
const lone = sh("grep -rn helper src 2>/dev/null | grep -v test | head -20", "s4");
check("bash lone grep denies", lone?.hookSpecificOutput?.permissionDecision === "deny");
check("file-scoped grep is not denied", sh("grep -n helper src/a.ts; ls", "s4")?.hookSpecificOutput?.permissionDecision === undefined);
check("several directories of one repo deny", sh("grep -rn helper src lib", "s7")?.hookSpecificOutput?.permissionDecision === "deny");
check("a guessed directory that is absent is ignored", sh("grep -rn helper src app 2>/dev/null", "s9")?.hookSpecificOutput?.permissionDecision === "deny");
check("a file among the paths is not denied", sh("grep -rn helper src/a.ts lib", "s8")?.hookSpecificOutput?.permissionDecision === undefined);

// Bash batch: only the hit segment is rewritten, the rest runs, and no decision is taken.
const batch = `echo BEFORE; rg -n "helper\\(" src | head -3 && rg -n nothingHere src; echo AFTER`;
const rw = sh(batch, "s5")?.hookSpecificOutput;
check("batch takes no permission decision", rw && rw.permissionDecision === undefined);
check("batch keeps other tool_input fields", rw?.updatedInput?.description === "d");
const cmd = rw?.updatedInput?.command || "";
check("batch rewrites the hit segment, folds the other grep", /^echo BEFORE; cat '[^']+' && rg -n nothingHere src \| '[^']+' fold --tag [0-9a-f]{12} --log '[^']+'; echo AFTER$/.test(cmd));
check("answer file sits in the repo's .codescratch", cmd.includes(`cat '${path.join(repo, ".codescratch", "answers")}`));
const ran = spawnSync("bash", ["-c", cmd], { cwd: repo, encoding: "utf8" });
check("rewritten batch runs the answer and the rest", /BEFORE[\s\S]*answered from the graph[\s\S]*## [\s\S]*helper[\s\S]*AFTER/.test(ran.stdout));
const again = sh(batch, "s5")?.hookSpecificOutput?.updatedInput?.command || "";
check("repeat batch runs the symbol grep raw", again.startsWith(`echo BEFORE; rg -n "helper\\(" src | head -3 && rg -n nothingHere src | '`));
check("Grep tool is never folded", grep(repo, "needleWord\\d+", "f0") === null);

// Text fold: the grep runs, `codescratch fold` reads it.
const exec = (command) => spawnSync("bash", ["-c", command], { cwd: repo, encoding: "utf8" });
const big = "grep -rn 'needleWord[0-9]' src 2>/dev/null | head -50";
const f1 = sh(big, "f1")?.hookSpecificOutput;
const f1cmd = f1?.updatedInput?.command || "";
check("fold takes no permission decision", f1 && f1.permissionDecision === undefined);
check("fold sits between the grep and its head", /^grep -rn 'needleWord\[0-9\]' src 2>\/dev\/null \| '[^']+' fold --tag [0-9a-f]{12} --log '[^']+' --head 50 \| head -50$/.test(f1cmd));
const f1out = exec(f1cmd).stdout;
check("large result comes back grouped", /^fold: 30 hits in 1 files/.test(f1out) && /1-32 function big ×30 L2,/.test(f1out) && f1out.length < exec(big).stdout.length);
check("repeat of a folded grep runs raw", sh(big, "f1") === null);
check("the run after the raw one folds again", sh(big, "f1")?.hookSpecificOutput?.updatedInput?.command === f1cmd);
// A symbol the graph cannot answer for falls through to the fold; 13 one-hit files do not shrink.
const lab = sh("grep -rn label lib", "f2")?.hookSpecificOutput?.updatedInput?.command || "";
check("unanswered symbol grep is folded", / fold --tag /.test(lab));
check("a result folding would not shrink is unchanged", exec(lab).stdout === exec("grep -rn label lib").stdout && exec(lab).stdout.length > 500);
check("a grep that was not folded is not a repeat", sh("grep -rn label lib", "f2")?.hookSpecificOutput?.updatedInput?.command === lab);
// One file: `-H` lets the fold place each hit; a pass-through prints what grep alone would.
const one = "grep -n 'needleWord[0-9]' src/big.ts | head -50";
const onecmd = sh(one, "f4")?.hookSpecificOutput?.updatedInput?.command || "";
check("one-file grep gets -H and --strip-path", /^grep -H -n 'needleWord\[0-9\]' src\/big\.ts \| '[^']+' fold --tag [0-9a-f]{12} --log '[^']+' --strip-path src\/big\.ts --head 50 \| head -50$/.test(onecmd));
check("one-file grep comes back grouped", /^fold: 30 hits in 1 files/.test(exec(onecmd).stdout));
// Capped by the tail: 8 raw lines are smaller than the fold's grouping would be after the cut.
const capped = "grep -n 'needleWord[0-9]' src/big.ts | cut -c1-30 | head -8";
const cappedcmd = sh(capped, "f8")?.hookSpecificOutput?.updatedInput?.command || "";
check("capped grep names its limits", / --head 8 --cut 1-30 \| cut -c1-30 \| head -8$/.test(cappedcmd));
check("capped grep below the pass size prints what grep prints", exec(cappedcmd).stdout === exec(capped).stdout);
const small = "grep -n helper src/a.ts";
const smallcmd = sh(small, "f7")?.hookSpecificOutput?.updatedInput?.command || "";
check("small one-file grep prints exactly what grep prints", / --strip-path src\/a\.ts$/.test(smallcmd) && exec(smallcmd).stdout === exec(small).stdout);
const none = sh("grep -rn nothingHere src && echo FOUND; echo AFTER", "f3")?.hookSpecificOutput?.updatedInput?.command || "";
check("no match still fails the chain", exec(none).stdout === "AFTER\n");
// A binary without `fold` would swallow the grep's output: such a grep is left alone.
const old = path.join(tmp, "old-bin");
fs.writeFileSync(old, "#!/bin/sh\nexit 2\n", { mode: 0o755 });
const stale = spawnSync("node", [HOOK], {
  input: JSON.stringify({ hook_event_name: "PreToolUse", session_id: "f5", cwd: repo, tool_name: "Bash", tool_input: { command: big } }),
  env: { ...env, CODESCRATCH_BIN: old },
  encoding: "utf8",
});
check("binary without fold leaves the grep alone", stale.status === 0 && stale.stdout === "");
check("a repeat of a symbol answer is raw, not folded", sh("grep -rn helper src", "f4")?.hookSpecificOutput?.permissionDecision === "deny" && sh("grep -rn helper src", "f4") === null);

fs.rmSync(tmp, { recursive: true, force: true });
if (failed) process.exit(1);
console.log("claude-hook OK");
