// OpenCode 2 status core -> the shared .harness/status.json (§4.5).
//
// OpenCode 2 runs one shared background service per Mac, on a random port,
// and every TUI is a client of it. The TUI is the only OpenCode process
// that is ours: it runs in the tab's terminal with the tab's env. So status
// is reported from a TUI plugin (`opencode-status-tui.js`, installed as
// `shims/opencode-status/tui.js`) that loads this module.
//
// Kept free of `@opencode/plugin/tui` imports so node can load it in tests.
//
// The state comes from the TUI's own session store (`context.data`), which
// OpenCode keeps current from the service's event stream:
//
//   pending permission (`permission.asked`) or question (`form.created`)  -> waiting
//   `session.status(id) === "running"` (`session.execution.started`)      -> running
//   otherwise (`session.execution.succeeded|failed|interrupted`,
//              `permission.replied`, `form.replied|cancelled` cleared it) -> idle
//
// Reading the store rather than replaying events survives two permissions
// pending at once, a missed event, and a TUI that opens a session already
// mid-turn.
//
// Own sessions only: the service streams every session on the Mac. A tab
// reports the sessions its own TUI has shown since launch, plus their
// subagent sessions. The set does not shrink when the TUI goes home or to a
// plugin view, because a turn keeps running there.
//
// Never fail loudly. Status is bookkeeping and must not break the TUI.

const RUNNING = new Set(["running", "busy", "retry"]);

// Events after which the store may hold a different state. Anything else
// (streaming deltas, mostly) cannot change it, so it is not worth a read.
export const STATE_EVENTS = new Set([
	"server.connected",
	"session.execution.started",
	"session.execution.succeeded",
	"session.execution.failed",
	"session.execution.interrupted",
	"permission.asked",
	"permission.replied",
	"form.created",
	"form.replied",
	"form.cancelled",
	// OpenCode 1.x names, in case a 2.x build still emits them.
	"permission.updated",
	"session.idle",
	"session.error",
	"session.status",
]);

function attempt(read, fallback) {
	try {
		return read() ?? fallback;
	} catch {
		return fallback;
	}
}

function count(read) {
	const items = attempt(read, []);
	return Array.isArray(items) ? items.length : 0;
}

/**
 * The sessions a tab owns: each shown session, its root, and the root's
 * subagent family (so a subagent's permission prompt counts as ours).
 *
 * Exported for the test harness.
 */
export function ownSessions(data, shown) {
	const own = new Set();
	for (const id of shown) {
		own.add(id);
		const root = attempt(() => data.session.root(id), id);
		own.add(root);
		for (const member of attempt(() => data.session.family(root), [])) own.add(member);
	}
	return own;
}

/**
 * Map the store's view of `ids` to running | waiting | idle.
 *
 * Exported for the test harness.
 */
export function sessionState(data, ids) {
	let running = false;
	for (const id of ids) {
		if (count(() => data.session.permission.list(id)) > 0) return "waiting";
		if (count(() => data.session.form.list(id)) > 0) return "waiting";
		const status = attempt(() => data.session.status(id), "idle");
		if (RUNNING.has(typeof status === "string" ? status : status?.type)) running = true;
	}
	return running ? "running" : "idle";
}

/**
 * Write `state` through remote-tab.mjs's `writeStatusFiles`, the helper
 * every agent's hook shares: `.harness/status.json` plus
 * `status-tab-<HARNESS_AGENT_TAB_ID>.json`, stamped agent "opencode".
 * `load` resolves to that module; writes wait for it and keep their order.
 *
 * Exported for the test harness.
 */
export function statusWriter(load, cwd) {
	let helper;
	const ready = Promise.resolve()
		.then(load)
		.then((module) => (helper = module), () => null);
	return (state) => {
		const status = {
			state,
			agent: "opencode",
			timestamp: new Date().toISOString(),
			sessionFile: null,
			tool: null,
		};
		const write = (module) => {
			try {
				module?.writeStatusFiles(cwd, status);
			} catch {
				// Never let status bookkeeping break the agent.
			}
		};
		if (helper) write(helper);
		else ready.then(write);
	};
}

/**
 * Report this TUI's state whenever it changes. Returns the plugin cleanup,
 * or undefined outside an openOrchestra tab.
 *
 * `context` is the TUI plugin context: `context.data` (`listen`, `session.*`)
 * and `context.ui.router.current()`.
 */
export function startStatusReporter(context, { write, env = process.env, interval = 1000 } = {}) {
	if (!env.HARNESS_AGENT_TAB_ID && !env.HARNESS_AGENT_WORKTREE && !env.HARNESS_REMOTE_SESSION) return undefined;
	const shown = new Set();
	let last;

	function update() {
		try {
			const route = context.ui.router.current();
			if (route?.type === "session" && typeof route.sessionID === "string") shown.add(route.sessionID);
			const state = sessionState(context.data, ownSessions(context.data, shown));
			if (state === last) return;
			last = state;
			write(state);
		} catch {
			// A status read must never interrupt the terminal.
		}
	}

	// `listen` sees every event after the store has applied it (the store's
	// own listener is registered first). The timer catches route changes.
	const stop = attempt(() => context.data.listen((event) => {
		if (STATE_EVENTS.has(event?.name)) update();
	}), undefined);
	const timer = setInterval(update, interval);
	update();
	return () => {
		if (typeof stop === "function") stop();
		clearInterval(timer);
		// Quitting mid-turn must not leave the tab spinning.
		if (last !== undefined && last !== "idle") write("idle");
	};
}
