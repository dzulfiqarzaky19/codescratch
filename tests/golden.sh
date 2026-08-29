#!/usr/bin/env bash
# End-to-end golden test: builds a tiny TS fixture repo, runs the binary,
# asserts the trust banner + honest edge labels + blast radius.
# Usage: rust/tests/golden.sh [path-to-codescratch-binary]
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
BIN="${1:-$here/../target/debug/codescratch}"
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"
[ -x "$BIN" ] || { echo "FAIL: binary not found/executable: $BIN"; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/src"

cat > "$tmp/src/util.ts" <<'EOF'
export function helper(x: number): number {
  return x + 1;
}
EOF
cat > "$tmp/src/main.ts" <<'EOF'
import { helper } from "./util";
export function run(n: number): number {
  return helper(n);
}
EOF

git -C "$tmp" init -q
git -C "$tmp" add -A
git -C "$tmp" -c user.email=t@t -c user.name=t commit -qm init

pass() { echo "  ok: $1"; }
fail() { echo "  FAIL: $1"; echo "----"; echo "$2"; exit 1; }

# 1. init → trust: fresh
out="$("$BIN" init "$tmp")"
echo "$out" | grep -q "trust: fresh"       || fail "init not fresh" "$out"
echo "$out" | grep -q "coverage: exhaustive" || fail "coverage not exhaustive" "$out"
pass "init reports trust: fresh · coverage: exhaustive"

# 2. explore helper → cross-file caller resolved via import-binding, blast shows main.ts
exp="$("$BIN" explore helper --path "$tmp")"
echo "$exp" | grep -q "## function .helper."  || fail "explore header missing" "$exp"
echo "$exp" | grep -q "import-binding"         || fail "no import-binding caller" "$exp"
echo "$exp" | grep -q "src/main.ts"            || fail "blast radius missing main.ts" "$exp"
pass "explore helper resolves import-binding caller + blast radius"

# 3. explore run → callee helper labeled import-binding (strong, not faked)
exp2="$("$BIN" explore run --path "$tmp")"
echo "$exp2" | grep -q "helper"                || fail "run's callee helper missing" "$exp2"
echo "$exp2" | grep -q "import-binding"         || fail "callee not import-binding" "$exp2"
pass "explore run shows strong import-binding callee"

# 4. search FTS finds exported symbol
s="$("$BIN" search helper --path "$tmp")"
echo "$s" | grep -q "helper"                    || fail "search miss" "$s"
pass "search (FTS5) finds helper"

# 5. MCP stdio: initialize + tools/list returns exactly explore + status
mcp="$(printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | "$BIN" mcp "$tmp")"
echo "$mcp" | grep -q '"name":"explore"'        || fail "explore tool not listed" "$mcp"
echo "$mcp" | grep -q '"name":"status"'         || fail "status tool not listed" "$mcp"
echo "$mcp" | grep -q '"name":"search"'         && fail "search should be hidden by default" "$mcp"
pass "MCP lists exactly explore + status"

# 6. tsconfig alias + export * barrel resolve as import-binding (WP-2A)
mkdir -p "$tmp/src/lib" "$tmp/src/services"
cat > "$tmp/tsconfig.json" <<'EOF'
{
  "compilerOptions": {
    "baseUrl": ".",
    "paths": { "@/*": ["src/*"] }
  }
}
EOF
cat > "$tmp/src/lib/math.ts" <<'EOF'
export function add(a: number, b: number): number { return a + b; }
EOF
cat > "$tmp/src/lib/barrel.ts" <<'EOF'
export * from "./math.js";
EOF
cat > "$tmp/src/services/via.ts" <<'EOF'
import { add as plus } from "@/lib/barrel.js";
export function viaBarrel(a: number, b: number): number { return plus(a, b); }
EOF
git -C "$tmp" add -A
git -C "$tmp" -c user.email=t@t -c user.name=t commit -qm alias
"$BIN" ensure "$tmp" >/dev/null
exp3="$("$BIN" explore add --path "$tmp")"
echo "$exp3" | grep -q "import-binding" || fail "alias/barrel not import-binding" "$exp3"
echo "$exp3" | grep -q "via.ts"          || fail "viaBarrel caller missing from blast" "$exp3"
pass "alias + export* barrel resolves import-binding"

# 7. explore shows call-path spine + depth-grouped blast (WP-2D)
echo "$exp3" | grep -q "call-path spine" || fail "call-path spine missing" "$exp3"
echo "$exp3" | grep -q "depth 1"         || fail "depth-grouped blast missing" "$exp3"
pass "explore payload has spine + depth-grouped blast"

# 8. Express route plugin (WP-3C)
cat > "$tmp/src/app.ts" <<'EOF'
import express from "express";
const app = express();
export function listUsers() {}
app.get("/users", listUsers);
EOF
git -C "$tmp" add -A
git -C "$tmp" -c user.email=t@t -c user.name=t commit -qm routes
"$BIN" ensure "$tmp" >/dev/null
exp4="$("$BIN" explore listUsers --path "$tmp")"
echo "$exp4" | grep -q "handles_route" || fail "route edge missing" "$exp4"
echo "$exp4" | grep -q "/users"        || fail "route path missing" "$exp4"
pass "express plugin emits route + handles_route"

echo "GOLDEN OK"
