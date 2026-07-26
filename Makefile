# openOrchestra developer entry points.
#
#   make setup   — install/verify build + runtime tools (idempotent)
#   make run     — verify prerequisites, then build & run the GPUI app
#   make build   — verify prerequisites, then build the GPUI app
#   make app     — build openOrchestra.app (macOS bundle → dist/)
#   make doctor  — report which build/runtime tools are present
#   make icon    — regenerate the app icon from script/gen-icon.py
#   make test    — headless workspace tests (harness-app excluded by design)

APP_DIR := crates/harness-app

.PHONY: setup run build check test app icon doctor

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

icon:
	python3 script/gen-icon.py

doctor:
	bash ./script/doctor.sh
