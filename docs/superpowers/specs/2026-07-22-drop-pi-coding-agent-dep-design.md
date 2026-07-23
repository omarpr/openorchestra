# Drop the `@mariozechner/pi-coding-agent` dependency — design

**Date:** 2026-07-22 · **Status:** approved, implementing

## Problem

`pi-ext/package.json` carries `@mariozechner/pi-coding-agent@0.73.1` as a
**devDependency**. The package is deprecated (npm points at
`@earendil-works/pi-coding-agent`) and pulls a transitive dev tree with several
high-severity advisories into this repo. Yet the two extensions that import it
(`pi-ext/harness-status.ts`, `pi-ext/harness-tools.ts`) use it **only** for
`import type { ExtensionAPI, ExtensionContext }` — a compile/editor-time type
reference, nothing at runtime. The tests load the extensions through `jiti`,
which strips TS types on import, so the package is never actually executed.

This is also the one place the harness violates its own agent-agnostic rule.
Architecture §4.5 states the harness "bundles, redistributes, and links **none**
of them … separate subprocesses resolved from `$PATH`." Every other agent
(Codex, Claude Code, Kimi, OpenCode) is a `$PATH` CLI we pull no npm package
for. pi should be the same.

## Goal / non-goals

**Goal.** Remove the vulnerable package from this repo's dependency tree while
keeping both pi extensions working, and reframe the docs so pi's package is
treated like its peers — a `$PATH` CLI, not a build-time dependency.

**Non-goals.** This does **not** demote pi as the reference runtime. The
training story (§4.4 tool-schema contract, §6.4 session-exhaust harvesting) and
the `agent-schema/` + `training/` crates are unchanged. pi remains the agent we
train end-to-end; we simply stop importing its npm package to get two type
names.

## Approach

Vendor the tiny type surface the extensions use into a local declaration file
and import it by relative path.

Rejected alternative: an ambient `declare module "@mariozechner/pi-coding-agent"`
that keeps the import string unchanged. It preserves a dangling reference to a
package we no longer depend on — exactly the coupling we are removing.

## Type surface actually used

`ExtensionAPI`: `on(event, handler)`, `setActiveTools(names)`, `getTools?()`.

`ExtensionContext`: `cwd`, `ui?` (`setStatus?`, `notify?`, plus the dialog
methods `select`/`confirm`/`input`/`editor`/`custom` that `harness-status.ts`
wraps dynamically), `hasPendingMessages()`, `sessionManager?.getSessionFile?()`.

Handler shape: `(event, ctx: ExtensionContext) => void | Promise<void>`.

## Changes

1. **New `pi-ext/pi-extension-api.d.ts`** — hand-authored declarations for
   `ExtensionAPI` and `ExtensionContext` covering exactly the members above.
   Header notes the types are vendored from pi 0.73.1's public extension API and
   points at the `@earendil-works` rename to re-verify on. `ui` is typed with
   the known fire-and-forget methods plus an index signature, since
   `harness-status.ts` reaches the dialog methods through a dynamic cast.

2. **`pi-ext/harness-status.ts` and `pi-ext/harness-tools.ts`** — change the
   import from `@mariozechner/pi-coding-agent` to `./pi-extension-api`. Keep the
   "Verified against pi 0.73.1" comments; they now mark where the vendored types
   came from.

3. **`pi-ext/package.json`** — drop `@mariozechner/pi-coding-agent` from
   `devDependencies` (keep `jiti`). Regenerate `package-lock.json` via
   `npm install`.

4. **Docs reframe (light):**
   - `docs/architecture.md` §4 (package-identity note), §4.4 (registration
     surface), and the R3 risk row (§8): the extension-API types are now
     vendored in-repo; the pin sites drop from four to three — `setup`,
     `doctor`, extension header — no longer `pi-ext/package.json`; per §4.5 the
     harness pulls no agent's npm package into its tree, pi included.
   - `docs/m2-pi-pin.md`: the §6 rename watch-item and the Verdict list
     `pi-ext/package.json` + the `import type` as scope-rename sites — note that
     both were removed (types vendored), leaving `setup` + `doctor`. The dated
     diff tables stay as historical provenance.

**Explicitly unchanged:** `script/setup-macos.sh` and `script/doctor.sh` (they
pin the *runtime* pi CLI users install with `npm i -g` — legitimate and still
required); `crates/agent-schema/tests/contract.rs` and `agent-schema/v1/tools.json`'s
`generatedFrom.agent` (schema provenance, not a dependency); all of
`agent-schema/` and `training/`.

## Verification

- `cd pi-ext && npm install` — regenerates the lockfile; `npm ls` shows no
  `pi-coding-agent` in the tree.
- `npm test` (both `harness-*.test.mjs`) passes — extensions still load via jiti.
- `npm audit` before/after — the advisories from the package's dev tree are gone.
- `cargo test -p agent-schema` still green (unaffected).
- Repo grep for `pi-coding-agent` returns only intentional references: runtime
  install scripts, schema provenance, and vendored-types provenance comments.

## Notes / edge cases

- There is **no `tsconfig`/compile step** in `pi-ext`; the `.d.ts` is
  editor/typecheck-time only. jiti strips `import type` at runtime, so the
  extensions execute identically regardless. The declaration file still earns
  its place: real editor/IDE type-checking and a self-documenting contract.
