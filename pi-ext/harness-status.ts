/**
 * harness-status — pi extension that mirrors agent lifecycle state into
 * `.harness/status.json` in the project cwd, for the harness's Rust status
 * watcher (crates/status-watch) to pick up. See docs/architecture.md §4.1.
 *
 * Verified against @mariozechner/pi-coding-agent v0.73.1 (the `pi` CLI;
 * note: the npm package `@mariozechner/pi` is the unrelated "pods" tool).
 * The pin moved 0.70.6 -> 0.73.1 in M2; see docs/m2-pi-pin.md for the API
 * diff that made the move safe and for the upcoming @earendil-works scope
 * rename to watch for.
 *
 * State mapping (architecture sketch -> real pi extension events):
 *   "agent_start"            -> running   (real event, 1:1)
 *   "UI dialog open"         -> waiting   (NO event exists; we wrap the shared
 *                                          ctx.ui dialog methods select/confirm/
 *                                          input/editor — covers approval-gate
 *                                          extensions, which is the harness case)
 *   "agent_settled"          -> idle      (RPC-only event; extension equivalent is
 *                                          "agent_end" + ctx.hasPendingMessages()
 *                                          to avoid flapping through queued
 *                                          follow-ups/steering)
 *
 * Verified against real pi: on provider errors, auto-retry emits
 * agent_end -> agent_start again (observed 4 attempts per prompt). The idle
 * write is therefore debounced (SETTLE_MS) and cancelled by a following
 * agent_start, so the pill doesn't flap running->idle->running mid-retry.
 *
 * Writes are atomic: temp file in the same directory + rename.
 *
 * Install: copy/symlink into `.pi/extensions/` (project) or
 * `~/.pi/agent/extensions/` (global), or run `pi -e pi-ext/harness-status.ts`,
 * or list it under "extensions" in settings.json.
 */

import type { ExtensionAPI, ExtensionContext } from "@mariozechner/pi-coding-agent";
import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as path from "node:path";

type HarnessState = "running" | "waiting" | "idle";

interface HarnessStatus {
	state: HarnessState;
	timestamp: string; // ISO-8601
	sessionFile: string | null;
}

export default function (pi: ExtensionAPI) {
	// ---- state machine -------------------------------------------------------
	let agentActive = false;
	let dialogDepth = 0; // >0 while any extension UI dialog is open
	let lastWritten: string | undefined;

	const currentState = (): HarnessState =>
		dialogDepth > 0 ? "waiting" : agentActive ? "running" : "idle";

	// ---- atomic writer -------------------------------------------------------
	function writeStatus(ctx: ExtensionContext, override?: HarnessState) {
		let cwd: string;
		let sessionFile: string | null;
		try {
			cwd = ctx.cwd;
			sessionFile = ctx.sessionManager?.getSessionFile?.() ?? null;
		} catch {
			// Runner already torn down (e.g. late shutdown callback) — nothing to do.
			return;
		}
		const status: HarnessStatus = {
			state: override ?? currentState(),
			timestamp: new Date().toISOString(),
			sessionFile,
		};
		try {
			const dir = path.join(cwd, ".harness");
			fs.mkdirSync(dir, { recursive: true });
			const payload = JSON.stringify(status, null, "\t") + "\n";
			// Skip redundant writes (ignoring the timestamp) so the file watcher
			// isn't spammed, but always write on state change.
			const key = `${status.state}|${status.sessionFile ?? ""}`;
			if (key === lastWritten) return;
			const tmp = path.join(dir, `.status-${process.pid}-${crypto.randomBytes(4).toString("hex")}.tmp`);
			fs.writeFileSync(tmp, payload, "utf8");
			fs.renameSync(tmp, path.join(dir, "status.json")); // atomic on POSIX
			lastWritten = key;
		} catch {
			// Never let status bookkeeping break the agent.
		}
	}

	// ---- "waiting" detection -------------------------------------------------
	// The extension API has no "dialog opened" event. All extensions of a session
	// share one ExtensionUIContext instance (ExtensionRunner.uiContext), so we
	// wrap its dialog methods once; any extension (e.g. an approval gate) opening
	// select/confirm/input/editor flips the state to "waiting" until it resolves.
	// `custom` is included because it "shows a custom component with keyboard
	// focus" and resolves only when the component calls done() — i.e. it blocks
	// the agent exactly like the other four. It was missed before M2.
	const patched = new WeakSet<object>();
	const DIALOG_METHODS = ["select", "confirm", "input", "editor", "custom"] as const;

	function patchUi(ctx: ExtensionContext) {
		let ui: Record<string, unknown>;
		try {
			ui = ctx.ui as unknown as Record<string, unknown>;
		} catch {
			return;
		}
		if (!ui || typeof ui !== "object" || patched.has(ui)) return;
		patched.add(ui);
		for (const name of DIALOG_METHODS) {
			const original = ui[name];
			if (typeof original !== "function") continue;
			ui[name] = async (...args: unknown[]) => {
				dialogDepth++;
				writeStatus(ctx);
				try {
					return await (original as (...a: unknown[]) => Promise<unknown>).apply(ui, args);
				} finally {
					dialogDepth = Math.max(0, dialogDepth - 1);
					writeStatus(ctx);
				}
			};
		}
	}

	// ---- idle debounce -------------------------------------------------------
	// Auto-retry emits agent_end -> agent_start again; delay the idle write a
	// little and cancel it if another agent_start arrives. unref() so the timer
	// never keeps the pi process alive.
	const SETTLE_MS = Number(process.env.HARNESS_STATUS_SETTLE_MS ?? 750);
	let settleTimer: ReturnType<typeof setTimeout> | undefined;

	function cancelSettle() {
		if (settleTimer !== undefined) {
			clearTimeout(settleTimer);
			settleTimer = undefined;
		}
	}

	// ---- lifecycle events ----------------------------------------------------
	pi.on("session_start", async (_event, ctx) => {
		// The bound uiContext can be swapped after extension load; (re-)patch here
		// and again on agent_start (idempotent via WeakSet).
		patchUi(ctx);
		cancelSettle();
		agentActive = false;
		dialogDepth = 0;
		lastWritten = undefined; // new session file -> force a write
		writeStatus(ctx);
	});

	pi.on("agent_start", async (_event, ctx) => {
		patchUi(ctx);
		cancelSettle();
		agentActive = true;
		writeStatus(ctx);
	});

	pi.on("agent_end", async (_event, ctx) => {
		// "agent_settled" is RPC-only; approximate it: if steering/follow-up
		// messages are queued, another agent_start is imminent — stay "running".
		let settled = true;
		try {
			settled = !ctx.hasPendingMessages();
		} catch {
			/* keep settled = true */
		}
		if (!settled) return;
		cancelSettle();
		settleTimer = setTimeout(() => {
			settleTimer = undefined;
			agentActive = false;
			writeStatus(ctx);
		}, SETTLE_MS);
		settleTimer.unref?.();
	});

	pi.on("session_shutdown", async (_event, ctx) => {
		cancelSettle();
		agentActive = false;
		dialogDepth = 0;
		writeStatus(ctx, "idle");
	});
}
