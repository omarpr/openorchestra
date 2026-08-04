#!/usr/bin/env bash
# openOrchestra — report the tools the app uses at build & run time.
# This only REPORTS (installs nothing). Run: make doctor
#   - `make setup` installs the build + runtime tools.
#   - the app itself degrades gracefully when the optional tools are absent.

ok()   { printf '  \033[1;32m✓\033[0m %s\n' "$*"; }
bad()  { printf '  \033[1;31m✗\033[0m %s\n' "$*"; }
warn() { printf '  \033[1;33m!\033[0m %s\n' "$*"; }
sec()  { printf '\n\033[1m%s\033[0m\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }

missing_required=0

sec "Required — the app cannot run without this"
if have git; then
  ok "git        $(git --version 2>/dev/null | sed 's/git version //')"
else
  bad "git        install the Xcode Command Line Tools:  xcode-select --install"
  missing_required=1
fi

# tmux was missing from this report entirely while the app called `tmux -V` at
# startup and hung every persistent agent session off it — the same blind spot
# the in-app requirements screen now covers. Note `-V`: tmux has no --version.
if have tmux; then
  ok "tmux       $(tmux -V 2>/dev/null | sed 's/tmux //')"
else
  bad "tmux       agents die with their tab and never reattach:  brew install tmux"
  missing_required=1
fi

sec "Agent — needed to run pi in worktree terminals (else the terminal is a plain shell)"
if have node; then ok "node       $(node --version 2>/dev/null)"; else warn "node       brew install node"; fi
# The supported pin (architecture §8 R3, docs/m2-pi-pin.md). The extension's
# state machine is re-verified against exactly this version; a drifting pi is
# the single most likely cause of a dead agent-status pill.
PI_PIN=0.73.1
if have pi; then
  pi_version="$(pi --version 2>&1 | tail -1 | tr -d '[:space:]')"
  if [[ "$pi_version" == "$PI_PIN" ]]; then
    ok "pi         $pi_version  ($(command -v pi))"
  else
    warn "pi         $pi_version installed, but $PI_PIN is the verified pin — re-run: bash pi-ext/testing/capture-lifecycle.sh"
  fi
else
  warn "pi         npm install -g @mariozechner/pi-coding-agent@$PI_PIN"
fi

sec "Forge — PR + CI badges (optional; badges hide when absent)"
if have gh; then
  ok "gh         $(gh --version 2>/dev/null | head -1)"
  if gh auth status >/dev/null 2>&1; then
    ok "gh auth    signed in"
  else
    warn "gh auth    not authenticated — run:  gh auth login"
  fi
else
  warn "gh         brew install gh   (then: gh auth login)"
fi

sec "Editor — rust-analyzer powers the LSP indicator in ▤ files (optional)"
if have rust-analyzer; then
  # ~/.cargo/bin/rust-analyzer is a rustup SHIM: it exists on $PATH even when
  # the component is not installed, and then exits 1 at runtime. Zed rejects
  # such a binary and falls back to downloading one, which openOrchestra blocks
  # — so "present on PATH" is not the question worth asking (N2 §4).
  if rust-analyzer --version >/dev/null 2>&1; then
    ok "rust-analyzer $(rust-analyzer --version 2>/dev/null | head -1)"
  else
    warn "rust-analyzer on \$PATH but not runnable (rustup shim without the component) — run:  rustup component add rust-analyzer"
  fi
else
  warn "rust-analyzer rustup component add rust-analyzer"
fi

sec "Build — needed for make build / make app"
if have cargo; then ok "cargo      $(cargo --version 2>/dev/null | sed 's/cargo //')"; else bad "cargo      install via https://rustup.rs"; missing_required=1; fi
if have cmake; then ok "cmake      $(cmake --version 2>/dev/null | head -1 | sed 's/cmake version //')"; else warn "cmake      brew install cmake  (or: make setup)"; fi
if have protoc; then ok "protobuf   $(protoc --version 2>/dev/null)"; else warn "protobuf   brew install protobuf  (or: make setup)"; fi
if xcrun -sdk macosx metal --version >/dev/null 2>&1; then
  ok "metal      Metal toolchain active"
else
  warn "metal      Metal toolchain missing — run:  make setup"
fi

sec "Release signing — needed for make dmg-notarized / make release (optional)"
# Absence here means "cannot cut a release", never "the app is broken": a
# contributor without an Apple account still gets a working make app / make dmg.
OO_TEAM_ID="${OO_TEAM_ID:-HLWNR3FFHW}"
OO_NOTARY_PROFILE="${OO_NOTARY_PROFILE:-openorchestra}"
# Match on the team ID, not the certificate name — the name is an identity that
# does not belong in terminal output, and the team is what actually has to be
# right. A Developer ID cert from another team is the failure being screened for.
if security find-identity -v -p codesigning 2>/dev/null \
     | grep -q "Developer ID Application.*($OO_TEAM_ID)"; then
  ok "Developer ID  certificate present for team $OO_TEAM_ID"
else
  warn "Developer ID  no certificate for team $OO_TEAM_ID — create one at
               https://developer.apple.com/account/resources/certificates
               (set the team switcher first; a cert from another team is valid
               and would publish the wrong entity in every binary)"
fi
if xcrun notarytool history --keychain-profile "$OO_NOTARY_PROFILE" >/dev/null 2>&1; then
  ok "notarytool    profile '$OO_NOTARY_PROFILE' authenticates"
else
  warn "notarytool    profile '$OO_NOTARY_PROFILE' missing — run:
               xcrun notarytool store-credentials $OO_NOTARY_PROFILE \\
                 --key <AuthKey.p8> --key-id <KEYID> --issuer <ISSUERID>"
fi

echo
if ((missing_required)); then
  printf '\033[1;31m✗ A required tool is missing.\033[0m Install it above, then re-run: make doctor\n'
  exit 1
else
  printf '\033[1;32m✓ All required tools present.\033[0m  Optional tools (!) only affect the agent / badges.\n'
fi
