/**
 * harness-tools — pins pi's runtime toolset to the versioned contract in
 * `agent-schema/v1/tools.json`. See docs/architecture.md §4.4 and
 * crates/agent-schema.
 *
 * Why this exists: the model is trained against a specific tool schema. If the
 * runtime offers a *different* set — because pi added a built-in, or a user
 * extension registered one — the model sees tools it never trained on and the
 * training/serving contract silently breaks. That is R5's failure mode, and it
 * is invisible: nothing errors, the model just behaves worse.
 *
 * So on session start this:
 *   1. calls `setActiveTools` with exactly the contract's tool names, and
 *   2. reports any drift between the contract and what pi actually offers.
 *
 * It deliberately does NOT `registerTool` the built-ins — pi already implements
 * them, and reimplementing would fork behavior from the schema. The contract's
 * job is to say *which* tools are active and what their arguments look like;
 * v1 is pinned to pi's own built-ins, so agreement is the expected state and
 * drift is the signal.
 *
 * Harness-specific tools (worktree-aware ops, "request review") are a future
 * MINOR bump: they get `registerTool`ed here and added to the contract in the
 * same commit.
 *
 * Install alongside harness-status.ts (the harness passes both with `pi -e`).
 */

import type { ExtensionAPI, ExtensionContext } from "@mariozechner/pi-coding-agent";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

interface ToolSchema {
	name: string;
	description: string;
	parameters: Record<string, unknown>;
}

interface AgentSchema {
	schemaVersion: string;
	generatedFrom: { agent: string; version: string; dumpedAt: string };
	tools: ToolSchema[];
}

/**
 * Locate `agent-schema/v1/tools.json`.
 *
 * `HARNESS_AGENT_SCHEMA` wins when set — that is how the packaged app points at
 * the copy it materialized, since a bundled `.app` has no repo to walk up from.
 * Otherwise walk up from this file, which covers running straight out of a
 * checkout.
 */
function locateSchema(): string | undefined {
	const override = process.env.HARNESS_AGENT_SCHEMA;
	if (override && fs.existsSync(override)) return override;

	let dir = path.dirname(fileURLToPath(import.meta.url));
	for (let i = 0; i < 6; i++) {
		const candidate = path.join(dir, "agent-schema", "v1", "tools.json");
		if (fs.existsSync(candidate)) return candidate;
		const parent = path.dirname(dir);
		if (parent === dir) break;
		dir = parent;
	}
	return undefined;
}

function loadSchema(): AgentSchema | undefined {
	const file = locateSchema();
	if (!file) return undefined;
	try {
		return JSON.parse(fs.readFileSync(file, "utf8")) as AgentSchema;
	} catch {
		return undefined;
	}
}

export default function (pi: ExtensionAPI) {
	pi.on("session_start", async (_event, ctx: ExtensionContext) => {
		const schema = loadSchema();
		if (!schema) {
			// Never block the agent over schema bookkeeping — an un-pinned
			// toolset is worse than ideal, not fatal.
			try {
				ctx.ui?.setStatus?.("harness-tools", "schema: not found");
			} catch {
				/* no UI in this mode */
			}
			return;
		}

		const wanted = schema.tools.map((t) => t.name);
		try {
			pi.setActiveTools(wanted);
		} catch {
			/* older pi, or tools already fixed — reporting below still applies */
		}

		// Report drift rather than silently correcting it: a tool pi offers that
		// the contract lacks means the contract needs a version bump, and one
		// the contract has but pi lacks means the model will emit dead calls.
		let available: string[] = [];
		try {
			available = (pi.getTools?.() ?? []).map((t: { name: string }) => t.name);
		} catch {
			/* introspection unavailable — the setActiveTools call still stands */
		}

		const missing = wanted.filter((n) => available.length > 0 && !available.includes(n));
		const extra = available.filter((n) => !wanted.includes(n));

		const summary =
			missing.length === 0 && extra.length === 0
				? `schema ${schema.schemaVersion} (${wanted.length} tools)`
				: `schema ${schema.schemaVersion} DRIFT` +
					(missing.length ? ` missing:${missing.join(",")}` : "") +
					(extra.length ? ` extra:${extra.join(",")}` : "");

		try {
			ctx.ui?.setStatus?.("harness-tools", summary);
			if (missing.length || extra.length) {
				ctx.ui?.notify?.(`harness-tools: ${summary}`, "warning");
			}
		} catch {
			/* no UI in this mode */
		}
	});
}
