# M1 — `gh` path confirmed against a real binary (retires R7)

**Status:** Done · July 21, 2026 · closes roadmap item **M1** (architecture §12) and the
"fixture-verified only" caveat on risk **R7** (§8).

**Binary under test:** `gh version 2.95.0 (2026-06-17)`, authenticated
(`gh auth status` → signed in, scopes `gist, read:org, repo, workflow`).
**Repos probed:** `cli/cli` (open / draft / merged / closed PRs, pass + fail +
skipping checks), `microsoft/vscode` (pending checks), `jwasham/coding-interview-university`
and `kamranahmedse/developer-roadmap` (PRs with no checks at all).

Everything `crates/forge/src/gh.rs` previously trusted from documentation is now
observed, **except one comment that was wrong** (exit codes — §4 below) and one
newly discovered limitation (fork-head PRs — §5).

---

## 1. `gh pr view --json number,state,isDraft,title -- <branch>`

Wire shape confirmed exactly as `GhPrView` models it — a **single object**, not an
array, keys emitted alphabetically:

```json
{"isDraft":false,"number":13894,"state":"OPEN","title":"Support nested skill discovery without skills directories"}
```

All four `map_pr_state` inputs observed live:

| Observed `state` / `isDraft` | Branch probed | → `PrState` |
|---|---|---|
| `"OPEN"`, `isDraft:false` | `tommaso-moro-skill-install-nested-discovery` (#13894) | `Open` |
| `"OPEN"`, `isDraft:true` | `babakks/cleanup-git-creds-on-logout` (#13450) | `Draft` |
| `"MERGED"`, `isDraft:false` | `bagtoad/search-repo-flag-hint` (#13922) | `Merged` |
| `"CLOSED"`, `isDraft:false` | `dependabot/github_actions/github/gh-aw-actions/setup-0.82.7` (#13871) | `Closed` |

**No PR for the branch:** exit status **1**, empty stdout, stderr exactly

```
no pull requests found for branch "no-such-branch-xyz-123"
```

→ `is_no_pr_error` matches on `"no pull requests found"`. Confirmed.

## 2. `gh pr checks --json name,state,bucket -- <branch>`

Wire shape confirmed as `Vec<GhCheck>`:

```json
[{"bucket":"pass","name":"CodeQL","state":"SUCCESS"},
 {"bucket":"pass","name":"integration-tests (windows-latest)","state":"SUCCESS"}]
```

Buckets observed in the wild, and the `state` values that accompany them:

| `bucket` | `state` seen | `map_check` → |
|---|---|---|
| `pass` | `SUCCESS` | `Pass` |
| `fail` | `FAILURE` | `Fail` |
| `skipping` | `SKIPPED` | `Skipped` |
| `pending` | `IN_PROGRESS` | `Running` |
| `cancel` | *not observed* — kept from gh's source; falls through to the raw-`state` arm harmlessly if the name ever changes | `Fail` |

## 3. PR exists but has no checks

Exit status **1**, **empty stdout**, stderr exactly

```
no checks reported on the 'master' branch
```

→ `is_no_checks_error` matches on `"no checks reported"`. Confirmed. Note the
branch name is quoted with **single** quotes here and **double** quotes in the
`pr view` message (§1) — both substring matches avoid the difference.

## 4. Exit codes — **the one thing the code got wrong**

`gh.rs` carried the comment:

> `gh pr checks` uses exit codes as signal (8 = pending, non-zero on failing checks)

The 8-means-pending convention is real, but it applies **only to non-JSON output**.
With `--json`, gh treats the request as a data query and exits **0** regardless of
check outcome. Measured on the same branch, back to back:

| Invocation | Checks state | Exit |
|---|---|---|
| `gh pr checks --json … <branch>` | 2 × `FAILURE` present | **0** |
| `gh pr checks <branch>` | same | **1** |
| `gh pr checks --json … <pr>` | `IN_PROGRESS` present | **0** |
| `gh pr checks <pr>` | same | **8** |

`gh help exit-codes` documents only 0 / 1 / 2 / 4 generically and warns that
per-command codes exist — 8 is one of those, undocumented in the top-level help.

**Consequence for the harness: none behaviourally.** `checks_for_branch` already
parses stdout first and only consults the status afterwards, so it was correct for
the wrong stated reason. The misleading comment is corrected in this commit.

## 5. New finding — `gh pr view <branch>` misses **fork-head PRs**

Branch lookup resolves PRs whose head branch lives in the *base* repository. For a
PR opened from a fork, the same command reports "no pull requests found" even
though the PR is open:

```
$ gh pr view --json number,state,isDraft,title -- docs/13898-item-edit-help   # PR #13927, head = a fork
no pull requests found for branch "docs/13898-item-edit-help"    (exit 1)
```

This is gh behaviour, not a parsing bug, and the harness degrades correctly (the
branch simply shows no PR badge). It matters because openOrchestra worktree
branches are pushed to whatever remote the user configured: **same-repo branches
get badges, fork-pushed branches silently will not.** If fork workflows become a
target, the fix is to query by head with owner qualification
(`gh pr list --head <branch> --json …`, which does match across forks) rather than
`gh pr view <branch>`. Filed as a follow-up, not a v1 blocker.

---

## Verdict

R7's parsing half is **retired**: field shapes, PR states, bucket values, both
"absent" stderr strings, and the exit-code semantics are all now observed rather
than assumed. R7's remaining half (rate limits / graceful degradation when `gh`
is absent) is unchanged and already mitigated by the poller's backoff + jitter +
`available()` probe.
