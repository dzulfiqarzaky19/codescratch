/**
 * codescratch host for Pi: session watch + catch-up ensure + high-confidence
 * symbol-grep rewrite. No MCP.
 *
 * session_start  → ensure (dirty-gate) + spawn `codescratch watch` (cwd-scoped)
 * session_shutdown → kill that watch
 * tool_call grep → identifier pattern blocked with the explore/search command
 * tool_call bash → `rg`/`grep` of a bare identifier rewritten to `codescratch explore`
 */

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const IDENT = /^[A-Za-z_][A-Za-z0-9_]*$/;

function bin(): string | null {
	const fromEnv = process.env.CODESCRATCH_BIN;
	if (fromEnv && existsSync(fromEnv)) return fromEnv;
	const fallback = join(homedir(), ".local/bin/codescratch");
	if (existsSync(fallback)) return fallback;
	return "codescratch";
}

function kick(args: string[], cwd: string): ChildProcess | null {
	const b = bin();
	if (!b) return null;
	const child = spawn(b, args, {
		cwd,
		detached: true,
		stdio: "ignore",
	});
	child.unref();
	return child;
}

function identifierFromRg(command: string): string | null {
	// rg/grep/ag of a single argv token that looks like a symbol, no -e/-i/-P/-F flags
	// that imply string/regex search. Path args after `--` are fine.
	const trimmed = command.trim();
	const m = trimmed.match(/^(?:rg|grep|ag|ugrep)\s+(.+)$/);
	if (!m) return null;
	const rest = m[1];
	if (/(^|\s)-(e|i|P|F|R|w)\b/.test(rest)) return null;
	const tokens = rest.split(/\s+/).filter((t) => t && t !== "--");
	const positional = tokens.filter((t) => !t.startsWith("-"));
	if (positional.length !== 1) return null;
	const q = positional[0].replace(/^['"]|['"]$/g, "");
	return IDENT.test(q) ? q : null;
}

export default function (pi: ExtensionAPI) {
	let watch: ChildProcess | null = null;

	pi.on("session_start", (_event, ctx) => {
		kick(["ensure", ctx.cwd], ctx.cwd);
		watch = kick(["watch", ctx.cwd], ctx.cwd);
	});

	pi.on("session_shutdown", () => {
		if (watch?.pid) {
			try {
				process.kill(-watch.pid, "SIGTERM");
			} catch {
				try {
					process.kill(watch.pid, "SIGTERM");
				} catch {
					/* already gone */
				}
			}
		}
		watch = null;
	});

	pi.on("tool_call", (event) => {
		const input = event.input as Record<string, unknown>;
		if (event.toolName === "grep") {
			const pattern = String(input.pattern ?? "");
			if (input.ignoreCase) return;
			if (!IDENT.test(pattern)) return;
			return {
				block: true,
				reason: `codescratch: use \`codescratch explore ${pattern}\` (or \`search ${pattern}\`) instead of grep for a symbol. grep is for strings/regex/TODO.`,
			};
		}
		if (event.toolName === "bash") {
			const q = identifierFromRg(String(input.command ?? ""));
			if (!q) return;
			input.command = `codescratch explore ${q}`;
		}
	});
}
