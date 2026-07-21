# training — session-exhaust harvesting

**Status:** v1 · closes roadmap item **M7** (architecture §12, §6.4).
**Implementation:** `crates/training` (library + the `harvest` binary).

This is the step that closes the loop the whole project exists for. Every
worktree session is an append-only JSONL tree we control the reading of;
archived sessions, their final diffs, and a merged/abandoned outcome label are
the raw material for the next training round.

```
  registry (sessionFile + archived)   forge poller (PR state)   git (merge-base diff)
           \                                  |                        /
            `---------------> HarvestRequest <'------------------------'
                                    |
                             TrainingRecord   (stamped with the §4.4 schema version)
                                    |
                                JSONL out
```

## Running it

```sh
cargo run -p training --bin harvest -- --out training-data.jsonl
```

| Flag | Effect |
|---|---|
| `--state-dir DIR` | Harness state root (default `~/.openorchestra`) |
| `--out FILE` | Output JSONL (default `./training-data.jsonl`) |
| `--all` | Include active worktrees, not just archived ones |
| `--include-unlabeled` | Keep `open` / `unknown` outcomes (provenance, not supervision) |
| `--no-forge` | Skip `gh`; outcomes fall back to the archived flag alone |

By default it harvests only **archived** worktrees with a **decided** outcome:
archiving is the "this task is over" signal (§6.1 D6), and an undecided outcome
is not supervision.

## Three things this gets deliberately right

**1. The session file is a tree, not a log.** Records carry `id`/`parentId`, and
`/fork` branches the conversation. Reading the file in order would splice
abandoned branches into the transcript and train the model on paths the user
explicitly walked away from. The parser resolves the **active branch** by taking
the last record and walking `parentId` to the root — on an append-only tree that
is always the branch that was live when the session ended, and therefore the one
that produced the final diff.

**2. Outcome labels are never guessed.**

| PR state | Archived | Label |
|---|---|---|
| Merged | — | `merged` |
| Closed | — | `abandoned` |
| Open / Draft | — | `open` (undecided — not a label) |
| *(none)* | yes | `abandoned` |
| *(none)* | no | `unknown` |

A live worktree with no PR yet is genuinely undecided, and a `gh` failure is
treated as *no information* rather than as "no PR" — otherwise a transient
network error would relabel merged work as abandoned. Mislabeled outcomes are
worse than missing ones: they teach the model the wrong lesson.

**3. Records are schema-versioned.** Every row carries the `agent-schema`
version in force at harvest time plus the pi session-format version it was
parsed as, so a training set can never be silently mixed across a breaking
toolset change (§4.4, R5). An unrecognized session format is **refused**, not
guessed at — quietly mis-parsing sessions would poison the set.

## Record shape

```jsonc
{
  "schemaVersion": "1.0",           // agent-schema contract in force
  "sessionFormatVersion": 3,        // pi session JSONL version
  "outcome": "merged",
  "provenance": {
    "worktreeId": "app/nav", "repo": "app", "branch": "feat/nav",
    "taskSlug": "nav", "createdAt": "...", "agentKind": "pi",
    "prNumber": 128, "sessionId": "...", "provider": "lmstudio",
    "model": "omar-agent-v1"
  },
  "diff": { "base": "...", "filesChanged": 2, "adds": 38, "dels": 8,
            "paths": ["src/nav.rs"], "unified": "..." },
  "turns": [
    { "role": "user", "text": "..." },
    { "role": "assistant", "text": null,
      "toolCalls": [{ "id": "call_1", "name": "bash", "arguments": {...} }],
      "stopReason": "toolUse" },
    { "role": "toolResult", "toolCallId": "call_1", "toolName": "bash",
      "text": "...", "isError": false }
  ],
  "stats": { "turnCount": 4, "toolCallCount": 1, "hadToolErrors": false }
}
```

## Known limits

- **Diffs are absent for archived worktrees.** Archiving removes the directory,
  so there is nothing left to diff — which is exactly why archive keeps the
  branch (§6.1 D6). Reconstructing the diff from the retained branch against its
  merge base is the natural next step.
- **pi only.** Harvesting is per-adapter (§4.5): Claude Code exposes a
  `transcript_path` and could be added, while Codex / OpenCode / Kimi keep their
  own stores. The training loop stays pi-first by design (§4.4).
