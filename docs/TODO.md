# TODO

Ideas and follow-ups that aren't scheduled yet. Newest first.

## Give each task its own simulator or emulator

**Added 2026-11-22.** The Device tab
(`docs/superpowers/specs/2026-11-22-device-tab-ios-design.md`) lists every
booted simulator and emulator on the Mac with a picker, because it can't
tell which task is using which device: iOS simulators run under
`launchd_sim`, not under the agent that booted them.

Goal: openOrchestra assigns a device to each task (creates or picks a
simulator/AVD, and tells the task's agents to use it, e.g. through an
environment variable or the agent's instructions), so the Device tab can
show "this task's device" exactly, and two tasks never fight over one
simulator.

What it takes: a per-task device record, a way to hand it to every agent
kind openOrchestra launches, and cleanup when a task is archived.

## Install more editor languages from Zed extensions

**Added 2026-11-18.** The editor highlights every language zed has built in
(`crates/harness-app/src/curated_languages.rs`). Everything else zed
supports (Ruby, PHP, Swift, Java, Kotlin, Elixir, TOML, HTML, SQL,
Dockerfile, …) comes from zed's extension registry as WebAssembly grammars.

Goal: a Settings → Languages screen that lists the built-in languages and
lets the user install more from zed's extensions, without rebuilding the app.

What it takes:

- Bring in zed's `extension_host` (and probably parts of `extensions_ui`):
  extension download, the installed-extensions store, loading their wasm
  grammars and queries into the `LanguageRegistry`. The WebAssembly runtime
  (`wasmtime`) is already linked; `extension_host` is not.
- An HTTP client wired into the app state, and a decision on the registry
  endpoint (zed.dev's API).
- Product: this is the first feature that contacts a cloud service. It must be
  opt-in, only on the user's action, and say so in the UI; the README's "no
  cloud" line needs a caveat.
- Risk: the extension API moves with zed releases, so every zed rev bump can
  break it. Budget for that in each bump.
- Built-in languages could get on/off toggles in the same screen (they are
  compiled in, so off only stops registering them).
