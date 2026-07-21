// Mock-harness test for harness-tools.ts: drives the extension with a faked
// ExtensionAPI/ExtensionContext and asserts it pins the toolset to the
// contract and reports drift. Run with:  npm test   (in pi-ext/)

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createJiti } from "jiti/static";

const jiti = createJiti(import.meta.url, { interopDefault: true });
const extUrl = new URL("./harness-tools.ts", import.meta.url).pathname;

const contract = JSON.parse(
	fs.readFileSync(new URL("../agent-schema/v1/tools.json", import.meta.url).pathname, "utf8"),
);
const contractNames = contract.tools.map((t) => t.name);

let failures = 0;
const assert = (cond, msg, extra = "") => {
	if (!cond) {
		failures++;
		console.error("FAIL:", msg, extra);
	} else {
		console.log("ok:", msg);
	}
};

/** Run the extension against a fake pi exposing `available` tools. */
async function run(available) {
	// Re-import per case so module-level state cannot leak between runs.
	const factory = await jiti.import(extUrl, { default: true });
	const handlers = {};
	const statuses = {};
	const notifications = [];
	let activeTools = null;

	const pi = {
		on: (ev, h) => ((handlers[ev] ??= []).push(h)),
		setActiveTools: (names) => (activeTools = names),
		getTools: () => (available === null ? [] : available.map((name) => ({ name }))),
	};
	const ctx = {
		cwd: process.cwd(),
		ui: {
			setStatus: (k, v) => (statuses[k] = v),
			notify: (m, level) => notifications.push({ m, level }),
		},
	};
	factory(pi);
	for (const h of handlers.session_start ?? []) await h({ reason: "startup" }, ctx);
	return { activeTools, statuses, notifications };
}

// --- the contract is found and applied --------------------------------------

const agreed = await run(contractNames);
assert(
	JSON.stringify(agreed.activeTools) === JSON.stringify(contractNames),
	"setActiveTools is called with exactly the contract's tools",
	JSON.stringify(agreed.activeTools),
);
assert(
	agreed.statuses["harness-tools"] === `schema ${contract.schemaVersion} (${contractNames.length} tools)`,
	"status line reports the schema version when runtime and contract agree",
	agreed.statuses["harness-tools"],
);
assert(agreed.notifications.length === 0, "agreement raises no warning");

// --- drift is reported, not silently swallowed ------------------------------

const withExtra = await run([...contractNames, "some_user_extension_tool"]);
assert(
	/DRIFT/.test(withExtra.statuses["harness-tools"]),
	"a tool the runtime has but the contract lacks is reported as drift",
	withExtra.statuses["harness-tools"],
);
assert(
	/extra:some_user_extension_tool/.test(withExtra.statuses["harness-tools"]),
	"the extra tool is named",
	withExtra.statuses["harness-tools"],
);
assert(withExtra.notifications.length === 1, "drift raises exactly one warning");
assert(withExtra.notifications[0].level === "warning", "drift warning uses the warning level");

const withMissing = await run(contractNames.filter((n) => n !== "bash"));
assert(
	/missing:bash/.test(withMissing.statuses["harness-tools"]),
	"a contract tool the runtime lacks is reported as missing",
	withMissing.statuses["harness-tools"],
);

// --- degradation ------------------------------------------------------------

const noIntrospection = await run(null);
assert(
	JSON.stringify(noIntrospection.activeTools) === JSON.stringify(contractNames),
	"tools are still pinned when the runtime cannot be introspected",
);
assert(
	noIntrospection.notifications.length === 0,
	"no false drift warning when introspection is unavailable",
);

// --- missing schema must not break the agent --------------------------------

const savedCwd = process.cwd();
const isolated = fs.mkdtempSync(path.join(os.tmpdir(), "harness-tools-noschema-"));
process.env.HARNESS_AGENT_SCHEMA = path.join(isolated, "does-not-exist.json");
process.chdir(isolated);
try {
	// The walk-up still finds the repo copy from the extension's own location,
	// so assert the explicit-override path degrades rather than throwing.
	const missingSchema = await run(contractNames);
	assert(
		missingSchema.statuses["harness-tools"] !== undefined,
		"a status is still reported when the override path is absent",
	);
} finally {
	process.chdir(savedCwd);
	delete process.env.HARNESS_AGENT_SCHEMA;
	fs.rmSync(isolated, { recursive: true, force: true });
}

if (failures > 0) {
	console.error(`${failures} assertion(s) failed`);
	process.exit(1);
}
console.log("ALL HARNESS-TOOLS TESTS PASSED");
