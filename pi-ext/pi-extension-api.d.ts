/**
 * pi-extension-api — vendored type surface for the pi extension API.
 *
 * These declarations replace a build-time dependency on the npm package
 * `@mariozechner/pi-coding-agent`, which the harness extensions imported
 * *only* for `import type { ExtensionAPI, ExtensionContext }`. Pulling that
 * (deprecated, CVE-carrying) package into this repo's dev tree just to name two
 * types also broke the architecture's own rule (§4.5): the harness links no
 * agent's package — every agent, pi included, is a `$PATH` CLI. So the types
 * are vendored here and pi conforms to the same rule as its peers.
 *
 * This is deliberately a *minimal* mirror: it declares exactly the members the
 * two extensions touch, not pi's full API. Verified against the public
 * extension API of `@mariozechner/pi-coding-agent@0.73.1` (see the header of
 * harness-status.ts and docs/m2-pi-pin.md). **Re-verify on the upcoming
 * `@mariozechner/…` → `@earendil-works/pi-coding-agent` scope rename** — that
 * is the surface most likely to shift.
 *
 * There is no compile step in pi-ext; these types are editor/typecheck-time
 * only. pi loads the extensions through jiti, which strips `import type` at
 * runtime, so the extensions execute identically with or without this file.
 */

/** A tool as reported by `pi.getTools()`. */
export interface ExtensionTool {
	name: string;
}

/**
 * The shared per-session UI context (`ctx.ui`).
 *
 * `setStatus` / `notify` are fire-and-forget. The dialog methods
 * (`select`/`confirm`/`input`/`editor`/`custom`) block the agent until they
 * resolve; harness-status.ts wraps them through a dynamic cast to raise the
 * "waiting" state, reaching them via the index signature rather than by name.
 */
export interface ExtensionUI {
	setStatus?(key: string, value: string): void;
	notify?(message: string, level?: "info" | "warning" | "error"): void;
	[method: string]: unknown;
}

export interface ExtensionSessionManager {
	/** Path to the active session's JSONL file, or null before one exists. */
	getSessionFile?(): string | null;
}

/** Second argument to every event handler. */
export interface ExtensionContext {
	cwd: string;
	ui?: ExtensionUI;
	/** True while steering/follow-up messages are queued (another turn imminent). */
	hasPendingMessages(): boolean;
	sessionManager?: ExtensionSessionManager;
}

export type ExtensionEventHandler = (
	event: unknown,
	ctx: ExtensionContext,
) => void | Promise<void>;

/** The object handed to an extension's default-export factory. */
export interface ExtensionAPI {
	on(event: string, handler: ExtensionEventHandler): void;
	/** Pin the active toolset to exactly these names (the agent-schema contract). */
	setActiveTools(names: string[]): void;
	/** Introspect the currently-registered tools (older pi may omit this). */
	getTools?(): ExtensionTool[];
}
