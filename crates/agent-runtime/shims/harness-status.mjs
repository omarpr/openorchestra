#!/usr/bin/env node
// Universal harness status shim -> shared .harness/status.json (§4.5).
//
// Any terminal agent whose hooks can invoke a command can drive the sidebar
// pill and OS notifications through this file. Two invocation styles:
//
//   1. Explicit state:  node harness-status.mjs running|waiting|idle
//   2. Claude-/Codex-shaped JSON on stdin (same classify as claude-code-status.mjs)
//
// The agent slug stamped into status.json comes from $HARNESS_AGENT (set by
// ConfigurableRuntime on spawn). Without $HARNESS_MANAGED=1 or $HARNESS_AGENT
// the shim exits without writing — so hooks left in a project (or in the
// shared ~/.codex/hooks.json) do not litter repos / race built-in adapters
// when the agent runs outside the harness or as a different kind.
//
// Worktree attribution (Claude vs Codex):
//   CLAUDE_PROJECT_DIR → payload.cwd → CODEX_PROJECT_DIR → process.cwd()
//
// Never fail loudly: status is bookkeeping; a hook that errors makes the
// agent look broken. Writes are atomic (temp + rename).

import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { writeStatusFiles } from "./remote-tab.mjs";

const AGENT = process.env.HARNESS_AGENT;

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
 * Resolve the worktree the status file belongs to.
 * Claude sets CLAUDE_PROJECT_DIR; Codex passes `cwd` on the hook payload
 * (and sometimes CODEX_PROJECT_DIR). Falling back to process.cwd() alone
 * would write outside the watched worktree when Codex launches the hook
 * from a different cwd.
 */
export function resolveCwd(payload) {
	return (
		process.env.CLAUDE_PROJECT_DIR ||
		pick(payload ?? {}, "cwd") ||
		process.env.CODEX_PROJECT_DIR ||
		process.env.CURSOR_PROJECT_DIR ||
		pick(payload ?? {}, "workspace_roots.0") ||
		process.cwd()
	);
}

/**
 * Map a Claude-compatible hook payload to running | waiting | idle.
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

export function writeStatus(cwd, state, agent, sessionFile) {
	try {
		writeStatusFiles(cwd, {
			state,
			agent: agent ?? "agent",
			timestamp: new Date().toISOString(),
			sessionFile: sessionFile ?? null,
		});
	} catch {
		// Never let status bookkeeping break the agent.
	}
}

function parseExplicitState(arg) {
	const s = String(arg ?? "").toLowerCase();
	if (s === "running" || s === "waiting" || s === "idle") return s;
	return null;
}

function isMainModule() {
	if (!process.argv[1]) return false;
	try {
		return fileURLToPath(import.meta.url) === fs.realpathSync(process.argv[1]);
	} catch {
		return false;
	}
}

if (isMainModule()) {
	if (process.env.HARNESS_MANAGED !== "1") process.exit(0);
	// Built-in Codex only sets HARNESS_MANAGED; requiring HARNESS_AGENT keeps
	// this universal shim from racing codex-hook.mjs when both sit in
	// ~/.codex/hooks.json.
	if (!AGENT) process.exit(0);

	const explicit = parseExplicitState(process.argv[2]);
	let state;
	let sessionFile = null;
	let payload = {};
	if (explicit) {
		state = explicit;
	} else {
		try {
			payload = JSON.parse(readStdin() || process.argv[2] || "{}");
		} catch {
			/* unparseable — still record that something happened as running */
		}
		state = classify(payload);
		sessionFile = pick(payload, "transcript_path", "transcriptPath", "session.transcript_path");
	}
	writeStatus(resolveCwd(payload), state, AGENT, sessionFile);
	process.exit(0);
}
