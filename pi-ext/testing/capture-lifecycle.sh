#!/usr/bin/env bash
# Drive a REAL `pi` process through a full agent turn and assert that
# harness-status.ts writes the expected .harness/status.json transitions.
#
# This is the R3/R9 re-verification step: run it on every pi version bump
# (architecture §8 R3) before moving the pin. It complements the mock test
# (`npm test`) — the mock proves the state machine, this proves the state
# machine is still wired to the *real* extension runner, the real jiti loader,
# and the real event names.
#
# Needs no API key and no network: pi is pointed at the local stub server in
# fake-openai-server.mjs via a sandboxed PI_CODING_AGENT_DIR, so the user's own
# ~/.pi config is never read or written.
#
# Usage:  bash pi-ext/testing/capture-lifecycle.sh
# Output: PASS/FAIL per assertion; writes the captured session JSONL path.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ext="$here/../harness-status.ts"
work="$(mktemp -d)"
trap 'rm -rf "$work"; [[ -n "${server_pid:-}" ]] && kill "$server_pid" 2>/dev/null' EXIT

command -v pi   >/dev/null || { echo "pi not on PATH — npm i -g @mariozechner/pi-coding-agent"; exit 1; }
command -v node >/dev/null || { echo "node not on PATH"; exit 1; }

mkdir -p "$work/agentdir" "$work/project" "$work/sessions"
cp "$here/models.json" "$work/agentdir/models.json"

echo "==> pi $(pi --version 2>&1 | tail -1)"
node "$here/fake-openai-server.mjs" > "$work/server.log" 2>&1 &
server_pid=$!
sleep 1
curl -sf http://127.0.0.1:1234/v1/models >/dev/null || { echo "FAIL: stub server did not start"; cat "$work/server.log"; exit 1; }
echo "==> stub server up"

# Verify pi actually parses the §5 models.json shape before relying on it.
if PI_CODING_AGENT_DIR="$work/agentdir" pi --list-models 2>&1 | grep -q 'omar-agent-v1'; then
  echo "PASS: models.json (architecture §5 shape) resolves lmstudio/omar-agent-v1"
else
  echo "FAIL: pi did not resolve the model from models.json"; exit 1
fi

cd "$work/project"
# Sample status.json fast enough to catch the running->idle transition.
( for _ in $(seq 1 300); do
    [[ -f .harness/status.json ]] && node -e 'try{console.log(JSON.parse(require("fs").readFileSync(".harness/status.json","utf8")).state)}catch(e){}'
    sleep 0.05
  done ) > "$work/states.raw" 2>/dev/null &
sampler_pid=$!

PI_CODING_AGENT_DIR="$work/agentdir" pi -p "run the probe" -e "$ext" \
  --model lmstudio/omar-agent-v1 --session-dir "$work/sessions" > "$work/pi.log" 2>&1
pi_status=$?
sleep 2
kill "$sampler_pid" 2>/dev/null
wait "$sampler_pid" 2>/dev/null   # reap quietly; the sampler is expected to be killed

# Collapse consecutive duplicates into the observed transition sequence.
observed="$(awk '$0!=prev{print; prev=$0}' "$work/states.raw" | tr '\n' ' ' | sed 's/ *$//')"
echo "==> observed state sequence: [$observed]"

fails=0
check() { if [[ "$2" == "true" ]]; then echo "PASS: $1"; else echo "FAIL: $1"; fails=$((fails+1)); fi; }

check "pi exited cleanly" "$([[ $pi_status -eq 0 ]] && echo true || echo false)"
check "the tool actually ran" "$(grep -q 'Probe complete' "$work/pi.log" && echo true || echo false)"
check "agent reached 'running'" "$([[ "$observed" == *running* ]] && echo true || echo false)"
check "agent settled to 'idle'" "$([[ "$observed" == *idle ]] && echo true || echo false)"
check "no running->idle flapping mid-turn" \
  "$([[ "$(echo "$observed" | tr ' ' '\n' | grep -c running)" -le 1 ]] && echo true || echo false)"

session_file="$(node -e 'try{console.log(JSON.parse(require("fs").readFileSync(".harness/status.json","utf8")).sessionFile??"")}catch(e){}')"
check "status.json carries the harvesting hook (sessionFile)" \
  "$([[ -n "$session_file" && -f "$session_file" ]] && echo true || echo false)"
check "no temp files left behind (atomic write)" \
  "$([[ "$(find .harness -type f ! -name status.json | wc -l | tr -d ' ')" == "0" ]] && echo true || echo false)"

if [[ -n "$session_file" && -f "$session_file" ]]; then
  echo "==> session JSONL: $(wc -l < "$session_file" | tr -d ' ') records"
fi

echo
if (( fails )); then echo "$fails assertion(s) FAILED"; exit 1; fi
echo "ALL LIFECYCLE ASSERTIONS PASSED"
