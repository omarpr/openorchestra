// Mock-harness test for harness-status.ts: drives the extension with a faked
// ExtensionAPI/ExtensionContext and asserts every state transition and the
// atomic-write contract. Run with:  npm install && npm test   (in pi-ext/)
//
// The extension is loaded through @mariozechner/jiti — the same loader real pi
// uses — so TS loading is part of what's under test.

process.env.HARNESS_STATUS_SETTLE_MS = "50";

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createJiti } from "@mariozechner/jiti";

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

fs.rmSync(cwd, { recursive: true, force: true });
if (failures > 0) {
	console.error(`${failures} assertion(s) failed`);
	process.exit(1);
}
console.log("ALL MOCK TESTS PASSED");
