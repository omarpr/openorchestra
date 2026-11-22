# openOrchestra developer entry points.
#
#   make setup   — install/verify build + runtime tools (idempotent)
#   make run     — verify prerequisites, build the web client, then build & run the GPUI app (as a
#                  dev .app bundle, so native notifications work)
#   make build   — verify prerequisites, build the web client, then build the GPUI app
#   (build, run and check depend on `web`; app/dmg/release get it via package-mac.sh)
#   make app     — build openOrchestra.app (macOS bundle → dist/)
#   make dmg     — universal (arm64+x86_64) bundle + .dmg → dist/  (two compiles)
#   make dmg-notarized — ...and Developer ID signed + notarized by Apple
#   make release — asks for a version bump, then a notarized universal .dmg published to GitHub
#                  Releases and the Homebrew tap
#   make doctor  — report which build/runtime tools are present
#   make icon    — regenerate the app icon from script/gen-icon.py
#   make dmg-background — regenerate the installer DMG's background art
#   make web     — build the oo-remote PWA (web/remote/dist, embedded by crates/remote)
#   make test    — headless workspace tests (harness-app excluded by design)
#
#   Windows: the same targets (minus the macOS packaging) plus dist, install
#   and uninstall, from script/windows/make.ps1; `.\make help` lists them.

APP_DIR := crates/harness-app

.PHONY: setup run build check test app dmg dmg-notarized release icon dmg-background doctor web dist install uninstall help

# Windows: every target is script/windows/make.ps1's (WinLibs ships make as
# mingw32-make; `.\make <target>` runs the same script without make). Extra
# app args for `run`: make run ARGS="C:\path\to\repo".
ifeq ($(OS),Windows_NT)

WIN_MAKE := powershell -NoProfile -ExecutionPolicy Bypass -File script/windows/make.ps1

setup run build check test app dmg dmg-notarized release icon dmg-background doctor web dist install uninstall help:
	$(WIN_MAKE) $@ $(ARGS)

else

setup:
	bash ./script/setup-macos.sh

run: setup web
	bash ./script/run-dev.sh

build: setup web
	cd $(APP_DIR) && cargo build
	bash ./script/build-voice-helper.sh "$(APP_DIR)/target/debug/oo-voice"

check: web
	cd $(APP_DIR) && cargo check

test:
	cargo test

app: setup
	bash ./script/package-mac.sh

dmg: setup
	bash ./script/package-mac.sh --universal --dmg

# The full release pipeline without the outward-facing half, so a notarized DMG
# can be produced and tested before one is ever published.
dmg-notarized: setup
	bash ./script/package-mac.sh --universal --dmg --notarize

release: setup
	bash ./script/release-version.sh
	bash ./script/package-mac.sh --universal --dmg --publish

icon:
	python3 script/gen-icon.py

# The installer DMG's background (arrow + hint). Rerun after changing the layout.
dmg-background:
	python3 script/gen-dmg-background.py

doctor:
	bash ./script/doctor.sh

web:
	pnpm -C web/remote install --frozen-lockfile
	pnpm -C web/remote build

endif
