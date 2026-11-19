#!/usr/bin/env node
// Claude Code hook shim -> the shared .harness/status.json (architecture §4.5, A3).
//
// Claude Code invokes this from a project-local .claude/settings.json hook,
// passing the event payload as JSON on stdin. We translate it into exactly the
// three-state file pi's extension writes, so `crates/status-watch` consumes
// both agents unchanged and the sidebar pill is agent-neutral.
//
// Design rules, both learned from the pi extension:
//   - NEVER fail loudly. A hook that errors makes the agent look broken, and
//     status is bookkeeping. Every path exits 0.
//   - Writes are atomic (temp + rename), because a file watcher is reading
//     this concurrently and a half-written file is a parse error.
//
// Hook-schema drift (R12): the payload shape moved across Claude Code 2.1.x,
// so every field is read through a list of plausible spellings and falls back
// to a safe value rather than throwing.

import fs from "node:fs";
import path from "node:path";
import { recordTab, writeStatusFiles } from "./remote-tab.mjs";
import os from "node:os";
import { pathToFileURL } from "node:url";

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
 * Map a hook payload to one of running | waiting | idle.
 *
 * `Stop` ends the turn -> idle, and so does the 60s `idle_prompt` reminder. A
 * Notification about a permission or approval prompt means the agent is
 * blocked on the user -> waiting.
 * Anything else we can still attribute to a live session -> running.
 *
 * Exported for the test harness.
 */
export function classify(payload) {
	const event = String(
		pick(payload, "hook_event_name", "hookEventName", "event", "type") ?? "",
	).toLowerCase();
	const detail = String(
		pick(payload, "matcher", "notification_type", "notificationType", "message", "reason") ?? "",
	).toLowerCase();

	if (event.includes("stop") || event.includes("subagentstop")) return "idle";
	if (detail.includes("permission") || detail.includes("approval")) return "waiting";
	// Claude Code's `idle_prompt` reminder ("waiting for your input") fires 60s
	// after a turn ends. The turn is already done (Stop said so); it is not a
	// block, and as `waiting` it pinned the pill until the user typed again.
	if (detail.includes("idle") || detail.includes("waiting for your input")) return "idle";
	if (event.includes("notification")) return "waiting";
	return "running";
}

/**
 * The tool a PreToolUse hook is about to run, trimmed to what a status line
 * needs: the conductor shows "Editing main.rs" / "Running cargo test" from it
 * and matches `id` against the transcript to see when it has finished. Only
 * a few short input fields are kept, so the file never carries file contents
 * or long commands. Any other event has no tool -> null.
 *
 * Exported for the test harness.
 */
export function toolFrom(payload) {
	const event = String(pick(payload, "hook_event_name", "hookEventName", "event") ?? "").toLowerCase();
	if (event !== "pretooluse") return null;
	const name = pick(payload, "tool_name", "toolName", "tool.name");
	if (typeof name !== "string") return null;
	const raw = pick(payload, "tool_input", "toolInput", "tool.input") ?? {};
	const input = {};
	for (const [key, max] of [["file_path", 400], ["notebook_path", 400], ["command", 160], ["pattern", 80]]) {
		if (typeof raw[key] === "string") input[key] = raw[key].slice(0, max);
	}
	const id = pick(payload, "tool_use_id", "toolUseId", "tool.id");
	return { name, id: typeof id === "string" ? id : null, input };
}

export function writeStatus(cwd, state, sessionFile, tool, sessionId) {
	try {
		const payload = writeStatusFiles(cwd, {
			state,
			agent: "claude-code",
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
	let payload = {};
	try {
		payload = JSON.parse(readStdin() || "{}");
	} catch {
		/* unparseable payload — still record that something happened */
	}
	// transcript_path is the harvesting hook (§6.4): the one peer agent that
	// exposes a session transcript we could train from.
	let sessionFile = pick(payload, "transcript_path", "transcriptPath", "session.transcript_path");
	if (!sessionFile && /^[a-f\d-]{36}$/i.test(payload.session_id ?? "")) {
		const config = process.env.CLAUDE_CONFIG_DIR || path.join(os.homedir(), ".claude");
		const project = CWD.replace(/[^a-z0-9]/gi, "-");
		const candidate = path.join(config, "projects", project, payload.session_id + ".jsonl");
		if (fs.existsSync(candidate)) sessionFile = candidate;
	}
	writeStatus(CWD, classify(payload), sessionFile, toolFrom(payload), payload.session_id);
	process.exit(0);
}
