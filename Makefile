# openOrchestra developer entry points.
#
#   make setup   — install/verify build + runtime tools (idempotent)
#   make run     — verify prerequisites, then build & run the GPUI app
#   make build   — verify prerequisites, then build the GPUI app
#   make app     — build openOrchestra.app (macOS bundle → dist/)
#   make dmg     — universal (arm64+x86_64) bundle + .dmg → dist/  (two compiles)
#   make dmg-notarized — ...and Developer ID signed + notarized by Apple
#   make release — notarized universal .dmg, published to GitHub Releases
#   make doctor  — report which build/runtime tools are present
#   make icon    — regenerate the app icon from script/gen-icon.py
#   make test    — headless workspace tests (harness-app excluded by design)

APP_DIR := crates/harness-app

.PHONY: setup run build check test app dmg dmg-notarized release icon doctor

setup:
	bash ./script/setup-macos.sh

run: setup
	cd $(APP_DIR) && cargo run

build: setup
	cd $(APP_DIR) && cargo build

check:
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
	bash ./script/package-mac.sh --universal --dmg --publish

icon:
	python3 script/gen-icon.py

doctor:
	bash ./script/doctor.sh
