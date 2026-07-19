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

sec "Agent — needed to run pi in worktree terminals (else the terminal is a plain shell)"
if have node; then ok "node       $(node --version 2>/dev/null)"; else warn "node       brew install node"; fi
if have pi; then
  ok "pi         $(command -v pi)"
else
  warn "pi         npm install -g @mariozechner/pi-coding-agent@0.70.6"
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

sec "Build — needed for make build / make app"
if have cargo; then ok "cargo      $(cargo --version 2>/dev/null | sed 's/cargo //')"; else bad "cargo      install via https://rustup.rs"; missing_required=1; fi
if have cmake; then ok "cmake      $(cmake --version 2>/dev/null | head -1 | sed 's/cmake version //')"; else warn "cmake      brew install cmake  (or: make setup)"; fi
if have protoc; then ok "protobuf   $(protoc --version 2>/dev/null)"; else warn "protobuf   brew install protobuf  (or: make setup)"; fi
if xcrun -sdk macosx metal --version >/dev/null 2>&1; then
  ok "metal      Metal toolchain active"
else
  warn "metal      Metal toolchain missing — run:  make setup"
fi

echo
if ((missing_required)); then
  printf '\033[1;31m✗ A required tool is missing.\033[0m Install it above, then re-run: make doctor\n'
  exit 1
else
  printf '\033[1;32m✓ All required tools present.\033[0m  Optional tools (!) only affect the agent / badges.\n'
fi
