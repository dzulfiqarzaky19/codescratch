#!/usr/bin/env bash
# Benchmark harness (WP-10B): times the hot paths against a synthetic corpus of a
# chosen size, so index/query cost is a number, not a vibe. No criterion crate —
# same "just run the binary" discipline as golden.sh, so it works from any static
# build with nothing but bash + coreutils.
#
# Usage: rust/tests/bench.sh [path-to-binary] [num-files]
#   BIN defaults to target/release/codescratch (build with `cargo build --release`).
#   num-files defaults to 2000 (~1 medium repo). Try 200 / 2000 / 10000.
#
# Reports, per phase, wall-clock ms (median of 3 where cheap enough) and the
# resulting graph size. Cold index = full rebuild; warm ensure = the dirty-gate
# no-op that SessionStart/PostToolUse hits on every untouched turn — that number
# is the one the host freshness loop pays repeatedly, so it matters most.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
BIN="${1:-$here/../target/release/codescratch}"
N="${2:-2000}"
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"
[ -x "$BIN" ] || { echo "FAIL: binary not found/executable: $BIN"; echo "build it: (cd rust && cargo build --release)"; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# --- portable millisecond clock (date +%s%N is GNU-only; fall back to python) ---
now_ms() {
  local ns
  ns="$(date +%s%N 2>/dev/null)" || ns=""
  if [ -n "$ns" ] && [ "$ns" != "$(date +%s)N" ]; then
    echo $(( ns / 1000000 ))
  else
    python3 -c 'import time; print(int(time.time()*1000))'
  fi
}

# time a command silently, echo elapsed ms
time_ms() {
  local start end
  start="$(now_ms)"
  "$@" >/dev/null 2>&1
  end="$(now_ms)"
  echo $(( end - start ))
}

# median of three runs of a command
median3() {
  local a b c
  a="$(time_ms "$@")"; b="$(time_ms "$@")"; c="$(time_ms "$@")"
  printf '%s\n' "$a" "$b" "$c" | sort -n | sed -n '2p'
}

# --- generate a synthetic TS corpus of N files with real cross-file call edges ---
# Each file f{i}.ts exports fn{i} which calls fn{i-1} imported from f{i-1}.ts,
# giving a long resolvable import-binding chain (exercises the resolver + blast).
echo "generating $N-file synthetic corpus in $tmp ..."
mkdir -p "$tmp/src"
{
  echo 'export function fn0(x: number): number { return x + 1; }'
} > "$tmp/src/f0.ts"
i=1
while [ "$i" -lt "$N" ]; do
  prev=$(( i - 1 ))
  cat > "$tmp/src/f${i}.ts" <<EOF
import { fn${prev} } from "./f${prev}";
export function fn${i}(x: number): number {
  return fn${prev}(x) + ${i};
}
export class C${i} {
  method${i}(v: number): number { return fn${prev}(v); }
}
EOF
  i=$(( i + 1 ))
done
git -C "$tmp" init -q
git -C "$tmp" add -A
git -C "$tmp" -c user.email=b@b -c user.name=b commit -qm corpus

files_on_disk="$(find "$tmp/src" -name '*.ts' | wc -l | tr -d ' ')"

echo
echo "=== codescratch benchmark ==="
echo "binary : $BIN"
echo "corpus : $files_on_disk files"
echo

# 1. cold index (full rebuild from empty) — the worst case
rm -rf "$tmp/.codescratch"
cold="$(time_ms "$BIN" init "$tmp")"

# 2. warm ensure (dirty-gate no-op: nothing changed since index) — hot host path
warm="$(median3 "$BIN" ensure "$tmp")"

# 3. reindex (forced full rebuild, lock + rewrite) — emergency path
reidx="$(time_ms "$BIN" reindex "$tmp")"

# 4. explore a mid-chain symbol (deep blast radius walk)
mid=$(( N / 2 ))
expl="$(median3 "$BIN" explore "fn${mid}" --path "$tmp")"

# 5. search (FTS / hybrid)
srch="$(median3 "$BIN" search "method" --path "$tmp")"

# --- graph size from the trust banner / status ---
status_out="$("$BIN" status "$tmp" 2>/dev/null || true)"

printf '%-28s %8s\n' "phase" "ms"
printf '%-28s %8s\n' "----------------------------" "--------"
printf '%-28s %8s\n' "cold index (full rebuild)" "$cold"
printf '%-28s %8s\n' "warm ensure (dirty no-op)" "$warm"
printf '%-28s %8s\n' "reindex (forced rebuild)" "$reidx"
printf '%-28s %8s\n' "explore fn${mid} (blast walk)" "$expl"
printf '%-28s %8s\n' "search method" "$srch"
echo
echo "--- graph (from status) ---"
echo "$status_out" | head -5
echo
echo "db size: $(du -h "$tmp/.codescratch/graph.db" 2>/dev/null | cut -f1)"
echo "BENCH OK"
