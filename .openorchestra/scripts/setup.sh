#!/usr/bin/env bash
# openOrchestra setup hook — runs with cwd = the member worktree.
# Env (set by the harness):
#   HARNESS_REPO_ROOT      main checkout of this project
#   HARNESS_WORKTREE       member worktree path (also cwd)
#   HARNESS_BRANCH         member branch
#   HARNESS_TASK_SLUG      task slug
#   HARNESS_TASK_ID        task id (multi-member tasks)
#   HARNESS_HOME_WORKTREE  home member path (multi-member tasks)
#   HARNESS_TASK_CONTEXT   path to agent-context.md (multi-member tasks)
set -euo pipefail

# Add your setup steps below.
mkdir -p "$HARNESS_WORKTREE/crates/harness-app/target"
cp -Rc "$HARNESS_REPO_ROOT/crates/harness-app/target/release" \
   "$HARNESS_WORKTREE/crates/harness-app/target/"
