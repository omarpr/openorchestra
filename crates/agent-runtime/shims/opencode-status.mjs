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
//   pending permission (`permission.asked`) or question (`form.created`)
//   while a turn runs                                                     -> waiting
//   `session.status(id) === "running"` (`session.execution.started`)      -> running
//   otherwise (`session.execution.succeeded|failed|interrupted`,
//              `permission.replied`, `form.replied|cancelled` cleared it) -> idle
//
// Reading the store rather than replaying events survives two permissions
// pending at once, a missed event, and a TUI that opens a session already
// mid-turn. One store gap is patched here: a request still listed when its
// session stops running is left over from an interrupt (`requestTracker`)
// and never counts again.
//
// Own sessions only: the service streams every session on the Mac. A tab
// reports the session its TUI shows, plus any it showed earlier that is
// still running (a turn keeps going while the TUI is home, in a plugin view
// or on another in-TUI tab), plus their subagent sessions. A session leaves
// the set once it is neither on screen nor running, so a session another
// tab later runs is not reported here.
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

function items(read) {
	const list = attempt(read, []);
	return Array.isArray(list) ? list : [];
}

function isRunning(data, id) {
	const status = attempt(() => data.session.status(id), "idle");
	return RUNNING.has(typeof status === "string" ? status : status?.type);
}

function requests(data, id) {
	return [...items(() => data.session.permission.list(id)), ...items(() => data.session.form.list(id))];
}

/**
 * Remembers which pending requests outlived their turn.
 *
 * OpenCode 2.3.2's server drops a pending permission or question when a
 * turn is interrupted without publishing `permission.replied` or
 * `form.cancelled`, and the TUI store only removes one on those events. The
 * leftover would read as waiting through the session's next turn. So when
 * a session stops running, every request it still lists is stale and never
 * counts again; requests asked during a later turn do.
 *
 * Exported for the test harness.
 */
export function requestTracker() {
	const running = new Map();
	const stale = new Map();
	return {
		/** The session's live (not stale) request count, updating the books. */
		live(data, id) {
			const now = isRunning(data, id);
			const listed = requests(data, id);
			const ids = new Set(listed.map((request) => request?.id).filter((key) => key !== undefined));
			let old = stale.get(id) ?? new Set();
			// Forget stale ids the store has dropped since.
			old = new Set([...old].filter((key) => ids.has(key)));
			if (running.get(id) && !now) for (const key of ids) old.add(key);
			running.set(id, now);
			if (old.size) stale.set(id, old);
			else stale.delete(id);
			return listed.filter((request) => !old.has(request?.id)).length;
		},
	};
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
 * Map the store's view of `ids` to running | waiting | idle. `tracker`
 * (from `requestTracker`) discounts requests left over from an interrupted
 * turn; without it every listed request counts.
 *
 * Exported for the test harness.
 */
export function sessionState(data, ids, tracker) {
	let running = false;
	let pending = false;
	for (const id of ids) {
		const live = tracker ? tracker.live(data, id) : requests(data, id).length;
		if (live > 0) pending = true;
		if (isRunning(data, id)) running = true;
	}
	// A prompt only blocks a turn in progress; a false waiting notifies.
	if (!running) return "idle";
	return pending ? "waiting" : "running";
}

/**
 * Write `state` through remote-tab.mjs's `writeStatusFiles`, the helper
 * every agent's hook shares: `.harness/status.json` plus
 * `status-tab-<HARNESS_AGENT_TAB_ID>.json`, stamped agent "opencode".
 * `load` resolves to that module; writes wait for it and keep their order.
 * Each write returns a promise of whether it landed (false: the helper
 * didn't load, or the write threw), so the reporter can try again.
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
			if (!module) return false;
			try {
				module.writeStatusFiles(cwd, status);
				return true;
			} catch {
				// Never let status bookkeeping break the agent.
				return false;
			}
		};
		return helper ? Promise.resolve(write(helper)) : ready.then(write);
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
	const tracker = requestTracker();
	// The state last handed to `write`, and whether that write failed (a
	// full disk): the next update writes it again rather than skipping an
	// unchanged state that never reached the file.
	let last;
	let failed = false;

	function publish(state) {
		last = state;
		failed = false;
		let written;
		try {
			written = write(state);
		} catch {
			written = false;
		}
		const lost = () => {
			if (last === state) failed = true;
		};
		Promise.resolve(written).then((ok) => {
			if (ok === false) lost();
		}, lost);
	}

	function update() {
		try {
			const route = context.ui.router.current();
			const current = route?.type === "session" && typeof route.sessionID === "string" ? route.sessionID : undefined;
			if (current) shown.add(current);
			const state = sessionState(context.data, ownSessions(context.data, shown), tracker);
			// After the read, so a turn's end is still reported before its
			// session leaves the set.
			for (const id of [...shown]) {
				if (id === current) continue;
				const tree = ownSessions(context.data, [id]);
				if (![...tree].some((member) => isRunning(context.data, member))) shown.delete(id);
			}
			if (state === last && !failed) return;
			publish(state);
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
		if (last !== undefined && (last !== "idle" || failed)) write("idle");
	};
}
