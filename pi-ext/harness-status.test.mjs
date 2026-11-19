// Mock-harness test for harness-status.ts: drives the extension with a faked
// ExtensionAPI/ExtensionContext and asserts every state transition and the
// atomic-write contract. Run with:  npm install && npm test   (in pi-ext/)
//
// The extension is loaded through jiti — the same loader real pi uses — so TS
// loading is part of what's under test. pi 0.73.1 switched from the
// @mariozechner/jiti fork to upstream jiti 2.7 (`jiti/static`), so this import
// tracks it; if it ever diverges again, extension loading breaks in production
// while a stale fork keeps the test green.

process.env.HARNESS_STATUS_SETTLE_MS = "50";

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createJiti } from "jiti/static";

const jiti = createJiti(import.meta.url, { interopDefault: true });
const factory = await jiti.import(
	new URL("./harness-status.ts", import.meta.url).pathname,
	{ default: true },
);

const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "harness-status-test-"));

const handlers = {};
const pi = { on: (ev, h) => { (handlers[ev] ??= []).push(h); } };
const ui = {
	confirm: async () => {
		await new Promise((r) => setTimeout(r, 80));
		return true;
	},
	// `custom` takes keyboard focus and resolves via done() — it blocks the
	// agent just like confirm, so the wrap must cover it too.
	custom: async () => {
		await new Promise((r) => setTimeout(r, 80));
		return "picked";
	},
};
const ctx = {
	cwd,
	ui,
	sessionManager: { getSessionFile: () => "/fake/session.jsonl" },
	hasPendingMessages: () => false,
};

factory(pi);

const emit = async (ev, e = {}) => {
	for (const h of handlers[ev] ?? []) await h(e, ctx);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const state = () =>
	JSON.parse(fs.readFileSync(path.join(cwd, ".harness/status.json"), "utf8"));
let failures = 0;
const assert = (cond, msg) => {
	if (!cond) {
		failures++;
		console.error("FAIL:", msg, JSON.stringify(state()));
	} else {
		console.log("ok:", msg);
	}
};

await emit("session_start", { reason: "startup" });
assert(
	state().state === "idle" && state().sessionFile === "/fake/session.jsonl",
	"session_start -> idle + sessionFile",
);
assert(
	/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}/.test(state().timestamp),
	"timestamp is ISO8601",
);

await emit("agent_start");
assert(state().state === "running", "agent_start -> running");

// A dialog opened by ANY extension through the shared ui object -> waiting.
const dialog = ctx.ui.confirm("t", "m");
await sleep(20);
assert(state().state === "waiting", "open dialog -> waiting");
await dialog;
assert(state().state === "running", "dialog resolved -> back to running");

// Same for ui.custom (focus-taking component dialog).
const customDialog = ctx.ui.custom(() => {});
await sleep(20);
assert(state().state === "waiting", "open custom dialog -> waiting");
assert((await customDialog) === "picked", "custom dialog return value passes through");
assert(state().state === "running", "custom dialog resolved -> back to running");

// Nested dialogs must not drop back to running until the *last* one closes.
const outer = ctx.ui.confirm("t", "m");
await sleep(10);
const inner = ctx.ui.custom(() => {});
await sleep(10);
assert(state().state === "waiting", "nested dialogs -> waiting");
await outer;
assert(state().state === "waiting", "still waiting while inner dialog is open");
await inner;
assert(state().state === "running", "all dialogs closed -> running");

// Auto-retry: agent_start within the settle window cancels the idle write.
await emit("agent_end", { messages: [] });
await emit("agent_start");
await sleep(120);
assert(state().state === "running", "agent_end->agent_start retry does not flap to idle");

// Real settle.
await emit("agent_end", { messages: [] });
await sleep(120);
assert(state().state === "idle", "agent_end + settle -> idle");

// Queued steer/follow-up messages keep the agent running.
await emit("agent_start");
ctx.hasPendingMessages = () => true;
await emit("agent_end", { messages: [] });
await sleep(120);
assert(state().state === "running", "agent_end with queued messages stays running");
ctx.hasPendingMessages = () => false;

await emit("session_shutdown", { reason: "quit" });
assert(state().state === "idle", "session_shutdown -> idle");

// Atomic-write contract: no temp files left behind.
const leftovers = fs
	.readdirSync(path.join(cwd, ".harness"))
	.filter((f) => f !== "status.json");
assert(leftovers.length === 0, "no temp files left behind");

// A pane created before HARNESS_AGENT_TAB_ID existed still names its tmux session.
delete process.env.HARNESS_AGENT_TAB_ID;
process.env.HARNESS_REMOTE_SESSION = "harness_example_7";
const remoteCwd = fs.mkdtempSync(path.join(os.tmpdir(), "harness-status-remote-"));
const remoteHandlers = {};
factory({ on: (ev, h) => { (remoteHandlers[ev] ??= []).push(h); } });
const remoteCtx = { ...ctx, cwd: remoteCwd };
for (const h of remoteHandlers.session_start ?? []) await h({ reason: "startup" }, remoteCtx);
const remoteStatus = JSON.parse(fs.readFileSync(path.join(remoteCwd, ".harness/status-tab-7.json"), "utf8"));
assert(remoteStatus.tabId === 7 && remoteStatus.agent === "pi", "remote session name supplies the tab id");

process.env.HARNESS_AGENT_TAB_ID = "9";
const explicitCwd = fs.mkdtempSync(path.join(os.tmpdir(), "harness-status-tab-"));
const explicitHandlers = {};
factory({ on: (ev, h) => { (explicitHandlers[ev] ??= []).push(h); } });
const explicitCtx = { ...ctx, cwd: explicitCwd };
for (const h of explicitHandlers.session_start ?? []) await h({ reason: "startup" }, explicitCtx);
const explicitStatus = JSON.parse(fs.readFileSync(path.join(explicitCwd, ".harness/status-tab-9.json"), "utf8"));
assert(explicitStatus.tabId === 9, "HARNESS_AGENT_TAB_ID wins over the session name");
assert(!fs.existsSync(path.join(explicitCwd, ".harness/status-tab-7.json")), "the session suffix is not used when the tab id is set");

fs.rmSync(cwd, { recursive: true, force: true });
fs.rmSync(remoteCwd, { recursive: true, force: true });
fs.rmSync(explicitCwd, { recursive: true, force: true });
delete process.env.HARNESS_AGENT_TAB_ID;
delete process.env.HARNESS_REMOTE_SESSION;
if (failures > 0) {
	console.error(`${failures} assertion(s) failed`);
	process.exit(1);
}
console.log("ALL MOCK TESTS PASSED");
