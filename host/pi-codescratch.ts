/**
 * codescratch host for Pi: session watch + catch-up ensure + the grep rules of the
 * Claude Code host. No MCP.
 *
 * session_start  → ensure (dirty-gate) + spawn `codescratch watch` (cwd-scoped)
 * session_shutdown → kill that watch
 * tool_call grep|bash → host/claude-codescratch.cjs decides (installed beside this file):
 *                   a symbol grep is blocked and the reason carries the graph's answer,
 *                   inside a batch it becomes a `cat` of that answer, and any other grep
 *                   for matching lines gets `| codescratch fold` appended
 */

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { homedir } from "node:os";
import { join } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

/** What the Claude Code hook prints for a PreToolUse event. */
type HookAnswer = {
	hookSpecificOutput?: {
		permissionDecision?: string;
		permissionDecisionReason?: string;
		updatedInput?: { command?: string };
	};
} | null;

const engine = createRequire(import.meta.url)("./claude-codescratch.cjs") as {
	decide(input: Record<string, unknown>, stateDir: string): HookAnswer;
};

/** Session state and fold logs, apart from the Claude Code hook's own. */
const STATE = join(homedir(), ".codescratch", "pi");

export type Decision = { block: string } | { command: string } | null;

/** One Pi `grep` or `bash` call → blocked with the graph's answer, a rewritten command, or
 *  null when the call should run as written. */
export function decide(
	toolName: string,
	input: Record<string, unknown>,
	cwd: string,
	session: string,
	stateDir: string = STATE,
): Decision {
	const call =
		toolName === "grep"
			? { tool_name: "Grep", tool_input: { pattern: input.pattern, path: input.path, glob: input.glob, "-i": input.ignoreCase } }
			: toolName === "bash"
				? { tool_name: "Bash", tool_input: { command: input.command } }
				: null;
	if (!call) return null;
	const out = engine.decide({ hook_event_name: "PreToolUse", session_id: session, cwd, ...call }, stateDir)?.hookSpecificOutput;
	if (out?.permissionDecision === "deny" && out.permissionDecisionReason) return { block: out.permissionDecisionReason };
	const command = out?.updatedInput?.command;
	return command ? { command } : null;
}

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
		stdio: ["ignore", "ignore", "inherit"],
	});
	child.unref();
	return child;
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

	pi.on("tool_call", (event, ctx) => {
		const input = event.input as Record<string, unknown>;
		const got = decide(event.toolName, input, ctx.cwd, ctx.sessionManager.getSessionId());
		if (!got) return;
		if ("block" in got) return { block: true, reason: got.block };
		input.command = got.command;
	});
}
