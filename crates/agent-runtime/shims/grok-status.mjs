#!/usr/bin/env node
// Grok hook shim -> the shared .harness/status.json.
//
// Grok's project hooks are Claude-compatible: JSON on stdin, the same event
// names, and snake_case field aliases. This file reuses Claude Code's
// classifier and writes the same status file, stamped agent "grok".
//
// One Grok-specific edge: a Notification whose type is task_complete means the
// turn finished. The shared classifier treats every other Notification as
// waiting, which would pin the pill after a finished turn.
//
// Never fail loudly. A hook that errors makes the agent look broken, and
// status is bookkeeping. Every path exits 0.

import fs from "node:fs";
import { pathToFileURL } from "node:url";
import { classify as classifyClaude, toolFrom } from "./claude-code-status.mjs";
import { recordTab, writeStatusFiles } from "./remote-tab.mjs";

const CWD = process.env.CLAUDE_PROJECT_DIR || process.cwd();

function readStdin() {
	try {
		return fs.readFileSync(0, "utf8");
	} catch {
		return "";
	}
}

function pick(obj, ...names) {
	for (const n of names) {
		const v = n.split(".").reduce((o, k) => (o == null ? undefined : o[k]), obj);
		if (v !== undefined && v !== null && v !== "") return v;
	}
	return undefined;
}

/**
 * Map a Grok hook payload to running | waiting | idle.
 *
 * `task_complete` is idle. Everything else is Claude Code's mapping.
 *
 * Exported for the test harness.
 */
export function classify(payload) {
	const detail = String(
		pick(payload, "notification_type", "notificationType", "matcher", "message", "reason") ?? "",
	).toLowerCase();
	if (detail.includes("task_complete")) return "idle";
	return classifyClaude(payload);
}

export { toolFrom };

export function writeStatus(cwd, state, sessionFile, tool, sessionId) {
	try {
		const payload = writeStatusFiles(cwd, {
			state,
			agent: "grok",
			timestamp: new Date().toISOString(),
			sessionFile: sessionFile ?? null,
			tool: tool ?? null,
		});
		recordTab(payload, sessionId);
	} catch {
		// Never let status bookkeeping break the agent.
	}
}

// Only run when invoked as the hook, so the test harness can import the pure
// functions above without side effects.
if (process.argv[1] && fs.existsSync(process.argv[1]) &&
	import.meta.url === pathToFileURL(fs.realpathSync(process.argv[1])).href) {
	// Only a tab openOrchestra launched reports: a Grok session started
	// elsewhere in this worktree runs the same hooks (see
	// claude-code-status.mjs).
	if (!process.env.HARNESS_AGENT_TAB_ID && !process.env.HARNESS_AGENT_WORKTREE &&
		!process.env.HARNESS_REMOTE_SESSION) {
		process.exit(0);
	}
	let payload = {};
	try {
		payload = JSON.parse(readStdin() || "{}");
	} catch {
		/* unparseable payload — still record that something happened */
	}
	const sessionFile = pick(payload, "transcript_path", "transcriptPath", "session.transcript_path");
	writeStatus(CWD, classify(payload), sessionFile, toolFrom(payload), payload.session_id);
	process.exit(0);
}
