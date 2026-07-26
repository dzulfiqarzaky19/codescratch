import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";

/**
 * Current HEAD sha, or null if not a git work tree / git missing.
 * Fail-soft: never throws to callers.
 */
export function readHead(root: string): string | null {
  if (!existsSync(join(root, ".git")) && !looksLikeWorktree(root)) {
    // Still try rev-parse — worktrees may only have .git file; cheap fail.
  }
  try {
    const out = execFileSync("git", ["-C", root, "rev-parse", "HEAD"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
      timeout: 5000,
      windowsHide: true,
    });
    const sha = out.trim();
    return /^[0-9a-f]{7,40}$/i.test(sha) ? sha : null;
  } catch {
    return null;
  }
}

function looksLikeWorktree(root: string): boolean {
  try {
    return existsSync(join(root, ".git"));
  } catch {
    return false;
  }
}

/** Injectable for tests. */
let headReader: (root: string) => string | null = readHead;

export function getHead(root: string): string | null {
  return headReader(root);
}

export function setHeadReaderForTest(
  fn: ((root: string) => string | null) | null,
): void {
  headReader = fn ?? readHead;
}
