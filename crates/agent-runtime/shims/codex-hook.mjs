#!/usr/bin/env node
// Codex hook shim -> the shared .harness/status.json (architecture §4.5, A4).
//
// Codex 0.145 grew a Claude-Code-style hooks system: it invokes configured
// hook commands with the event payload as JSON on stdin, tagged with a
// `hook_event_name`. Unlike the old `notify` program (which only ever fired
// `agent-turn-complete` in TUI mode — turn END, never turn START), hooks give
// us BOTH edges:
//
//   UserPromptSubmit -> a turn just started  -> running
//   Stop             -> the turn finished    -> idle
//
// That turn-START edge is the whole point: without it the pill was stuck on
// IDLE the entire time codex was working, because nothing ever wrote "running"
// for the second and later turns. See crates/agent-runtime/src/codex.rs.
//
// Same two rules as the pi / Claude Code shims: never fail loudly (status is
// bookkeeping — a hook that throws makes the agent look broken), and write
// atomically (temp + rename), because a file watcher is reading concurrently.

import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { recordTab } from "./remote-tab.mjs";
import os from "node:os";
import { pathToFileURL } from "node:url";

function readStdin() {
	try {
		return fs.readFileSync(0, "utf8");
	} catch {
		return "";
	}
}

/**
 * Map a Codex hook payload to one of running | idle.
 *
 * Only `Stop` means the turn ended. Every other event we install for
 * (`UserPromptSubmit`) — or any future one we happen to receive — means codex
 * is alive and working. We deliberately never return "waiting": codex runs
 * here with `--ask-for-approval never`, so it does not block on the user, and
 * inventing a "needs input" state would be exactly the R11 lie the codex
 * adapter's BestEffort fidelity exists to prevent.
 *
 * Exported for the test harness.
 */
export function classify(payload) {
	const event = String(
		payload?.hook_event_name ?? payload?.hookEventName ?? payload?.type ?? "",
	).toLowerCase();
	if (event.includes("stop")) return "idle";
	return "running";
}

export function writeStatus(cwd, state, sessionFile = null, sessionId) {
	try {
		const dir = path.join(cwd, ".harness");
		fs.mkdirSync(dir, { recursive: true });
		const payload =
			JSON.stringify({ state, agent: "codex", timestamp: new Date().toISOString(), sessionFile }, null, "\t") +
			"\n";
		const tmp = path.join(dir, `.status-${process.pid}-${crypto.randomBytes(4).toString("hex")}.tmp`);
		fs.writeFileSync(tmp, payload, "utf8");
		fs.renameSync(tmp, path.join(dir, "status.json")); // atomic on POSIX
		recordTab(payload, sessionId);
	} catch {
		// Never let status bookkeeping break the agent.
	}
}

if (process.argv[1] && fs.existsSync(process.argv[1]) &&
	import.meta.url === pathToFileURL(fs.realpathSync(process.argv[1])).href) {
	// The hook lives in the user's GLOBAL codex config (~/.codex/hooks.json), so
	// this shim runs for every codex everywhere. Only harness-spawned codex
	// processes carry this marker (set on the pty env and inherited); without
	// it, exit before touching the filesystem — a codex run in a random repo
	// must not grow a .harness directory.
	if (process.env.HARNESS_MANAGED !== "1") process.exit(0);
	// Custom agents (ConfigurableRuntime) set HARNESS_AGENT and install their
	// own gated harness-status.mjs entry in the same global hooks.json. Skip
	// so we do not overwrite their agent slug with "codex".
	if (process.env.HARNESS_AGENT) process.exit(0);
	let payload = {};
	try {
		// Codex hooks deliver the event as JSON on stdin (Claude-Code style); the
		// argv fallback keeps this working if a future codex passes it positionally.
		payload = JSON.parse(readStdin() || process.argv[2] || "{}");
	} catch {
		/* unparseable — fall through to "running" */
	}
	// Attribution: prefer the payload's own cwd, then codex's project-dir env,
	// then this process's cwd — the worktree the harness launched codex in.
	const cwd = payload?.cwd || process.env.CODEX_PROJECT_DIR || process.cwd();
	let file = payload?.transcript_path;
	const id = payload?.session_id || payload?.thread_id || process.env.CODEX_THREAD_ID;
	if (!file && typeof id === "string" && /^[a-f\d]{8}-[a-f\d]{4}-7[a-f\d]{3}-[a-f\d]{4}-[a-f\d]{12}$/i.test(id)) {
		const date = new Date(Number.parseInt(id.replaceAll("-", "").slice(0, 12), 16));
		const base = process.env.CODEX_HOME || path.join(os.homedir(), ".codex");
		const dir = path.join(base, "sessions", String(date.getFullYear()),
			String(date.getMonth() + 1).padStart(2, "0"), String(date.getDate()).padStart(2, "0"));
		try {
			const name = fs.readdirSync(dir).find(n => n.startsWith("rollout-") &&
				(n.endsWith(id + ".jsonl") || n.endsWith(id + ".jsonl.zst")));
			if (name) file = path.join(dir, name);
		} catch { /* The first turn may precede rollout creation. */ }
	}
	writeStatus(cwd, classify(payload), typeof file === "string" && path.isAbsolute(file) ? file : null, id);
	process.exit(0);
}
