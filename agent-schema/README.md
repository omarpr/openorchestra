# agent-schema — the tool contract

**Status:** v1.0 · closes roadmap item **M6** (architecture §12, §4.4).

`v1/tools.json` is the **single source of truth** for the toolset the reference
runtime (pi) exposes and the toolset the model is trained against. It is the
contract that makes "our model, our loop" checkable instead of aspirational.

## Why this exists

The model is trained on a specific set of tools with specific argument shapes.
If the *runtime* offers a different set — pi adds a built-in, a user extension
registers one, a parameter gets renamed — the model sees tools it never trained
on. Nothing errors. The model just quietly gets worse. That is R5's failure
mode, and it is invisible without a contract to check against.

## Who reads it

Three consumers, and they must never drift:

| Consumer | What it does with the contract |
|---|---|
| `pi-ext/harness-tools.ts` | Calls `setActiveTools` with exactly these tool names on every session start, and reports drift between the contract and what pi actually offers |
| `crates/agent-schema` | Embeds it (`include_str!`) and checks a checkpoint's declared `schemaVersion` before the harness hands it work |
| `training/` (M7) | Stamps every emitted record with the version, so a training set can be traced to the toolset it was generated under |

The harness materializes the contract next to the extensions in
`~/.openorchestra/pi-ext/` and points pi at it with `HARNESS_AGENT_SCHEMA`, so a
packaged `openOrchestra.app` — which has no checkout to walk up from — carries
the contract with it (`crates/agent-runtime/src/pi.rs`).

## Versioning

`schemaVersion` is `MAJOR.MINOR`.

- **MINOR** — additive and backward compatible: a new tool, a new *optional*
  parameter. A checkpoint trained on `1.0` still runs on `1.1`; it simply never
  emits what it has not seen.
- **MAJOR** — breaking: a tool removed or renamed, a parameter's type or meaning
  changed, a *required* parameter added. A checkpoint trained on a different
  major will call tools that no longer exist, or call them wrongly, so the
  harness refuses (`Compatibility::Incompatible`).

An unparseable or absent declaration is refused too — failing closed, since an
undeclared schema is exactly what the contract exists to prevent.

## v1.0

Pinned to pi's seven built-ins, dumped verbatim from
`@mariozechner/pi-coding-agent@0.73.1` rather than hand-transcribed:

`read` · `ls` · `grep` · `find` · `bash` · `edit` · `write`

Harness-specific tools (worktree-aware ops, "request review" — §4.4) are a
future MINOR bump: they get `registerTool`ed in `harness-tools.ts` and added
here in the same commit.

## Changing the contract

1. Edit `v1/tools.json` (or add `v2/` for a breaking change).
2. Bump `schemaVersion` per the rules above.
3. `cargo test -p agent-schema` — the tool list is asserted, so an accidental
   addition or removal fails the build rather than silently invalidating every
   checkpoint.
4. `cd pi-ext && npm test` — asserts the extension pins and reports drift.
5. Re-run `bash pi-ext/testing/capture-lifecycle.sh` to confirm real pi still
   loads it.
