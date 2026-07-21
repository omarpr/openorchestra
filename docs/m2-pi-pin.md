# M2 — pi version pin reconciled to 0.73.1 (retires the R3 pin gap, re-verifies R9)

**Status:** Done · July 21, 2026 · closes roadmap item **M2** (architecture §12).
**Decision: the supported pin is `@mariozechner/pi-coding-agent@0.73.1`.**

The repo previously carried two pins: `script/setup-macos.sh` + `pi-ext/package.json`
said **0.70.6** (what the extension was written and tested against), while Spike 1's
container actually ran **0.73.1**. That split is the R3 "version-pin gap." It is now
one number, and 0.73.1 is also what npm currently serves as `latest`, so the pin no
longer drifts away from a fresh `npm i -g` the day someone runs `make setup`.

---

## 1. Why moving forward was safe

Diffed the two installed packages rather than trusting the changelog. For everything
`harness-status.ts` touches, **0.70.6 → 0.73.1 is purely additive**:

| Surface the extension depends on | 0.70.6 | 0.73.1 |
|---|---|---|
| Events `session_start` / `agent_start` / `agent_end` / `session_shutdown` | present | unchanged |
| `ctx.cwd`, `ctx.ui`, `ctx.sessionManager.getSessionFile()`, `ctx.hasPendingMessages()` | present | unchanged |
| `ctx.ui` resolution | getter returning `runner.uiContext` | **identical** (`runner.js` ~line 383) |
| Dialog methods `select` / `confirm` / `input` / `editor` | present | unchanged |

`dist/core/extensions/types.d.ts` diffs in 39 lines, all **additions**:
`thinking_level_select` event, `EditorFactory` alias + `getEditorComponent()`,
a `MessageEndEventResult` return type for `message_end`, and provider-config
fields (`name`, `baseUrl`, `thinkingLevelMap`). None are removals; none intersect
the extension.

`runner.js` diffs in 42 lines: the new `emitMessageEnd` handler-chaining path and
`getEditorComponent` on the no-op UI context. Again, no removals.

The 0.71–0.73 **breaking changes** in the changelog — `compat.reasoningEffortMap`
→ `thinkingLevelMap`, removal of the built-in Gemini CLI / Antigravity providers,
the Xiaomi provider switching to API billing — are all *provider configuration*
concerns. They do not touch the extension API, and none of them are providers we
use (§5 configures a local OpenAI-compatible endpoint).

## 2. R9 re-verified: the shared-`uiContext` wrap still holds

R9 is the risk that the "waiting" pill depends on an undocumented implementation
detail: that all extensions of a session share **one** `ExtensionUIContext`, so
patching its dialog methods once observes dialogs opened by *any* extension.

Confirmed still true at 0.73.1: `ExtensionRunner` holds a single `uiContext` field
(set via `setUIContext`, defaulting to `noOpUIContext`), and `createContext()`
exposes it through a getter that resolves at call time. The extension's idempotent
`WeakSet` patching therefore behaves exactly as before.

**Gap found and closed while re-verifying.** The wrap covered `select`, `confirm`,
`input`, and `editor` — but `ExtensionUIContext` also exposes **`custom`**, documented
as "show a custom component with keyboard focus," which returns a promise resolved by
the component calling `done()`. That blocks the agent exactly like the other four, so
a `custom` dialog would have left the pill showing **Running** while the agent was in
fact waiting on the user. `custom` is now in `DIALOG_METHODS`, with mock coverage for
it and for nested dialogs (the depth counter must not drop back to `running` until the
*last* dialog closes).

## 3. Extension loading changed — the test harness had to follow

0.73.1 "changed extension loading to use upstream `jiti` 2.7 instead of the
`@mariozechner/jiti` fork." Real pi now does `import { createJiti } from "jiti/static"`.

`harness-status.test.mjs` loads the extension through jiti deliberately, so that TS
loading is part of what is under test — but it was importing the **fork**. Left alone
it would have kept passing against a loader production no longer uses. It now imports
`jiti/static`, matching pi.

## 4. Real-process verification, not just mocks

The mock test (`npm test`, 16 assertions) proves the state machine. It cannot prove the
state machine is still *wired* to the real runner, real loader, and real event names —
which is precisely what a version bump threatens. So M2 adds
**`pi-ext/testing/capture-lifecycle.sh`**: it drives an actual `pi` process through a
full agent turn and asserts the resulting `.harness/status.json` transitions.

It needs no API key and no network. `pi-ext/testing/fake-openai-server.mjs` is a
~90-line OpenAI-compatible SSE server that answers two turns (a `bash` tool call, then
a short text reply) behind a deliberate delay so `running` is observable; pi is pointed
at it through a sandboxed `PI_CODING_AGENT_DIR`, so the user's own `~/.pi` is never
read or written.

Result against pi 0.73.1:

```
==> pi 0.73.1
PASS: models.json (architecture §5 shape) resolves lmstudio/omar-agent-v1
==> observed state sequence: [running idle]
PASS: pi exited cleanly
PASS: the tool actually ran
PASS: agent reached 'running'
PASS: agent settled to 'idle'
PASS: no running->idle flapping mid-turn
PASS: status.json carries the harvesting hook (sessionFile)
PASS: no temp files left behind (atomic write)
==> session JSONL: 7 records
```

**Run this on every pi bump** — that is the R3 discipline the architecture asks for,
now executable in one command instead of described in prose. `make doctor` also warns
when the installed pi differs from the pin.

## 5. Bonus: §5's `models.json` is no longer unverified

§5 noted that Spike 2 could not confirm the LM Studio provider block because the build
container blocked even localhost sockets. The capture harness loads **the exact JSON
printed in §5** and pi resolves `lmstudio/omar-agent-v1` from it, streams an
OpenAI-compatible response through it, and records `"provider":"lmstudio"` in the
session. The block is correct as written, including the dummy `apiKey` and the `compat`
flags. *Caveat:* this validates the shape pi expects, not LM Studio's own behavior —
`compat.supportsDeveloperRole` / `supportsReasoningEffort` still want one check against
a live LM Studio build.

## 6. Watch item for the next bump: the npm scope is being renamed

0.73.1's headline feature is self-update support for an **upcoming package rename**:

> `@mariozechner/pi-coding-agent` → `@earendil-works/pi-coding-agent`

The GitHub org has already moved (`badlogic/pi-mono` → `earendil-works/pi-mono`; the
changelog cites both). When the new package publishes, `script/setup-macos.sh`,
`script/doctor.sh`, `pi-ext/package.json`, and the `import type` in
`harness-status.ts` all need the new scope. Nothing breaks until then. This is now the
principal R3 residual, replacing the pin gap M2 closed.

---

## Verdict

R3's *pin-gap* half is retired: one pin (0.73.1), recorded in setup, doctor,
`package.json`, and the extension header, with both a mock suite and a real-process
capture pinned to it. R3's *drift* half is unchanged by nature — but it is now cheap
to re-check (`bash pi-ext/testing/capture-lifecycle.sh`) instead of expensive.
R9 is re-verified at the new version, and came out stronger: `custom` dialogs now
raise `waiting` too.
