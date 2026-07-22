#!/usr/bin/env node
// Codex `notify` shim -> the shared .harness/status.json (architecture §4.5, A4).
//
// Codex invokes the configured notify program with the event as a JSON string
// argument. In TUI mode the only event it ever sends is `agent-turn-complete`
// — there is NO approval-blocked event (R11). So this shim can honestly report
// "the turn ended" and nothing else, which is why the Codex adapter declares
// WaitFidelity::BestEffort and never raises a "needs input" notification.
//
// Same two rules as the Claude Code shim: never fail loudly, always write
// atomically.

import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";

const CWD = process.env.CODEX_PROJECT_DIR || process.cwd();

/**
 * Map a Codex notify event to one of running | waiting | idle.
 *
 * Only `agent-turn-complete` is documented for TUI mode; anything else is
 * treated as "the agent is alive and working" rather than guessed at.
 * Exported for the test harness.
 */
export function classify(payload) {
	const type = String(payload?.type ?? payload?.["event"] ?? "").toLowerCase();
	if (type.includes("turn-complete") || type.includes("turn_complete")) return "idle";
	// Deliberately never returns "waiting": Codex's TUI exposes no
	// approval-blocked event, and inventing one would raise false
	// "input needed" notifications.
	return "running";
}

export function writeStatus(cwd, state) {
	try {
		const dir = path.join(cwd, ".harness");
		fs.mkdirSync(dir, { recursive: true });
		const payload =
			JSON.stringify({ state, timestamp: new Date().toISOString(), sessionFile: null }, null, "\t") +
			"\n";
		const tmp = path.join(dir, `.status-${process.pid}-${crypto.randomBytes(4).toString("hex")}.tmp`);
		fs.writeFileSync(tmp, payload, "utf8");
		fs.renameSync(tmp, path.join(dir, "status.json"));
	} catch {
		// Never let status bookkeeping break the agent.
	}
}

if (import.meta.url === `file://${process.argv[1]}`) {
	// The notify hook sits in the user's GLOBAL codex config, so this shim
	// runs for every codex everywhere. Only harness-spawned codex processes
	// carry this marker (set on the pty env and inherited); without it,
	// exit before touching the filesystem — a codex run in a random repo
	// must not grow a .harness directory.
	if (process.env.HARNESS_MANAGED !== "1") process.exit(0);
	let payload = {};
	try {
		// Codex passes the event as a single JSON argument.
		payload = JSON.parse(process.argv[2] ?? "{}");
	} catch {
		/* unparseable — fall through to "running" */
	}
	// Attribution: codex spawns notify with its own cwd — the worktree the
	// harness launched it in.
	writeStatus(CWD, classify(payload));
	process.exit(0);
}
