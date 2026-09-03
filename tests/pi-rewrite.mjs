#!/usr/bin/env node
// Regression: symbol-grep rewrite must not steal string/TODO searches.
import { identifierFromRg } from "../host/pi-codescratch.ts";

const want = {
  "rg helper": "helper",
  "rg Foo": "Foo",
  "grep helper": "helper",
  "rg TODO": null,
  "rg FIXME": null,
  "rg HACK": null,
  "rg XXX": null,
  "rg todo": null,
  "rg Todo": null,
  "rg -i TODO": null,
  "rg helper src/": null,
  "rg -e helper": null,
};

let failed = 0;
for (const [cmd, expected] of Object.entries(want)) {
  const got = identifierFromRg(cmd);
  if (got !== expected) {
    console.error(`FAIL ${JSON.stringify(cmd)} => ${JSON.stringify(got)} want ${JSON.stringify(expected)}`);
    failed++;
  }
}
if (failed) {
  process.exit(1);
}
console.log("ok: pi-rewrite leaves TODO/FIXME to grep, rewrites helper");
