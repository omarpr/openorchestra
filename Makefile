# omar-harness developer entry points.
#
#   make setup   — install/verify macOS build prerequisites (idempotent)
#   make run     — verify prerequisites, then build & run the GPUI app
#   make build   — verify prerequisites, then build the GPUI app
#   make test    — headless workspace tests (harness-app excluded by design)

APP_DIR := crates/harness-app

.PHONY: setup run build check test

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
